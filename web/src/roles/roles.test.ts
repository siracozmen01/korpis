// SPDX-License-Identifier: AGPL-3.0-or-later

import { create } from "@bufbuild/protobuf";
import { GrantTemplateSchema } from "../gen/korpis/v1/authority_pb";
import { ObjectKind } from "../gen/korpis/v1/common_pb";
import { RoleError, appliesAt, bodyOf, draftOf, lifetimeDays } from "./roles";

const moderator = create(GrantTemplateSchema, {
  id: "tpl_m",
  name: "Moderator",
  version: 2n,
  actions: ["workload.console.read", "workload.restart"],
  scopePattern: { kind: ObjectKind.PROJECT },
  delegation: { allowed: true, maxDepth: 1, actions: ["workload.restart"] },
  conditions: { lifetime: { seconds: 90n * 86_400n }, requiresMfa: true },
});

test("a role is offered where its kind is the place's", () => {
  expect(appliesAt(moderator, ObjectKind.PROJECT)).toBe(true);
  expect(appliesAt(moderator, ObjectKind.ORGANIZATION)).toBe(false);
  const anywhere = create(GrantTemplateSchema, { scopePattern: {} });
  expect(appliesAt(anywhere, ObjectKind.WORKLOAD)).toBe(true);
  expect(appliesAt(anywhere, ObjectKind.VOLUME)).toBe(false);
  expect(lifetimeDays(moderator)).toBe(90n);
});

test("an edit round-trips a role, and a field that cannot be read is named", () => {
  const draft = draftOf(moderator);
  expect(draft.version).toBe(2n);
  const body = bodyOf(draft);
  expect(body.actions).toEqual(moderator.actions);
  expect(body.delegation).toMatchObject({
    allowed: true,
    maxDepth: 1,
    actions: ["workload.restart"],
  });
  expect(body.conditions?.lifetime?.seconds).toBe(90n * 86_400n);
  expect(body.conditions?.requiresMfa).toBe(true);
  expect(body.scopePattern?.kind).toBe(ObjectKind.PROJECT);
  expect(body.scopePattern?.selector).toBeUndefined();

  const silent = bodyOf({ ...draft, delegates: "" });
  expect(silent.delegation).toMatchObject({ allowed: false, actions: [] });
  const gold = bodyOf({ ...draft, labels: "tier=gold" });
  expect(gold.scopePattern?.selector?.matchLabels).toEqual({ tier: "gold" });
  const named = (d: typeof draft) => {
    try {
      bodyOf(d);
    } catch (e) {
      return e instanceof RoleError ? e.field : "other";
    }
    return "none";
  };
  expect(named({ ...draft, name: " " })).toBe("name");
  expect(named({ ...draft, actions: "" })).toBe("actions");
  expect(named({ ...draft, labels: "tier" })).toBe("labels");
  expect(named({ ...draft, days: "0" })).toBe("days");
  expect(named({ ...draft, uses: "many" })).toBe("uses");
});
