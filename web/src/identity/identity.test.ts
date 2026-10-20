// SPDX-License-Identifier: AGPL-3.0-or-later

import { create } from "@bufbuild/protobuf";
import { DurationSchema } from "@bufbuild/protobuf/wkt";
import { IdentityBindingSchema } from "../gen/korpis/v1/authority_pb";
import {
  BLANK,
  BindingError,
  days,
  draftOf,
  longestMinutes,
  ofDays,
  subjectOf,
  subjectText,
  termsOf,
} from "./identity";

const minutesOf = (n: bigint) => create(DurationSchema, { seconds: n * 60n });

test("a set is drafted at the organization's provider, and a realm never holds a colon", () => {
  const draft = { ...BLANK, realm: "groups", name: "admins", provider: "corp" };
  const s = subjectOf(draft, "org_r");
  expect(s.provider).toBe("org_r/corp");
  expect(subjectText(s)).toBe("set:groups/admins");
  expect(() => subjectOf({ ...draft, realm: "https://idp" }, "org_r")).toThrow(BindingError);
  const person = subjectOf({ ...BLANK, kind: "user", user: "ann", provider: "corp" }, "org_r");
  expect(subjectText(person)).toBe("user:ann");
});

test("terms carry the interval and grace asked, and an edit keeps what it does not name", () => {
  const terms = termsOf({ ...BLANK, interval: "30", grace: "10" });
  expect(terms.reverifyInterval?.seconds).toBe(1800n);
  expect(terms.constraints?.grace?.seconds).toBe(600n);
  expect(() => termsOf({ ...BLANK, interval: "0" })).toThrow(BindingError);
  const bound = create(IdentityBindingSchema, {
    provider: "corp",
    template: { template: "tpl_m", version: 2n },
    allowsIrreversible: true,
    reverifyInterval: minutesOf(30n),
    constraints: { grace: minutesOf(10n) },
  });
  expect(longestMinutes(bound)).toBe(40n);
  const kept = termsOf({ ...draftOf(bound), interval: "60" }, bound);
  expect(kept.template?.template).toBe("tpl_m");
  expect(kept.allowsIrreversible).toBe(true);
  expect(kept.reverifyInterval?.seconds).toBe(3600n);
});

test("a ceiling is read and written in days", () => {
  expect(days(ofDays(7n))).toBe(7n);
  expect(days(undefined)).toBeUndefined();
});
