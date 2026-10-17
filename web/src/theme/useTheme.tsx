// SPDX-License-Identifier: AGPL-3.0-or-later

// The theme the page wears, chosen by who is looking rather than by what is shown (§7 of
// docs/11-surface-web.md): a signed-in viewer wears the brand of whoever provides their access,
// the top organization they can read, whatever deeper place they open, so a tenant's theme never
// dresses its provider's screens; a share link's holder wears the brand of the place it opens, its
// colours, name, and logo but never its stylesheet; before anyone signs in, the installation
// root's. A theme that cannot be read leaves the panel's own on, which is what its absence means.

import {
  createContext,
  useContext,
  useEffect,
  useLayoutEffect,
  useState,
  type ReactNode,
} from "react";
import type { Client } from "@connectrpc/connect";
import type { Clients } from "../clients";
import type { ServerInfoService } from "../gen/korpis/v1/api_pb";
import type { Organization, Theme } from "../gen/korpis/v1/tenancy_pb";
import { PAGE_SIZE } from "../views/Pages";
import type { Entry } from "../views/entries";
import { logoSource, productName, wear } from "./theme";

/** Where a theme is read from. */
export type Source =
  | {
      readonly kind: "viewer";
      readonly clients: Clients;
      readonly info: Client<typeof ServerInfoService>;
    }
  | { readonly kind: "link"; readonly clients: Clients; readonly entry: Entry }
  | { readonly kind: "sign-in"; readonly info: Client<typeof ServerInfoService> };

/** The one organization of `orgs` whose parent is not among them, the top of what the viewer
 * reads; none where there is no such organization or more than one, as for a viewer holding
 * grants on two unrelated organizations, whose provider the panel cannot name. */
export function topOf(orgs: readonly Organization[]): Organization | undefined {
  const ids = new Set(orgs.map((o) => o.id));
  const tops = orgs.filter((o) => o.parent === undefined || !ids.has(o.parent));
  return tops.length === 1 ? tops[0] : undefined;
}

/** Every organization `clients`' caller can read, every page of them. */
async function readable(clients: Clients): Promise<Organization[]> {
  const orgs: Organization[] = [];
  let pageToken = "";
  do {
    const r = await clients.organizations.listOrganizations({
      page: { pageSize: PAGE_SIZE, pageToken },
    });
    orgs.push(...r.organizations);
    pageToken = r.page?.nextPageToken ?? "";
  } while (pageToken !== "");
  return orgs;
}

async function read(source: Source): Promise<Theme | undefined> {
  switch (source.kind) {
    case "sign-in":
      return (await source.info.getSignInTheme({})).governing?.theme;
    case "link": {
      const { kind, id } = source.entry;
      const r = await source.clients.organizations.getTheme({
        of: { kind: { case: kind, value: id } },
      });
      return r.governing?.theme;
    }
    case "viewer": {
      const top = topOf(await readable(source.clients));
      if (top === undefined) return (await source.info.getSignInTheme({})).governing?.theme;
      const r = await source.clients.organizations.getTheme({
        of: { kind: { case: "organization", value: top.id } },
      });
      return r.governing?.theme;
    }
  }
}

/** A key naming `source`, so the theme is read again only when what it names changes. */
function keyOf(source: Source): string {
  return source.kind === "link" ? `${source.entry.kind}/${source.entry.id}` : source.kind;
}

const Worn = createContext<Theme | undefined>(undefined);

/** Wears the theme `source` names on the page while mounted, and gives it to what it holds; its
 * stylesheet only where `stylesheet` says so, never on a screen that holds the viewer's own
 * credentials or a share link's disclosure, where a rule could restyle what the page tells them. */
export function Themed(props: {
  readonly source: Source;
  readonly stylesheet: boolean;
  readonly children: ReactNode;
}) {
  const [theme, setTheme] = useState<Theme>();
  const key = keyOf(props.source);
  const { source, stylesheet } = props;
  useEffect(() => {
    let gone = false;
    read(source).then(
      (t) => {
        if (!gone) setTheme(t);
      },
      () => {
        if (!gone) setTheme(undefined);
      },
    );
    return () => {
      gone = true;
    };
    // The source is read again when what it names changes, not when its object is rebuilt.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);
  // Worn before the browser paints, so no frame shows the new page in the old theme.
  useLayoutEffect(() => {
    wear(document, theme, { stylesheet });
    // A page outside every theme, the device approval among them, wears the panel's own, not
    // what the page before it wore.
    return () => {
      wear(document, undefined, { stylesheet: false });
    };
  }, [theme, stylesheet]);
  return <Worn.Provider value={theme}>{props.children}</Worn.Provider>;
}

/** The theme being worn. */
export function useWorn(): Theme | undefined {
  return useContext(Worn);
}

/** The product's name, and its logo where the theme has one. */
export function Brand() {
  const theme = useWorn();
  const logo = logoSource(theme?.logo);
  return (
    <span className="brand">
      {logo !== undefined && <img src={logo} alt="" height={24} />}
      {productName(theme)}
    </span>
  );
}
