// SPDX-License-Identifier: AGPL-3.0-or-later

// A plan as the control plane computed it (§4 of docs/11-surface-web.md): every step, each with
// the object it acts on and how it was classified, and the impact: the downtime it estimates,
// the data at risk, and why it does or does not apply without a person. Nothing is summarized
// away: the affected objects are enumerated.

import { durationMs } from "@bufbuild/protobuf/wkt";
import { ObjectKind } from "../gen/korpis/v1/common_pb";
import type { Plan, Step } from "../gen/korpis/v1/workload_pb";
import { intl, t } from "../i18n/intl";

/** The kind of object a step acts on, in words. */
function kindOf(kind: ObjectKind): string {
  const named: Partial<Record<ObjectKind, string>> = {
    [ObjectKind.WORKLOAD]: "workload",
    [ObjectKind.VOLUME]: "volume",
    [ObjectKind.ENDPOINT]: "endpoint",
    [ObjectKind.STREAM]: "stream",
  };
  const said = named[kind];
  return t(
    {
      defaultMessage:
        "{kind, select, workload {workload} volume {volume} endpoint {endpoint} stream {stream} other {object}}",
      id: "q57TbyeF",
      description: "the kind of object a plan's step acts on",
    },
    { kind: said ?? "other" },
  );
}

/** What a step is marked as. */
function marks(s: Step): string {
  const said = [
    s.reversible ? "" : t({ defaultMessage: "IRREVERSIBLE", id: "vcc/+oBE" }),
    s.disruptive ? t({ defaultMessage: "interrupts service", id: "ConS74VK" }) : "",
  ];
  return intl.formatList(
    said.filter((m) => m !== ""),
    { type: "unit" },
  );
}

/** The impact, in sentences. */
function impact(plan: Plan): string[] {
  const i = plan.impact;
  if (i === undefined) return [t({ defaultMessage: "Its impact is unknown.", id: "iJNuzDiK" })];
  const down = i.downtimeEstimate;
  const lines = [
    i.downtimeUnknown || down === undefined
      ? t({ defaultMessage: "Downtime: unknown.", id: "w9y4DE8u" })
      : t(
          {
            defaultMessage: "Downtime: about {seconds, plural, one {# second} other {# seconds}}.",
            id: "HErk5s7f",
          },
          { seconds: Math.round(durationMs(down) / 1000) },
        ),
  ];
  if (i.dataAtRiskUnknown)
    lines.push(t({ defaultMessage: "Data at risk: unknown.", id: "ZVtnkB2O" }));
  else if (i.dataAtRisk.length > 0) {
    lines.push(
      t(
        { defaultMessage: "Data at risk: {volumes}.", id: "Zs6s57kx" },
        { volumes: intl.formatList(i.dataAtRisk, { type: "unit" }) },
      ),
    );
  }
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
              <span className="aside">
                {kindOf(s.target.kind)} <code>{s.target.id}</code>
              </span>
            )}
            {marks(s) !== "" && (
              <strong className="aside">
                {t(
                  { defaultMessage: "({marks})", id: "1BDY1a0A", description: "a step's marks" },
                  { marks: marks(s) },
                )}
              </strong>
            )}
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
