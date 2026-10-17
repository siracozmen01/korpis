// SPDX-License-Identifier: AGPL-3.0-or-later
// The panel's catalogues against its source (question 4 of §10 of docs/11-surface-web.md):
// `web/src/i18n/en.json` must be what extracting the source writes, so no message is missing
// from it and none it holds is unused; and each translation in `web/src/i18n/catalogues` may
// name only messages the source has. Whether a translation's ICU parses is `formatjs compile`'s
// to say, which check-web.sh runs. Arguments: the freshly extracted catalogue.
import { readFileSync, readdirSync } from "node:fs";

const [fresh] = process.argv.slice(2);
const read = (path) => JSON.parse(readFileSync(path, "utf8"));
const source = read("src/i18n/en.json");
const extracted = read(fresh);
let failed = false;
const fail = (message) => {
  console.log(message);
  failed = true;
};
for (const id of Object.keys(extracted)) {
  if (!(id in source)) fail(`en.json lacks ${id}: run (cd web && pnpm extract) and commit it`);
  else if (JSON.stringify(source[id]) !== JSON.stringify(extracted[id])) {
    fail(`en.json holds an old ${id}: run (cd web && pnpm extract) and commit it`);
  }
}
for (const id of Object.keys(source)) {
  if (!(id in extracted)) fail(`en.json holds ${id}, which no source uses: run pnpm extract`);
}
for (const file of readdirSync("src/i18n/catalogues").filter((f) => f.endsWith(".json"))) {
  for (const id of Object.keys(read(`src/i18n/catalogues/${file}`))) {
    if (!(id in source)) fail(`catalogues/${file} translates ${id}, which the source lacks`);
  }
}
process.exit(failed ? 1 : 0);
