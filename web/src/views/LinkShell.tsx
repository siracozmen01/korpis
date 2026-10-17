// SPDX-License-Identifier: AGPL-3.0-or-later

// A share link opened (§6 of docs/11-surface-web.md): no account, no sign-in, a screen holding
// exactly the object the link is for and exactly what it allows. The link arrives in the
// fragment of `/g/#korpislk_…`, is held in memory and cleared from the address bar, and every
// call is made with it and nothing else. Reading the link's own grant spends nothing; the
// workload's live status is one call, so opening the screen spends one use of the link's cap
// (§6.1 of docs/08-identity.md), and a reload, which forgets the link, asks for it again.

import { timestampDate } from "@bufbuild/protobuf/wkt";
import { useCallback, useMemo } from "react";
import { type Clients, ReachProvider } from "../clients";
import type { Grant } from "../gen/korpis/v1/authority_pb";
import { entryOf } from "./entries";
import { useLoad } from "./load";
import { Place, placeAt } from "./place";
import { Shown } from "./Shown";

/** The prefix every share link starts with (link.rs). */
export const LINK_PREFIX = "korpislk_";

/** The base path of a share link's screens. */
export const LINK_BASE = "/g";

/** What each action lets the link's holder do, in words. */
const ALLOWS: Record<string, string> = {
  "workload.read": "see its status",
  "workload.console.read": "read its console",
  "workload.console.write": "send commands to its console",
  "workload.start": "start it",
  "workload.stop": "stop it",
  "workload.restart": "restart it",
  "workload.file.read": "read its files",
  "workload.file.write": "change its files",
  "workload.exec": "run commands in it",
};

/** The share link's screen, given clients that present `link`; `path` is within `LINK_BASE`. */
export function LinkShell(props: { readonly clients: Clients | undefined; readonly path: string }) {
  const { clients } = props;
  if (clients === undefined) {
    return (
      <main>
        <h1>Open the link again</h1>
        <p>
          A share link is held only while its page is open, so it is not left in the browser. Open
          it again from where it was sent to you.
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
      what="what the link allows"
      done={(grant) =>
        grant === undefined ? (
          <p role="alert">This link opens nothing.</p>
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
    () => ({ clients, base: LINK_BASE, metered: true, allowed: new Set(grant.actions) }),
    [clients, grant],
  );
  const within = path.slice(LINK_BASE.length).replace(/^\/$/, "");
  const entry = within === "" ? entryOf(grant.scope) : placeAt(within);
  const expires = grant.conditions?.expiresAt;
  const uses = grant.conditions?.maxUses;
  return (
    <ReachProvider value={reach}>
      <aside aria-label="What this link allows">
        <p>
          This link lets you {grant.actions.map((a) => ALLOWS[a] ?? a).join(", ")}.
          {expires !== undefined && ` It works until ${timestampDate(expires).toLocaleString()}.`}
          {uses !== undefined && ` It can be used ${uses.toString()} times in all.`}
        </p>
      </aside>
      {entry !== undefined && <Place entry={entry} />}
    </ReachProvider>
  );
}
