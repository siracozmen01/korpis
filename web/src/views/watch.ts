// SPDX-License-Identifier: AGPL-3.0-or-later

// One workload's status, live (WatchWorkload of api.proto): the stream starts with the current
// state and sends each change. It is one call however long it stays open, which is what lets a
// share link with a use cap open a workload's screen for one use (§6.1 of docs/08-identity.md).
// When it ends, the last status stays on screen as stale, with when it was last heard (§2 of
// docs/11-surface-web.md). A signed-in screen opens it again by itself, waiting longer after
// each failure; a share link's screen asks first, because each opening spends a use.

import { useEffect, useReducer } from "react";
import type { Clients } from "../clients";
import type { WorkloadStatus } from "../gen/korpis/v1/api_pb";
import type { Heard } from "../workload/freshness";
import { type Timeline, advance } from "../workload/timeline";
import { notTheirs } from "./load";
import { t } from "../i18n/intl";

/** What the screen knows of the workload, and how it came to know it. */
export interface Live {
  readonly status: WorkloadStatus | undefined;
  readonly timeline: Timeline | undefined;
  readonly heard: Heard;
  /** Why the live view ended, while it is ended. */
  readonly ended: unknown;
  /** The workload is not the caller's to see. */
  readonly hidden: boolean;
  readonly reopen: () => void;
}

interface State {
  readonly attempt: number;
  readonly failures: number;
  readonly status: WorkloadStatus | undefined;
  readonly timeline: Timeline | undefined;
  readonly heard: Heard;
  readonly ended: unknown;
  readonly hidden: boolean;
}

type Event =
  | { readonly kind: "heard"; readonly status: WorkloadStatus; readonly at: number }
  | { readonly kind: "ended"; readonly why: unknown }
  | { readonly kind: "reopen" };

function reduce(s: State, e: Event): State {
  switch (e.kind) {
    case "heard":
      return {
        ...s,
        failures: 0,
        status: e.status,
        timeline: advance(s.timeline, e.status),
        heard: { live: true, at: e.at },
        ended: undefined,
      };
    case "ended":
      return {
        ...s,
        failures: s.failures + 1,
        heard: { ...s.heard, live: false },
        ended: e.why,
        hidden: notTheirs(e.why),
      };
    case "reopen":
      return { ...s, attempt: s.attempt + 1, ended: undefined };
  }
}

const START: State = {
  attempt: 0,
  failures: 0,
  status: undefined,
  timeline: undefined,
  heard: { live: false, at: 0 },
  ended: undefined,
  hidden: false,
};

/** The longest wait before opening it again by itself. */
const LONGEST_MS = 30_000;

/** Workload `id`, live; reopened by itself after a failure when `automatic`. */
export function useLive(clients: Clients, id: string, automatic: boolean): Live {
  const [state, dispatch] = useReducer(reduce, START);
  const { attempt, failures, ended, hidden } = state;
  useEffect(() => {
    const stop = new AbortController();
    void (async () => {
      try {
        const stream = clients.workloads.watchWorkload({ workload: id }, { signal: stop.signal });
        for await (const change of stream) {
          const status = change.status;
          if (status !== undefined) dispatch({ kind: "heard", status, at: Date.now() });
        }
        throw new Error(t({ defaultMessage: "the live view ended", id: "A76GYv5R" }));
      } catch (why) {
        if (!stop.signal.aborted) dispatch({ kind: "ended", why });
      }
    })();
    return () => {
      stop.abort();
    };
  }, [clients, id, attempt]);
  const waiting = ended !== undefined && automatic && !hidden;
  useEffect(() => {
    if (!waiting) return;
    const wait = Math.min(LONGEST_MS, 1000 * 2 ** Math.max(0, failures - 1));
    const timer = setTimeout(() => {
      dispatch({ kind: "reopen" });
    }, wait);
    return () => {
      clearTimeout(timer);
    };
  }, [waiting, failures]);
  return {
    ...state,
    reopen: () => {
      dispatch({ kind: "reopen" });
    },
  };
}
