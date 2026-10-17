// SPDX-License-Identifier: AGPL-3.0-or-later

// Where the navigation starts (§5 of docs/11-surface-web.md): at the objects the caller's own
// grants are anchored on, not at a tree it is shown and refused parts of. An organization grant
// opens that organization and what is inside it, a project grant that project, a workload grant
// that workload; a selector grant opens the object it selects within. A grant on anything else
// (a volume, a domain, a node) is not a place to navigate to, and is not shown as one.

import { type Grant, GrantState, type Scope } from "../gen/korpis/v1/authority_pb";

/** A place the navigation can open. */
export interface Entry {
  readonly kind: "organization" | "project" | "workload";
  readonly id: string;
}

/** The place a scope opens, if it is one. */
export function entryOf(scope: Scope | undefined): Entry | undefined {
  const kind = scope?.kind;
  switch (kind?.case) {
    case "organization":
    case "project":
    case "workload":
      return { kind: kind.case, id: kind.value };
    case "selector":
      return entryOf(kind.value.within);
    default:
      return undefined;
  }
}

/** The places `grants` open, each once, in the order the grants came; a grant that no longer
 * works (expired, used up, revoked, suspended) opens nothing. */
export function entries(grants: readonly Grant[]): Entry[] {
  const seen = new Set<string>();
  const found: Entry[] = [];
  for (const grant of grants) {
    if (grant.state !== GrantState.ISSUED) continue;
    const entry = entryOf(grant.scope);
    if (entry === undefined || seen.has(`${entry.kind}:${entry.id}`)) continue;
    seen.add(`${entry.kind}:${entry.id}`);
    found.push(entry);
  }
  return found;
}

/** The panel's path of `entry`, under `base`. */
export function pathOf(base: string, entry: Entry): string {
  const letter = { organization: "o", project: "p", workload: "w" }[entry.kind];
  return `${base}/${letter}/${entry.id}`;
}
