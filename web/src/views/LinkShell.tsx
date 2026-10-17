// SPDX-License-Identifier: AGPL-3.0-or-later

// A share link opened (§6 of docs/11-surface-web.md): no account, no sign-in, a screen holding
// exactly the object the link is for and exactly what it allows. The link arrives in the
// fragment of `/g/#korpislk_…`, is held in memory and cleared from the address bar, and every
// call is made with it and nothing else. Reading the link's own grant spends nothing; the
// workload's live status is one call, so opening the screen spends one use of the link's cap
// (§6.1 of docs/08-identity.md), and a reload, which forgets the link, asks for it again.

import { timestampDate } from "@bufbuild/protobuf/wkt";
import { useCallback, useMemo } from "react";
import { relay } from "../api";
import { type Clients, ReachProvider } from "../clients";
import type { Grant } from "../gen/korpis/v1/authority_pb";
import { entryOf } from "./entries";
import { useLoad } from "./load";
import { Place, placeAt } from "./place";
import { Shown } from "./Shown";
import { intl, t } from "../i18n/intl";
import { Themed } from "../theme/useTheme";
import { ALLOWS } from "../workload/allows";

/** The prefix every share link starts with (link.rs). */
export const LINK_PREFIX = "korpislk_";

/** The base path of a share link's screens. */
export const LINK_BASE = "/g";

/** The share link's screen, given clients that present `link`; `path` is within `LINK_BASE`. */
export function LinkShell(props: { readonly clients: Clients | undefined; readonly path: string }) {
  const { clients } = props;
  if (clients === undefined) {
    return (
      <main>
        <h1>{t({ defaultMessage: "Open the link again", id: "4xwCWoPe" })}</h1>
        <p>
          {t({
            defaultMessage:
              "A share link is held only while its page is open, so it is not left in the browser. Open it again from where it was sent to you.",
            id: "kvFHVWDb",
          })}
        </p>
      </main>
    );
  }
  return <Held clients={clients} path={props.path} />;
}

function Held({ clients, path }: { readonly clients: Clients; readonly path: string }) {
  const own = useLoad(
    useCallback(async () => (await clients.grants.listGrants({})).grants[0], [clients]),
  );
  return (
    <Shown
      loaded={own}
      reading={t({ defaultMessage: "Reading what the link allows…", id: "uzVYhI2z" })}
      done={(grant) =>
        grant === undefined ? (
          <p role="alert">{t({ defaultMessage: "This link opens nothing.", id: "4jmg2y3r" })}</p>
        ) : (
          <Granted clients={clients} grant={grant} path={path} />
        )
      }
    />
  );
}

/** What the link allows, and the object it is for, offering only the actions it holds. */
function Granted(props: {
  readonly clients: Clients;
  readonly grant: Grant;
  readonly path: string;
}) {
  const { clients, grant, path } = props;
  const reach = useMemo(
    () => ({ clients, base: LINK_BASE, metered: true, allowed: new Set(grant.actions), relay }),
    [clients, grant],
  );
  const within = path.slice(LINK_BASE.length).replace(/^\/$/, "");
  const entry = within === "" ? entryOf(grant.scope) : placeAt(within);
  const expires = grant.conditions?.expiresAt;
  const uses = grant.conditions?.maxUses;
  return (
    <ReachProvider value={reach}>
      <aside aria-label={t({ defaultMessage: "What this link allows", id: "IrnB9O3m" })}>
        <p>
          {t(
            {
              defaultMessage:
                "This link lets you {allows}.{expiring, select, yes { It works until {until}.} other {}}{capped, select, yes { It can be used {uses, plural, one {# time} other {# times}} in all.} other {}}",
              id: "to6AYEiK",
              description: "what a share link allows, until when, and how many uses",
            },
            {
              allows: intl.formatList(
                grant.actions.map((a) => ALLOWS[a] ?? a),
                { type: "conjunction" },
              ),
              expiring: expires === undefined ? "no" : "yes",
              until:
                expires === undefined
                  ? ""
                  : intl.formatDate(timestampDate(expires), {
                      dateStyle: "medium",
                      timeStyle: "short",
                    }),
              capped: uses === undefined ? "no" : "yes",
              uses: Number(uses ?? 0n),
            },
          )}
        </p>
      </aside>
      {entry !== undefined && (
        <Themed source={{ kind: "link", clients, entry }} stylesheet={false}>
          <Place entry={entry} />
        </Themed>
      )}
    </ReachProvider>
  );
}
