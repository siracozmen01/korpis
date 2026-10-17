// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The supply-chain policy of deny.toml, held for the web client too (§9 of docs/21-stack.md):
// every package the panel or its build depends on carries a licence from the list deny.toml
// allows, read from there so there is one list, or is named in web/licence-exceptions.json with
// the reason. Reads `pnpm licenses list --json` on standard input; exits non-zero on any other.

import { readFileSync } from "node:fs";

const deny = readFileSync(new URL("../deny.toml", import.meta.url), "utf8");
const block = /\nallow = \[([^\]]*)\]/.exec(deny);
if (block === null) {
  console.error("deny.toml has no licence allow list");
  process.exit(1);
}
const allowed = new Set([...block[1].matchAll(/"([^"]+)"/g)].map((m) => m[1]));
const exceptions = JSON.parse(
  readFileSync(new URL("../web/licence-exceptions.json", import.meta.url), "utf8"),
);

/** Whether the SPDX expression `e` is satisfied by the allowed licences. */
function satisfied(e) {
  const expr = e.trim().replace(/^\((.*)\)$/, "$1");
  if (expr.includes(" OR ")) return expr.split(" OR ").some(satisfied);
  if (expr.includes(" AND ")) return expr.split(" AND ").every(satisfied);
  return allowed.has(expr);
}

const listed = JSON.parse(readFileSync(0, "utf8"));
const refused = [];
for (const [licence, packages] of Object.entries(listed)) {
  for (const p of packages) {
    const excepted = exceptions[p.name]?.licence === licence;
    if (!satisfied(licence) && !excepted) refused.push(`${p.name}: ${licence}`);
  }
}
if (refused.length > 0) {
  console.error(`licences outside deny.toml's list:\n${refused.join("\n")}`);
  process.exit(1);
}
console.log("web licences ok");
