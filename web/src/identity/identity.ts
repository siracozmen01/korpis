// SPDX-License-Identifier: AGPL-3.0-or-later

// Identity bindings and the delegation ceiling as the panel shows and edits them (§5 and §5.1
// of docs/08-identity.md). A binding is shown with its interval and grace together, since the
// longest an unreachable provider extends it is their sum; a binding is drafted as text that
// becomes the request the control plane checks. Pure, so what the screen shows and sends is
// tested apart from the screen.

import { create } from "@bufbuild/protobuf";
import { type Duration, DurationSchema } from "@bufbuild/protobuf/wkt";
import { type IdentityBindingTerms, IdentityBindingTermsSchema } from "../gen/korpis/v1/api_pb";
import type { IdentityBinding } from "../gen/korpis/v1/authority_pb";
import { type ExternalIdentity, ExternalIdentitySchema } from "../gen/korpis/v1/tenancy_pb";

const MINUTE = 60n;
const DAY = 86_400n;

/** A subject as written: `user:<id>`, or `set:<realm>/<name>`. */
export function subjectText(s: ExternalIdentity | undefined): string {
  switch (s?.selector.case) {
    case "user":
      return `user:${s.selector.value}`;
    case "set":
      return `set:${s.selector.value.realm}/${s.selector.value.memberOf}`;
    default:
      return "-";
  }
}

/** A duration in whole minutes, if it has one. */
export function minutes(d: Duration | undefined): bigint | undefined {
  return d === undefined ? undefined : d.seconds / MINUTE;
}

/** The longest an unreachable provider extends `b` past its last confirmation, in minutes. */
export function longestMinutes(b: IdentityBinding): bigint | undefined {
  const interval = minutes(b.reverifyInterval);
  const grace = minutes(b.constraints?.grace);
  return interval === undefined || grace === undefined ? undefined : interval + grace;
}

/** A ceiling in whole days, rounded down, if there is one. */
export function days(d: Duration | undefined): bigint | undefined {
  return d === undefined ? undefined : d.seconds / DAY;
}

/** A ceiling of `n` days. */
export function ofDays(n: bigint): Duration {
  return create(DurationSchema, { seconds: n * DAY });
}

/** A binding as it is drafted. */
export interface BindingDraft {
  readonly kind: "user" | "set";
  readonly user: string;
  readonly realm: string;
  readonly name: string;
  readonly provider: string;
  readonly interval: string;
  readonly grace: string;
}

/** A blank draft: a set, fifteen minutes of interval and of grace. */
export const BLANK: BindingDraft = {
  kind: "set",
  user: "",
  realm: "",
  name: "",
  provider: "",
  interval: "15",
  grace: "15",
};

/** A field of a draft the panel cannot read, named before anything is sent. */
export class BindingError extends Error {
  constructor(readonly field: "subject" | "provider" | "interval" | "grace") {
    super(field);
  }
}

function wholeMinutes(text: string, field: "interval" | "grace", least: bigint): bigint {
  const t = text.trim();
  if (!/^\d+$/.test(t) || BigInt(t) < least) throw new BindingError(field);
  return BigInt(t);
}

/** The subject a draft names at `organization`'s provider. */
export function subjectOf(d: BindingDraft, organization: string): ExternalIdentity {
  const provider = `${organization}/${d.provider.trim()}`;
  if (d.kind === "user") {
    if (d.user.trim() === "") throw new BindingError("subject");
    return create(ExternalIdentitySchema, {
      provider,
      selector: { case: "user", value: d.user.trim() },
    });
  }
  const [realm, name] = [d.realm.trim(), d.name.trim()];
  if (realm === "" || name === "" || realm.includes(":")) throw new BindingError("subject");
  return create(ExternalIdentitySchema, {
    provider,
    selector: { case: "set", value: { realm, memberOf: name } },
  });
}

/** The terms a draft asks for: its interval, at least a minute, and its grace. */
export function termsOf(d: BindingDraft, keep?: IdentityBinding): IdentityBindingTerms {
  const interval = wholeMinutes(d.interval, "interval", 1n);
  const grace = wholeMinutes(d.grace, "grace", 0n);
  return create(IdentityBindingTermsSchema, {
    ...(keep?.template === undefined ? {} : { template: keep.template }),
    allowsIrreversible: keep?.allowsIrreversible ?? false,
    reverifyInterval: create(DurationSchema, { seconds: interval * MINUTE }),
    constraints: {
      requireMembership: keep?.constraints?.requireMembership ?? [],
      grace: create(DurationSchema, { seconds: grace * MINUTE }),
    },
  });
}

/** The draft an existing binding is edited from. */
export function draftOf(b: IdentityBinding): BindingDraft {
  return {
    ...BLANK,
    provider: b.provider,
    interval: String(minutes(b.reverifyInterval) ?? 15n),
    grace: String(minutes(b.constraints?.grace) ?? 15n),
  };
}
