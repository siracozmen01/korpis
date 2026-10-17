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
import { intl, t } from "../i18n/intl";
import { PAGE_SIZE, Pages } from "../views/Pages";
import { refusal } from "../views/refusal";

const STATE: Record<GrantState, string> = {
  [GrantState.UNSPECIFIED]: t({
    defaultMessage: "unknown",
    id: "07xzvM/V",
    description: "a grant's state",
  }),
  [GrantState.ISSUED]: t({
    defaultMessage: "in force",
    id: "O3El0XWZ",
    description: "a grant's state",
  }),
  [GrantState.EXPIRED]: t({
    defaultMessage: "expired",
    id: "9b5VJp+J",
    description: "a grant's state",
  }),
  [GrantState.EXHAUSTED]: t({
    defaultMessage: "used up",
    id: "SsdT0IyI",
    description: "a grant's state",
  }),
  [GrantState.REVOKED]: t({
    defaultMessage: "revoked",
    id: "J35Dw5fO",
    description: "a grant's state",
  }),
  [GrantState.SUSPENDED]: t({
    defaultMessage: "suspended",
    id: "LMQ8/0ve",
    description: "a grant's state",
  }),
};

/** Who a subject is, as far as the grant says. */
function holder(s: Subject | undefined): string {
  const k = s?.kind;
  switch (k?.case) {
    case "link":
      return t({ defaultMessage: "whoever holds a share link", id: "ecniKn1Y" });
    case "user":
    case "token":
    case "workload":
    case "extension":
      return t(
        {
          defaultMessage:
            "{kind, select, user {user {id}} token {token {id}} workload {workload {id}} other {extension {id}}}",
          id: "8syzz/1K",
          description: "the subject a grant is issued to",
        },
        { kind: k.case, id: k.value },
      );
    default:
      return t({
        defaultMessage: "someone",
        id: "vxN1EcKI",
        description: "a grant's subject, unnamed",
      });
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
      reading={t({ defaultMessage: "Reading who holds authority over it…", id: "MnHvrun/" })}
      title={t({ defaultMessage: "Who can act on it", id: "fk9cvb2U" })}
      empty={t({ defaultMessage: "No grant is anchored on this workload itself.", id: "YCjxvQQ4" })}
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
          setSaid(
            t(
              {
                defaultMessage:
                  "{n, plural, =0 {Revoked.} one {Revoked, with # grant attenuated from it.} other {Revoked, with # grants attenuated from it.}}",
                id: "cbCaFOB4",
              },
              { n },
            ),
          );
        },
        (e: unknown) => {
          setSaid(refusal(e));
        },
      );
  };
  return (
    <li>
      {t(
        {
          defaultMessage:
            "{holder}: {actions}{labelled, select, yes { ({label})} other {}}, {state}{expiring, select, yes {, until {until}} other {}}",
          id: "XKxxNWbL",
          description: "one grant: who holds it, its actions, its label, state, and expiry",
        },
        {
          holder: holder(grant.subject),
          actions: intl.formatList(grant.actions, { type: "unit" }),
          labelled: grant.label === "" ? "no" : "yes",
          label: grant.label,
          state: STATE[state],
          expiring: expires === undefined ? "no" : "yes",
          until:
            expires === undefined
              ? ""
              : intl.formatDate(timestampDate(expires), {
                  dateStyle: "medium",
                  timeStyle: "short",
                }),
        },
      )}
      {state === GrantState.ISSUED && (
        <Button onPress={revoke}>
          {t({ defaultMessage: "Revoke", id: "Eu7t3/2z", description: "button" })}
        </Button>
      )}
      {said !== undefined && <span role="status"> {said}</span>}
    </li>
  );
}
