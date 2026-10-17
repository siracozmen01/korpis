// SPDX-License-Identifier: AGPL-3.0-or-later

// A workload's actions: each proposes its plan, which then interrupts as much as its risk calls
// for (§4 of docs/11-surface-web.md, planned.tsx).

import { Button } from "react-aria-components";
import { useReach } from "../clients";
import type { WorkloadStatus } from "../gen/korpis/v1/api_pb";
import { t } from "../i18n/intl";
import { LABEL, offered, propose } from "./act";
import { usePlanned } from "./planned";

/** The actions on the workload of `status`. */
export function Actions({ status }: { readonly status: WorkloadStatus }) {
  const { clients, allowed } = useReach();
  const flow = usePlanned();
  if (flow.review) return flow.review;
  return (
    <section
      aria-label={t({
        defaultMessage: "Actions",
        id: "ZWw2kSUj",
        description: "a workload's actions",
      })}
    >
      {offered(status, allowed).map((a) => (
        <Button
          key={a}
          isDisabled={!flow.ready}
          onPress={() => {
            flow.begin(LABEL[a], () => propose(clients, status, a));
          }}
        >
          {LABEL[a]}
        </Button>
      ))}
      {flow.shown}
    </section>
  );
}
