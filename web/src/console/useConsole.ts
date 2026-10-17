// SPDX-License-Identifier: AGPL-3.0-or-later

// A workload's console, live (§8 of docs/11-surface-web.md, §6 of docs/14-streams.md): opened
// with the last records the node holds and followed; reopened from the offset it was told to
// read next, so a dropped session misses nothing and repeats nothing; older records read on
// request, from the node while it holds them and from the offloaded segments beyond.

import { useEffect, useReducer, useRef } from "react";
import type { Clients } from "../clients";
import type { ConsoleChunk } from "../gen/korpis/v1/console_pb";
import { type Relay, chunkOf, open, reading } from "./relay";
import { EMPTY, type View, append, prepend } from "./view";

/** How many records the console starts with, and reads at a time when going back. */
export const BACK = 200;

const READ = "workload.console.read";

interface State {
  readonly view: View;
  readonly live: boolean;
  readonly ended: string | undefined;
  readonly attempt: number;
  readonly failures: number;
}

type Event =
  | { readonly kind: "live" }
  | { readonly kind: "chunk"; readonly chunk: ConsoleChunk }
  | { readonly kind: "older"; readonly chunk: ConsoleChunk }
  | { readonly kind: "ended"; readonly why: string }
  | { readonly kind: "reopen" };

function reduce(s: State, e: Event): State {
  switch (e.kind) {
    case "live":
      return { ...s, live: true, ended: undefined, failures: 0 };
    case "chunk":
      return { ...s, view: append(s.view, e.chunk) };
    case "older":
      return { ...s, view: prepend(s.view, e.chunk) };
    case "ended":
      return { ...s, live: false, ended: e.why, failures: s.failures + 1 };
    case "reopen":
      return { ...s, attempt: s.attempt + 1, ended: undefined };
  }
}

/** What the console needs. */
export interface Reaching {
  readonly clients: Clients;
  readonly relay: () => Promise<Relay>;
  readonly workload: string;
  /** Reopen by itself after a failure; a share link asks first, since each opening is a use. */
  readonly automatic: boolean;
}

/** The console of `reaching.workload`. */
export function useConsole(reaching: Reaching) {
  const { clients, relay, workload, automatic } = reaching;
  const [state, dispatch] = useReducer(reduce, {
    view: EMPTY,
    live: false,
    ended: undefined,
    attempt: 0,
    failures: 0,
  });
  const next = useRef<bigint | undefined>(undefined);
  useEffect(() => {
    next.current = state.view.next;
  });
  const { attempt, ended, failures } = state;
  useEffect(() => {
    const stop = new AbortController();
    void (async () => {
      try {
        const from = next.current;
        const params = reading(
          from === undefined
            ? { last: BigInt(BACK), follow: true }
            : { fromOffset: from, follow: true },
        );
        const opening = {
          clients,
          relay: await relay(),
          workload,
          action: READ,
          params,
          signal: stop.signal,
        };
        for await (const heard of open(opening)) {
          if (heard.kind === "opened") dispatch({ kind: "live" });
          else if (heard.kind === "data") dispatch({ kind: "chunk", chunk: chunkOf(heard.data) });
          else
            throw new Error(
              heard.message === "" ? `the session ended (${heard.code})` : heard.message,
            );
        }
        throw new Error("the session ended");
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

  /** Reads the records before the oldest shown: from the offloaded segments the control plane
   * reads back, and from the node for what has not been offloaded yet. */
  const earlier = async () => {
    const first = state.view.first;
    if (first === undefined || first === 0n) return;
    const from = first > BigInt(BACK) ? first - BigInt(BACK) : 0n;
    const limit = first - from;
    let found = 0;
    for await (const chunk of clients.streams.readOffloaded({
      workload,
      fromOffset: from,
      limit,
    })) {
      found += chunk.records.length;
      dispatch({ kind: "older", chunk });
    }
    if (found > 0) return;
    const params = reading({ fromOffset: from, limit, follow: false });
    const stop = new AbortController();
    const opening = {
      clients,
      relay: await relay(),
      workload,
      action: READ,
      params,
      signal: stop.signal,
    };
    for await (const heard of open(opening)) {
      if (heard.kind === "data") dispatch({ kind: "older", chunk: chunkOf(heard.data) });
      if (heard.kind === "end") break;
    }
  };

  return {
    view: state.view,
    live: state.live,
    ended,
    reopen: () => {
      dispatch({ kind: "reopen" });
    },
    earlier,
  };
}
