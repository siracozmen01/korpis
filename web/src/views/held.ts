// SPDX-License-Identifier: AGPL-3.0-or-later

// What the caller's own grants hold (§5 of docs/11-surface-web.md): every one of them, read to
// the last page, and whether one in force holds an action over an organization. The panel offers
// an action only where one does, as a courtesy; the control plane decides.

import type { Clients } from "../clients";
import { type Grant, GrantState } from "../gen/korpis/v1/authority_pb";
import { PAGE_SIZE } from "./Pages";

/** Every grant the caller holds, every page of them. */
export async function everyGrant(clients: Clients): Promise<Grant[]> {
  const held: Grant[] = [];
  let pageToken = "";
  do {
    const r = await clients.grants.listGrants({ page: { pageSize: PAGE_SIZE, pageToken } });
    held.push(...r.grants);
    pageToken = r.page?.nextPageToken ?? "";
  } while (pageToken !== "");
  return held;
}

/** Whether a grant in force among `grants` holds `action` over one of `organizations`. */
export function holds(
  grants: readonly Grant[],
  action: string,
  organizations: readonly string[],
): boolean {
  return grants.some((g) => {
    const scope = g.scope?.kind;
    return (
      g.state === GrantState.ISSUED &&
      g.actions.includes(action) &&
      scope?.case === "organization" &&
      organizations.includes(scope.value)
    );
  });
}

/** `from` and the organizations above it, upward, as far as the caller may read them. */
export async function upward(clients: Clients, from: string | undefined): Promise<string[]> {
  const found: string[] = [];
  let next = from;
  while (next !== undefined && !found.includes(next)) {
    found.push(next);
    try {
      next = (await clients.organizations.getOrganization({ id: next })).organization?.parent;
    } catch {
      break;
    }
  }
  return found;
}
