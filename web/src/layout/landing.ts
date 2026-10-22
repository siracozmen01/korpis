// SPDX-License-Identifier: AGPL-3.0-or-later

// The layout a signed-in viewer's landing screen shows (question 2 of §10 of
// docs/11-surface-web.md): that of whoever provides their access, as their theme is
// (../theme/useTheme.tsx), which the control plane names from the grants it holds (Finding 134
// of docs/23-walkthroughs.md); and the top organization the viewer can read, whose quota the
// landing shows where its layout places it.

import type { Clients } from "../clients";
import type { Layout, LayoutItem, Organization } from "../gen/korpis/v1/tenancy_pb";
import { readable, topOf } from "../theme/useTheme";

/** What the landing shows: the layout, none for the panel's own, and the viewer's top
 * organization, none where no one organization is. */
export interface Landing {
  readonly layout: Layout | undefined;
  readonly top: Organization | undefined;
}

/** The landing of `clients`' caller. */
export async function landingOf(clients: Clients): Promise<Landing> {
  const [orgs, landing] = await Promise.all([
    readable(clients),
    clients.organizations.getLandingLayout({}),
  ]);
  return { layout: landing.governing?.layout, top: topOf(orgs) };
}

/** The host a link goes to, shown beside its label; none unless it is an `https` address with
 * a host and no user information, the control plane's own rule, held here as well so that no
 * other scheme, `javascript:` among them, could ever reach an `href` the panel writes. */
export function hostOf(url: string): string | undefined {
  try {
    const u = new URL(url);
    const plain = u.protocol === "https:" && u.username === "" && u.password === "";
    return plain && u.host !== "" ? u.host : undefined;
  } catch {
    return undefined;
  }
}

/** A key for an item, unique among a layout's: its kind and place. */
export function keyOf(item: LayoutItem, at: number): string {
  return `${item.kind.case ?? "unknown"}:${String(at)}`;
}
