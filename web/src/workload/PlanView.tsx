// SPDX-License-Identifier: AGPL-3.0-or-later

// A plan as the control plane computed it (§4 of docs/11-surface-web.md): every step, each with
// the object it acts on and how it was classified, and the impact: the downtime it estimates,
// the data at risk, and why it does or does not apply without a person. Nothing is summarized
// away: the affected objects are enumerated.

import { durationMs } from "@bufbuild/protobuf/wkt";
import { ObjectKind } from "../gen/korpis/v1/common_pb";
import type { Plan, Step } from "../gen/korpis/v1/workload_pb";

const KIND: Partial<Record<ObjectKind, string>> = {
  [ObjectKind.WORKLOAD]: "workload",
  [ObjectKind.VOLUME]: "volume",
  [ObjectKind.ENDPOINT]: "endpoint",
  [ObjectKind.STREAM]: "stream",
};

/** What a step is marked as. */
function marks(s: Step): string {
  const said = [s.reversible ? "" : "IRREVERSIBLE", s.disruptive ? "interrupts service" : ""];
  return said.filter((m) => m !== "").join(", ");
}

/** The impact, in sentences. */
function impact(plan: Plan): string[] {
  const i = plan.impact;
  if (i === undefined) return ["Its impact is unknown."];
  const down = i.downtimeEstimate;
  const lines = [
    i.downtimeUnknown || down === undefined
      ? "Downtime: unknown."
      : `Downtime: about ${Math.round(durationMs(down) / 1000).toString()} s.`,
  ];
  if (i.dataAtRiskUnknown) lines.push("Data at risk: unknown.");
  else if (i.dataAtRisk.length > 0) lines.push(`Data at risk: ${i.dataAtRisk.join(", ")}.`);
  if (i.approvalReason !== "") lines.push(i.approvalReason);
  return lines;
}

/** The plan, step by step. */
export function PlanView({ plan }: { readonly plan: Plan }) {
  return (
    <div>
      <ol>
        {plan.steps.map((s) => (
          <li key={s.index}>
            {s.action}
            {s.target !== undefined && (
              <>
                {" "}
                {KIND[s.target.kind] ?? "object"} <code>{s.target.id}</code>
              </>
            )}
            {marks(s) !== "" && <strong> ({marks(s)})</strong>}
            {s.reason !== "" && <small> {s.reason}</small>}
          </li>
        ))}
      </ol>
      {impact(plan).map((line) => (
        <p key={line}>{line}</p>
      ))}
    </div>
  );
}
