// SPDX-License-Identifier: AGPL-3.0-or-later

import { create, type MessageInitShape } from "@bufbuild/protobuf";
import { GrantSchema, GrantState, ScopeSchema } from "../gen/korpis/v1/authority_pb";
import { entries, pathOf } from "./entries";

type Kind = NonNullable<MessageInitShape<typeof ScopeSchema>["kind"]>;

const grant = (kind: Kind, state = GrantState.ISSUED) =>
  create(GrantSchema, { scope: { kind }, state });

test("the places are those the working grants are anchored on, each once", () => {
  const found = entries([
    grant({ case: "organization", value: "org_a" }),
    grant({ case: "workload", value: "wl_b" }),
    grant({ case: "organization", value: "org_a" }),
    grant({
      case: "selector",
      value: { within: { kind: { case: "project", value: "prj_c" } } },
    }),
    grant({ case: "volume", value: "vol_d" }),
    grant({ case: "project", value: "prj_e" }, GrantState.REVOKED),
  ]);
  expect(found).toEqual([
    { kind: "organization", id: "org_a" },
    { kind: "workload", id: "wl_b" },
    { kind: "project", id: "prj_c" },
  ]);
  expect(found.map((e) => pathOf("/g", e))).toEqual(["/g/o/org_a", "/g/w/wl_b", "/g/p/prj_c"]);
});
