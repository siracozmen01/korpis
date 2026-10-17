// SPDX-License-Identifier: AGPL-3.0-or-later

import { create, type MessageInitShape } from "@bufbuild/protobuf";
import { ObjectKind } from "../gen/korpis/v1/common_pb";
import { PlanSchema } from "../gen/korpis/v1/workload_pb";
import { tierOf } from "./tier";

const plan = (init: MessageInitShape<typeof PlanSchema>) =>
  create(PlanSchema, { workload: "wl_1", ...init });
const safe = { disruptive: false, dataAtRisk: [], dataAtRiskUnknown: false };

test("the interruption follows what the control plane classified, never the action's name", () => {
  const start = plan({ steps: [{ action: "start", reversible: true }], impact: safe });
  expect(tierOf(start)).toBe("direct");
  const restart = plan({
    steps: [
      { action: "stop", reversible: true, disruptive: true },
      { action: "start", reversible: true },
    ],
    impact: { ...safe, disruptive: true },
  });
  expect(tierOf(restart)).toBe("confirm");
  const several = plan({
    steps: [
      { action: "start", reversible: true, target: { kind: ObjectKind.WORKLOAD, id: "wl_2" } },
    ],
    impact: safe,
  });
  expect(tierOf(several)).toBe("confirm");
  const volume = plan({
    steps: [
      {
        action: "create_volume",
        reversible: true,
        target: { kind: ObjectKind.VOLUME, id: "vol_1" },
      },
    ],
    impact: safe,
  });
  expect(tierOf(volume)).toBe("direct");
  const remove = plan({
    steps: [{ action: "retire", reversible: false }],
    impact: { ...safe, dataAtRisk: ["world"] },
  });
  expect(tierOf(remove)).toBe("full");
  expect(
    tierOf(plan({ steps: [{ reversible: true }], impact: { ...safe, dataAtRiskUnknown: true } })),
  ).toBe("full");
  expect(tierOf(plan({ steps: [{ reversible: true }] }))).toBe("full");
});
