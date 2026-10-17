// SPDX-License-Identifier: AGPL-3.0-or-later

// A workload's screen, as far as the navigation needs it: its name and the power its owner
// declared, live. The three states of what is observed, and the convergence timeline, are §2 and
// §3 of docs/11-surface-web.md and are drawn here as they are built.

import { useReach } from "../clients";
import { PowerState } from "../gen/korpis/v1/workload_pb";
import { Shown } from "./Shown";
import { useWatch } from "./watch";

const DECLARED: Record<PowerState, string> = {
  [PowerState.UNSPECIFIED]: "not declared",
  [PowerState.RUNNING]: "running",
  [PowerState.STOPPED]: "stopped",
};

/** The workload `id`. */
export function WorkloadPage({ id }: { readonly id: string }) {
  const { clients } = useReach();
  const status = useWatch(clients, id);
  return (
    <main>
      <Shown
        loaded={status}
        what="the workload"
        done={(s) => (
          <>
            <h1>{s.workload?.name ?? id}</h1>
            <p>Declared: {DECLARED[s.workload?.power?.desired ?? PowerState.UNSPECIFIED]}</p>
          </>
        )}
      />
    </main>
  );
}
