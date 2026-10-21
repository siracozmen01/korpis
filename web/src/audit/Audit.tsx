// SPDX-License-Identifier: AGPL-3.0-or-later

// An organization's audit trail (§4 of docs/15-observability.md, §1 of docs/11-surface-web.md):
// what was done to anything in its subtree, newest last, as far as the viewer's grants reach,
// a page at a time. Each entry says when, what, how it turned out, who did it, and the client
// that relayed them if one did, so a person acting through a bot is never shown as the bot. A
// refusal is marked as one: what was attempted against what the organization holds is the
// signal an audit trail exists for.

import { timestampDate } from "@bufbuild/protobuf/wkt";
import { useCallback } from "react";
import { useReach } from "../clients";
import type { Subject } from "../gen/korpis/v1/tenancy_pb";
import { type Effect, EffectOutcome } from "../gen/korpis/v1/workload_pb";
import { subjectText } from "../identity/identity";
import { intl, t } from "../i18n/intl";
import { PAGE_SIZE, Pages } from "../views/Pages";

/** Who a subject is, as an effect names them. */
export function actorText(s: Subject | undefined): string {
  const k = s?.kind;
  switch (k?.case) {
    case "user":
    case "token":
    case "workload":
    case "extension":
      return t(
        {
          defaultMessage:
            "{kind, select, user {user {id}} token {token {id}} workload {workload {id}} other {extension {id}}}",
          id: "7HIqExhL",
          description: "who did what an effect records",
        },
        { kind: k.case, id: k.value },
      );
    case "link":
      return t(
        { defaultMessage: "whoever held the share link {id}", id: "wl8ilnIC" },
        { id: k.value },
      );
    case "external":
      return t(
        { defaultMessage: "{subject} at {provider}", id: "LivUik0h" },
        { subject: subjectText(k.value), provider: k.value.provider },
      );
    case "system":
      return t({
        defaultMessage: "the control plane",
        id: "sjzOngga",
        description: "an effect's actor",
      });
    case "anonymous":
      return t({
        defaultMessage: "someone not signed in",
        id: "G75UWPra",
        description: "an effect's actor",
      });
    default:
      return t({
        defaultMessage: "someone",
        id: "p+T4+0NW",
        description: "an effect's actor, unnamed",
      });
  }
}

/** How an effect turned out, in words. */
function outcomeText(o: EffectOutcome): string {
  switch (o) {
    case EffectOutcome.SUCCEEDED:
      return t({ defaultMessage: "done", id: "EMBvy9ZF", description: "an effect's outcome" });
    case EffectOutcome.FAILED:
      return t({ defaultMessage: "failed", id: "FGxoaUqS", description: "an effect's outcome" });
    case EffectOutcome.SKIPPED:
      return t({
        defaultMessage: "not needed",
        id: "yl1+rIGt",
        description: "an effect's outcome",
      });
    case EffectOutcome.ROLLED_BACK:
      return t({ defaultMessage: "undone", id: "rFeYbQzb", description: "an effect's outcome" });
    case EffectOutcome.DENIED:
      return t({ defaultMessage: "refused", id: "BqSUTrrS", description: "an effect's outcome" });
    default:
      return "-";
  }
}

/** One entry of the trail. */
function Entry({ effect: e }: { readonly effect: Effect }) {
  const at =
    e.at === undefined
      ? "-"
      : intl.formatDate(timestampDate(e.at), {
          dateStyle: "medium",
          timeStyle: "medium",
        });
  const refused = e.outcome === EffectOutcome.DENIED;
  const by =
    e.relay === undefined
      ? t({ defaultMessage: "by {actor}", id: "8BU1HZM/" }, { actor: actorText(e.actor) })
      : t(
          { defaultMessage: "by {actor}, through {relay}", id: "iQx3ZysS" },
          { actor: actorText(e.actor), relay: actorText(e.relay) },
        );
  return (
    <li className={refused ? "effect refused" : "effect"}>
      <time>{at}</time>
      <code>{e.action}</code>
      <strong>{outcomeText(e.outcome)}</strong>
      <span>{by}</span>
      {e.count > 1n && (
        <small className="aside">
          {t({ defaultMessage: "{count} times", id: "qvul+rhG" }, { count: Number(e.count) })}
        </small>
      )}
    </li>
  );
}

/** The audit trail of organization `id`'s subtree. */
export function Audit({ id }: { readonly id: string }) {
  const { clients } = useReach();
  const load = useCallback(
    async (pageToken: string) => {
      const r = await clients.effects.listEffects({
        organization: id,
        page: { pageSize: PAGE_SIZE, pageToken },
      });
      return { items: r.effects, next: r.page?.nextPageToken ?? "" };
    },
    [clients.effects, id],
  );
  return (
    <Pages
      load={load}
      reading={t({ defaultMessage: "Reading what was done here…", id: "elS34uKD" })}
      title={t({
        defaultMessage: "Audit",
        id: "txhbVtC/",
        description: "an organization's effect log",
      })}
      empty={t({ defaultMessage: "Nothing you can read.", id: "N8Z+Vwlk" })}
      item={(e) => <Entry key={e.id} effect={e} />}
    />
  );
}
