// SPDX-License-Identifier: AGPL-3.0-or-later

// A workload's screen (§2 and §3 of docs/11-surface-web.md), live over its watch: what it is
// converging to and how far it has come, the stage it is at when it has not converged, and what
// its node observed, each value known, stale, or unknown.

import { Button } from "react-aria-components";
import { useReach } from "../clients";
import { diagnosis } from "../workload/diagnosis";
import { age, freshness } from "../workload/freshness";
import { Observed } from "../workload/Observed";
import { TimelineView } from "../workload/TimelineView";
import { useNow } from "../workload/useNow";
import { PowerState } from "../gen/korpis/v1/workload_pb";
import { CONVERGENCE, DECLARED } from "../workload/words";
import { refusal } from "./refusal";
import { useLive } from "./watch";

/** The workload `id`. */
export function WorkloadPage({ id }: { readonly id: string }) {
  const { clients, metered } = useReach();
  const live = useLive(clients, id, !metered);
  const now = useNow();
  const { status, ended } = live;
  if (live.hidden) return null;
  if (status === undefined) {
    return ended === undefined ? (
      <p role="status">Reading the workload…</p>
    ) : (
      <main>
        <p role="alert">{refusal(ended)}</p>
        {metered && <Reopen reopen={live.reopen} />}
      </main>
    );
  }
  const said = diagnosis(status, now);
  return (
    <main>
      <h1>{status.workload?.name ?? id}</h1>
      <p>
        Declared {DECLARED[status.workload?.power?.desired ?? PowerState.UNSPECIFIED]},{" "}
        {CONVERGENCE[status.convergence]}
      </p>
      {said !== undefined && <p role="status">{said}</p>}
      {ended !== undefined && (
        <div role="alert">
          <p>
            Not live: last heard {age(now - live.heard.at)} ago. {metered ? "" : "Reconnecting…"}
          </p>
          {metered && <Reopen reopen={live.reopen} />}
        </div>
      )}
      {live.timeline !== undefined && <TimelineView timeline={live.timeline} />}
      <Observed status={status} freshness={freshness(status, live.heard)} now={now} />
    </main>
  );
}

/** Opening the live view again through a share link, which spends one of its uses. */
function Reopen({ reopen }: { readonly reopen: () => void }) {
  return <Button onPress={reopen}>Open it again (uses one of the link's uses)</Button>;
}
