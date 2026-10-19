// SPDX-License-Identifier: AGPL-3.0-or-later

// Grant templates as the panel shows and edits them (§4 of docs/08-identity.md): the roles of
// the interface. A role is offered where its pattern's kind is the kind of the place, and is
// edited as a draft of text that becomes the body the control plane checks. Pure, so what the
// screen shows and sends is tested apart from the screen.

import { create } from "@bufbuild/protobuf";
import { DurationSchema } from "@bufbuild/protobuf/wkt";
import { type GrantTemplateBody, GrantTemplateBodySchema } from "../gen/korpis/v1/api_pb";
import type { GrantTemplate } from "../gen/korpis/v1/authority_pb";
import { LabelSelectorSchema, ObjectKind } from "../gen/korpis/v1/common_pb";
import { labelsText, parseLabels } from "../views/quota";

const DAY = 86_400n;

/** Whether `template` is applied at a place of `kind`. */
export function appliesAt(template: GrantTemplate, kind: ObjectKind): boolean {
  const want = template.scopePattern?.kind ?? ObjectKind.UNSPECIFIED;
  const applicable = [ObjectKind.ORGANIZATION, ObjectKind.PROJECT, ObjectKind.WORKLOAD];
  return applicable.includes(kind) && (want === ObjectKind.UNSPECIFIED || want === kind);
}

/** A template's lifetime in whole days, if it has one that is. */
export function lifetimeDays(template: GrantTemplate): bigint | undefined {
  const seconds = template.conditions?.lifetime?.seconds;
  return seconds === undefined || seconds % DAY !== 0n ? undefined : seconds / DAY;
}

/** A role as an edit holds it: each field as text, the version it was read at. */
export interface RoleDraft {
  readonly name: string;
  readonly actions: string;
  readonly kind: ObjectKind;
  readonly labels: string;
  readonly delegates: string;
  readonly depth: string;
  readonly days: string;
  readonly mfa: boolean;
  readonly uses: string;
  readonly version: bigint | undefined;
}

const list = (text: string) =>
  text
    .split(",")
    .map((a) => a.trim())
    .filter((a) => a !== "");

/** The edit of `template`, or of a new role. */
export function draftOf(template?: GrantTemplate): RoleDraft {
  const d = template?.delegation;
  const days = template === undefined ? undefined : lifetimeDays(template);
  return {
    name: template?.name ?? "",
    actions: template?.actions.join(", ") ?? "",
    kind: template?.scopePattern?.kind ?? ObjectKind.PROJECT,
    labels: labelsText(template?.scopePattern?.selector),
    delegates: d?.allowed === true ? d.actions.join(", ") : "",
    depth: d?.allowed === true ? String(d.maxDepth ?? 1) : "1",
    days: days === undefined ? "" : String(days),
    mfa: template?.conditions?.requiresMfa ?? false,
    uses: template?.conditions?.maxUses === undefined ? "" : String(template.conditions.maxUses),
    version: template?.version,
  };
}

/** Why a draft cannot be sent: the field it names. */
export class RoleError extends Error {
  constructor(readonly field: "name" | "actions" | "labels" | "depth" | "days" | "uses") {
    super(field);
  }
}

function whole(text: string, field: "depth" | "days" | "uses"): bigint | undefined {
  const t = text.trim();
  if (t === "") return undefined;
  if (!/^[1-9]\d*$/.test(t)) throw new RoleError(field);
  return BigInt(t);
}

/** The body `draft` asks for; throws `RoleError` naming the first field that cannot be read.
 * The control plane checks the rest: that the actions exist, that it delegates only what it
 * permits. */
export function bodyOf(draft: RoleDraft): GrantTemplateBody {
  if (draft.name.trim() === "") throw new RoleError("name");
  const actions = list(draft.actions);
  if (actions.length === 0) throw new RoleError("actions");
  const matchLabels = parseLabels(draft.labels);
  if (matchLabels === undefined) throw new RoleError("labels");
  const delegates = list(draft.delegates);
  const depth = whole(draft.depth, "depth");
  const days = whole(draft.days, "days");
  const uses = whole(draft.uses, "uses");
  const selected = Object.keys(matchLabels).length > 0;
  return create(GrantTemplateBodySchema, {
    name: draft.name.trim(),
    actions,
    scopePattern: {
      kind: draft.kind,
      ...(selected ? { selector: create(LabelSelectorSchema, { matchLabels }) } : {}),
    },
    delegation:
      delegates.length > 0
        ? { allowed: true, maxDepth: Number(depth ?? 1n), actions: delegates }
        : { allowed: false, actions: [] },
    conditions: {
      ...(days === undefined ? {} : { lifetime: create(DurationSchema, { seconds: days * DAY }) }),
      requiresMfa: draft.mfa,
      ...(uses === undefined ? {} : { maxUses: uses }),
      sourceCidr: [],
    },
  });
}
