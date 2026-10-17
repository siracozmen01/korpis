// SPDX-License-Identifier: AGPL-3.0-or-later

// One workload's status, live (WatchWorkload of api.proto): the stream starts with the current
// state and sends each change. It is one call however long it stays open, which is what lets a
// share link with a use cap open a workload's screen for one use (§6.1 of docs/08-identity.md).

import { Code, ConnectError } from "@connectrpc/connect";
import { useEffect, useState } from "react";
import type { Clients } from "../clients";
import type { WorkloadStatus } from "../gen/korpis/v1/api_pb";
import { type Loaded, notTheirs } from "./load";

/** The status of workload `id`, kept current while the screen is open. */
export function useWatch(clients: Clients, id: string): Loaded<WorkloadStatus> {
  const [watched, setWatched] = useState<{
    readonly of: string;
    readonly loaded: Loaded<WorkloadStatus>;
  }>();
  useEffect(() => {
    const stop = new AbortController();
    const settle = (loaded: Loaded<WorkloadStatus>) => {
      if (!stop.signal.aborted) setWatched({ of: id, loaded });
    };
    void (async () => {
      try {
        const stream = clients.workloads.watchWorkload({ workload: id }, { signal: stop.signal });
        for await (const change of stream) {
          if (change.status !== undefined) settle({ state: "done", value: change.status });
        }
        // A stream that ends leaves a status that is no longer live, which is not shown as if it
        // were (§2 of docs/11-surface-web.md).
        throw new ConnectError("the live view ended", Code.Unavailable);
      } catch (error) {
        settle(notTheirs(error) ? { state: "hidden" } : { state: "failed", error });
      }
    })();
    return () => {
      stop.abort();
    };
  }, [clients, id]);
  return watched?.of === id ? watched.loaded : { state: "loading" };
}
