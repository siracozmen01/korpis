// SPDX-License-Identifier: AGPL-3.0-or-later

// Which stage a workload that has not converged is at (§3 of docs/11-surface-web.md): "declared
// 40 seconds ago, node fra-3 has not acknowledged" is a diagnosis, a greyed-out button is not. Each
// sentence is read from the status the control plane computed (§4.3 of docs/02-architecture.md),
// never guessed: the stage is the convergence it reports, and the detail what the status holds.
// Each is one whole message, the node named inside it by a select, so a language can order and
// inflect it as its grammar needs rather than as English splices fragments.

import { timestampMs } from "@bufbuild/protobuf/wkt";
import { Convergence, type WorkloadStatus } from "../gen/korpis/v1/api_pb";
import { ObservedState } from "../gen/korpis/v1/workload_pb";
import { intl, t } from "../i18n/intl";
import { age } from "./freshness";
import { UNSATISFIABLE } from "./words";

/** The node a workload is placed on, as far as the caller may read it, as a message's values:
 * `where` is `node` with its name, `region` with the region, or `none`. */
export function placeOf(status: WorkloadStatus): { where: string; node: string; region: string } {
  const p = status.placement;
  const node = p?.node ?? "";
  const region = p?.region ?? "";
  const where = node !== "" ? "node" : region !== "" ? "region" : "none";
  return { where, node, region };
}

/** When what it converges to was declared: the approved intent's or the power's, the later. */
export function declaredAt(status: WorkloadStatus): number | undefined {
  const times = [status.target?.declaredAt, status.workload?.power?.setAt].flatMap((t) =>
    t === undefined ? [] : [timestampMs(t)],
  );
  return times.length === 0 ? undefined : Math.max(...times);
}

/** Why no node took it, from the newest attempt's explanation. */
function unplaced(status: WorkloadStatus): string {
  const rejected = status.unplaced?.explanation?.rejected ?? [];
  if (rejected.length === 0) return t({ defaultMessage: "No node can take it.", id: "CCPT98zG" });
  const checks = rejected.map((c) =>
    c.nodes === undefined
      ? c.check
      : t(
          {
            defaultMessage: "{check} ({nodes, plural, one {# node} other {# nodes}})",
            id: "5OvTyyrO",
            description: "a placement check that refused, and how many nodes it refused",
          },
          { check: c.check, nodes: c.nodes },
        ),
  );
  return t(
    { defaultMessage: "No node can take it: {checks}.", id: "liG+xsKp" },
    { checks: intl.formatList(checks, { type: "unit" }) },
  );
}

/** Why the node cannot satisfy what it holds. */
function failing(status: WorkloadStatus): string {
  const o = status.observation;
  const place = placeOf(status);
  if (o === undefined) {
    return t(
      {
        defaultMessage:
          "{where, select, node {Node {node}} region {Its node in {region}} other {Its node}} cannot run it.",
        id: "YYmFtOqj",
      },
      place,
    );
  }
  if (o.state === ObservedState.CRASHED) {
    const code = o.exitCode;
    return t(
      {
        defaultMessage:
          "It crashed on {where, select, node {node {node}} region {its node in {region}} other {its node}}{coded, select, yes {, exit code {code}} other {}}.",
        id: "HnsZRsrg",
      },
      { ...place, coded: code === undefined ? "no" : "yes", code: code?.toString() ?? "" },
    );
  }
  const why = o.reasonDetail !== "" ? o.reasonDetail : UNSATISFIABLE[o.reason];
  return t(
    {
      defaultMessage:
        "{where, select, node {Node {node}} region {Its node in {region}} other {Its node}} holds it and cannot run it: {why}.",
      id: "sTddvshh",
    },
    { ...place, why },
  );
}

/** What it is waiting on while it applies. */
function applying(status: WorkloadStatus, now: number): string {
  const plan = status.activePlan;
  const progress = status.observation?.planProgress;
  const place = placeOf(status);
  if (plan !== undefined && progress?.plan === plan.id) {
    return t(
      {
        defaultMessage:
          "{where, select, node {Node {node}} region {Its node in {region}} other {Its node}} is applying step {step, number} of {steps, number}.",
        id: "VXCry6sZ",
      },
      { ...place, step: progress.step + 1, steps: plan.steps.length },
    );
  }
  const at = declaredAt(status);
  return t(
    {
      defaultMessage:
        "{known, select, yes {Declared {ago} ago} other {Declared}}, {where, select, node {node {node}} region {its node in {region}} other {its node}} has not acknowledged.",
      id: "DbQxUDQO",
    },
    {
      ...place,
      known: at === undefined ? "no" : "yes",
      ago: at === undefined ? "" : age(now - at),
    },
  );
}

/** The stage a workload is at, in a sentence; nothing once it has converged. */
export function diagnosis(status: WorkloadStatus, now: number): string | undefined {
  // Held, it is converged to stopped, which is not what it declares: said first, whatever the
  // stage (§2.1 of docs/08-identity.md).
  if (status.heldBySuspension)
    return t({
      defaultMessage: "Held stopped while its organization, or one above it, is suspended.",
      id: "m8Zm/he+",
    });
  switch (status.convergence) {
    case Convergence.CONVERGED:
      return undefined;
    case Convergence.APPLYING:
      return applying(status, now);
    case Convergence.FAILING:
      return failing(status);
    case Convergence.UNPLACED:
      return unplaced(status);
    case Convergence.AWAITING_APPROVAL:
      return t({ defaultMessage: "Its plan waits for approval.", id: "d147hhoF" });
    case Convergence.SCHEDULED: {
      const after = status.activePlan?.applyAfter;
      if (after === undefined)
        return t({ defaultMessage: "Its plan is scheduled.", id: "PIXTVfME" });
      const when = intl.formatDate(timestampMs(after), { dateStyle: "medium", timeStyle: "short" });
      return t({ defaultMessage: "Its plan applies at {when}.", id: "2E9YtXqZ" }, { when });
    }
    case Convergence.UNKNOWN:
    case Convergence.UNSPECIFIED: {
      const o = status.observation;
      if (o?.observedAt === undefined) {
        return t({ defaultMessage: "Nothing has been observed of it yet.", id: "67v1+jt2" });
      }
      return t(
        {
          defaultMessage:
            "{where, select, node {Node {node}} region {Its node in {region}} other {Its node}} has not been heard for {span}.",
          id: "JiItQLSQ",
        },
        { ...placeOf(status), span: age(now - timestampMs(o.observedAt)) },
      );
    }
  }
}
