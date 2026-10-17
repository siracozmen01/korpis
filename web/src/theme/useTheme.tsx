// SPDX-License-Identifier: AGPL-3.0-or-later

// The theme the page wears, chosen by what it shows (§7 of docs/11-surface-web.md): a place wears
// its organization's theme or the nearest ancestor's; the start page and the profile wear the
// first organization the viewer can read; before anyone signs in, the installation root's. A
// theme that cannot be read leaves the panel's own on, which is what a theme's absence means.

import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import type { Client } from "@connectrpc/connect";
import type { Clients } from "../clients";
import type { ServerInfoService } from "../gen/korpis/v1/api_pb";
import type { Theme } from "../gen/korpis/v1/tenancy_pb";
import type { Entry } from "../views/entries";
import { logoSource, productName, wear } from "./theme";

/** Where a theme is read from. */
export type Source =
  | { readonly kind: "place"; readonly clients: Clients; readonly entry: Entry }
  | { readonly kind: "home"; readonly clients: Clients }
  | { readonly kind: "sign-in"; readonly info: Client<typeof ServerInfoService> };

async function read(source: Source): Promise<Theme | undefined> {
  switch (source.kind) {
    case "sign-in":
      return (await source.info.getSignInTheme({})).governing?.theme;
    case "place": {
      const { kind, id } = source.entry;
      const r = await source.clients.organizations.getTheme({
        of: { kind: { case: kind, value: id } },
      });
      return r.governing?.theme;
    }
    case "home": {
      const orgs = await source.clients.organizations.listOrganizations({});
      const first = orgs.organizations[0];
      if (first === undefined) return undefined;
      const r = await source.clients.organizations.getTheme({
        of: { kind: { case: "organization", value: first.id } },
      });
      return r.governing?.theme;
    }
  }
}

/** A key naming `source`, so the theme is read again only when what is shown changes. */
function keyOf(source: Source): string {
  return source.kind === "place" ? `${source.entry.kind}/${source.entry.id}` : source.kind;
}

const Worn = createContext<Theme | undefined>(undefined);

/** Wears the theme `source` names on the page while mounted, and gives it to what it holds. */
export function Themed(props: { readonly source: Source; readonly children: ReactNode }) {
  const [theme, setTheme] = useState<Theme>();
  const key = keyOf(props.source);
  const { source } = props;
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
  useEffect(() => {
    wear(document, theme);
  }, [theme]);
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
