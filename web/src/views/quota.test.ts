// SPDX-License-Identifier: AGPL-3.0-or-later

import { create } from "@bufbuild/protobuf";
import { LabelOperator, LabelSelectorSchema } from "../gen/korpis/v1/common_pb";
import { QuotaEntrySchema, QuotaSetSchema } from "../gen/korpis/v1/tenancy_pb";
import { DraftError, draftOf, givenBelow, maySell, parseLabels, sameShape, setOf } from "./quota";

const GIB = 1n << 30n;

function entry(memory: bigint, limit: bigint, nodes?: Record<string, string>) {
  return create(QuotaEntrySchema, {
    reservationBounds: { memoryBytes: memory },
    limitBounds: { memoryBytes: limit },
    ...(nodes === undefined ? {} : { nodes: create(LabelSelectorSchema, { matchLabels: nodes }) }),
  });
}

test("labels are read as key=value pairs, and anything else is not labels", () => {
  expect(parseLabels("region=eu, tier=gold")).toEqual({ region: "eu", tier: "gold" });
  expect(parseLabels("")).toEqual({});
  expect(parseLabels("region")).toBeUndefined();
  expect(parseLabels("=eu")).toBeUndefined();
});

test("what was given below is summed by shape, and what may be sold is the limit times the ratio", () => {
  const all = entry(32n * GIB, 32n * GIB);
  const eu = entry(8n * GIB, 8n * GIB, { region: "eu" });
  expect(sameShape(all, eu)).toBe(false);
  const child = create(QuotaSetSchema, { entries: [entry(4n * GIB, 16n * GIB), eu] });
  const given = givenBelow(all, [child, child]);
  expect(given.reserved.get("memoryBytes")).toBe(8n * GIB);
  expect(given.sold.get("memoryBytes")).toBe(32n * GIB);
  const reseller = create(QuotaSetSchema, {
    entries: [all],
    childOvercommitPercent: { memory_bytes: 400 },
  });
  expect(maySell(reseller, all, "memoryBytes")).toBe(128n * GIB);
  expect(maySell(reseller, all, "cpuMillis")).toBeUndefined();
});

test("an edit round-trips a set, and a bound that is not a number names its field", () => {
  const set = create(QuotaSetSchema, {
    entries: [entry(0n, 16n * GIB, { region: "eu" })],
    childOvercommitPercent: { memory_bytes: 200 },
  });
  const again = setOf(draftOf(set));
  expect(again.entries[0]?.reservationBounds?.memoryBytes).toBe(0n);
  expect(again.entries[0]?.limitBounds?.memoryBytes).toBe(16n * GIB);
  expect(again.entries[0]?.nodes?.matchLabels).toEqual({ region: "eu" });
  expect(again.entries[0]?.selector).toBeUndefined();
  expect(again.childOvercommitPercent).toEqual({ memory_bytes: 200 });
  const wrong = {
    entries: [{ workloads: "", nodes: "", reserved: {}, sold: { memoryBytes: "lots" } }],
    overcommit: {},
  };
  expect(() => setOf(wrong)).toThrow(DraftError);
});

test("an edit keeps the expressions it does not write, and an empty selector stays present", () => {
  const tier = { key: "tier", operator: LabelOperator.IN, values: ["gold", "silver"] };
  const set = create(QuotaSetSchema, {
    entries: [
      create(QuotaEntrySchema, {
        selector: create(LabelSelectorSchema, {
          matchLabels: { game: "mc" },
          matchExpressions: [tier],
        }),
        nodes: create(LabelSelectorSchema, {}),
      }),
    ],
  });
  const draft = draftOf(set);
  const kept = setOf(draft).entries[0];
  expect(kept?.selector?.matchExpressions.map((e) => e.values)).toEqual([["gold", "silver"]]);
  expect(kept?.nodes).toBeDefined();
  const first = draft.entries[0];
  if (first === undefined) throw new Error("the draft lost its entry");
  const relabelled = setOf({ ...draft, entries: [{ ...first, workloads: "game=rust" }] });
  expect(relabelled.entries[0]?.selector?.matchLabels).toEqual({ game: "rust" });
  expect(relabelled.entries[0]?.selector?.matchExpressions).toHaveLength(1);
});
