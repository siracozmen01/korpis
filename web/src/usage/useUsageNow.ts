// SPDX-License-Identifier: AGPL-3.0-or-later

// What a workload uses now, live (§2 of docs/15-observability.md): a `metering.read` session at
// its node through the relay, which is sent the meter's reading of the workload each time the
// meter takes one, every ten seconds. Reopened by itself after a failure, waiting longer each
// time, unless reopening is asked for first, as a share link's is.

import { fromBinary } from "@bufbuild/protobuf";
import { useEffect, useReducer } from "react";
import type { Clients } from "../clients";
import { type Relay, open } from "../console/relay";
import { type UsageNow, UsageNowSchema } from "../gen/korpis/v1/metering_pb";
import { t } from "../i18n/intl";

/** The action a session for what a workload uses now is opened under. */
export const METERING = "metering.read";

interface State {
  readonly now: UsageNow | undefined;
  readonly live: boolean;
  readonly ended: string | undefined;
  readonly attempt: number;
  readonly failures: number;
}

type Event =
  | { readonly kind: "live" }
  | { readonly kind: "read"; readonly now: UsageNow }
  | { readonly kind: "ended"; readonly why: string }
  | { readonly kind: "reopen" };

function reduce(s: State, e: Event): State {
  switch (e.kind) {
    case "live":
      return { ...s, live: true, ended: undefined };
    case "read":
      return { ...s, now: e.now, failures: 0 };
    case "ended":
      return { ...s, live: false, ended: e.why, failures: s.failures + 1 };
    case "reopen":
      return { ...s, attempt: s.attempt + 1, ended: undefined };
  }
}

/** What the session needs. */
export interface Reaching {
  readonly clients: Clients;
  readonly relay: () => Promise<Relay>;
  readonly workload: string;
  /** Reopen by itself after a failure; a share link asks first, since each opening is a use. */
  readonly automatic: boolean;
}

/** The latest reading of `reaching.workload` its node sent, and whether the session is open. */
export function useUsageNow(reaching: Reaching) {
  const { clients, relay, workload, automatic } = reaching;
  const [state, dispatch] = useReducer(reduce, {
    now: undefined,
    live: false,
    ended: undefined,
    attempt: 0,
    failures: 0,
  });
  const { attempt, ended, failures } = state;
  useEffect(() => {
    const stop = new AbortController();
    void (async () => {
      try {
        const opening = {
          clients,
          relay: await relay(),
          workload,
          action: METERING,
          params: new Uint8Array(),
          signal: stop.signal,
        };
        for await (const heard of open(opening)) {
          if (heard.kind === "opened") dispatch({ kind: "live" });
          else if (heard.kind === "data")
            dispatch({ kind: "read", now: fromBinary(UsageNowSchema, heard.data) });
          else
            throw new Error(
              heard.message === "" ? `the session ended (${heard.code})` : heard.message,
            );
        }
        throw new Error(t({ defaultMessage: "the session ended", id: "ba8ytnij" }));
      } catch (e) {
        if (!stop.signal.aborted)
          dispatch({ kind: "ended", why: e instanceof Error ? e.message : String(e) });
      }
    })();
    return () => {
      stop.abort();
    };
  }, [clients, relay, workload, attempt]);
  const waiting = ended !== undefined && automatic;
  useEffect(() => {
    if (!waiting) return;
    const timer = setTimeout(
      () => {
        dispatch({ kind: "reopen" });
      },
      Math.min(30_000, 1000 * 2 ** Math.max(0, failures - 1)),
    );
    return () => {
      clearTimeout(timer);
    };
  }, [waiting, failures]);
  return {
    now: state.now,
    live: state.live,
    ended,
    reopen: () => {
      dispatch({ kind: "reopen" });
    },
  };
}
