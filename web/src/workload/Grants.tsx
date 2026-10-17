// SPDX-License-Identifier: AGPL-3.0-or-later

// Who holds authority over a workload (§6 of docs/11-surface-web.md): the grants anchored on it,
// to whom, allowing what, until when, and in what state, each revocable by whoever may revoke it.
// Revoking a grant revokes what was attenuated from it too, and the screen says how many. Shown
// only to a caller who may read grants there; to anyone else the list is not there at all.

import { timestampDate } from "@bufbuild/protobuf/wkt";
import { useCallback, useState } from "react";
import { Button } from "react-aria-components";
import { useReach } from "../clients";
import { type Grant, GrantState } from "../gen/korpis/v1/authority_pb";
import type { Subject } from "../gen/korpis/v1/tenancy_pb";
import { PAGE_SIZE, Pages } from "../views/Pages";
import { refusal } from "../views/refusal";

const STATE: Record<GrantState, string> = {
  [GrantState.UNSPECIFIED]: "unknown",
  [GrantState.ISSUED]: "in force",
  [GrantState.EXPIRED]: "expired",
  [GrantState.EXHAUSTED]: "used up",
  [GrantState.REVOKED]: "revoked",
  [GrantState.SUSPENDED]: "suspended",
};

/** Who a subject is, as far as the grant says. */
function holder(s: Subject | undefined): string {
  const k = s?.kind;
  switch (k?.case) {
    case "link":
      return "whoever holds a share link";
    case "user":
    case "token":
    case "workload":
    case "extension":
      return `${k.case} ${k.value}`;
    default:
      return "someone";
  }
}

/** The grants on workload `id`. */
export function Grants({ id }: { readonly id: string }) {
  const { clients } = useReach();
  const load = useCallback(
    async (pageToken: string) => {
      const r = await clients.grants.listGrants({
        filter: { case: "scope", value: { kind: { case: "workload", value: id } } },
        page: { pageSize: PAGE_SIZE, pageToken },
      });
      return { items: r.grants, next: r.page?.nextPageToken ?? "" };
    },
    [clients, id],
  );
  return (
    <Pages
      load={load}
      what="who holds authority over it"
      title="Who can act on it"
      empty="No grant is anchored on this workload itself."
      item={(g) => <GrantItem key={g.id} grant={g} />}
    />
  );
}

/** One grant, and revoking it. */
function GrantItem({ grant }: { readonly grant: Grant }) {
  const { clients } = useReach();
  const [state, setState] = useState(grant.state);
  const [said, setSaid] = useState<string>();
  const expires = grant.conditions?.expiresAt;
  const revoke = () => {
    clients.grants
      .revokeGrant({ mutation: { idempotencyKey: crypto.randomUUID() }, grant: grant.id })
      .then(
        (r) => {
          setState(GrantState.REVOKED);
          const n = Number(r.cascaded);
          setSaid(n === 0 ? "Revoked." : `Revoked, with ${n.toString()} attenuated from it.`);
        },
        (e: unknown) => {
          setSaid(refusal(e));
        },
      );
  };
  return (
    <li>
      {holder(grant.subject)}: {grant.actions.join(", ")}
      {grant.label !== "" && <> ({grant.label})</>}, {STATE[state]}
      {expires !== undefined && `, until ${timestampDate(expires).toLocaleString()}`}
      {state === GrantState.ISSUED && <Button onPress={revoke}>Revoke</Button>}
      {said !== undefined && <span role="status"> {said}</span>}
    </li>
  );
}
