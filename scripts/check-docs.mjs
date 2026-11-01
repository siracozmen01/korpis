// SPDX-License-Identifier: AGPL-3.0-or-later
// The documentation's rules that a machine can check: every page under 400 lines, a contents list on
// every page over 100, every relative link reaching a file, and every anchor reaching a heading of
// the page it names. External links are not fetched, so a check never depends on the network.
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";

const root = resolve(dirname(new URL(import.meta.url).pathname), "..");
const roots = ["README.md", "README.tr.md", "CONTRIBUTING.md", "SECURITY.md", "GOVERNANCE.md",
    "AGENTS.md", "docs"];

function pages(path) {
    const full = join(root, path);
    if (!existsSync(full)) return [];
    if (!statSync(full).isDirectory()) return path.endsWith(".md") ? [path] : [];
    return readdirSync(full).flatMap((name) => pages(join(path, name)));
}

// GitHub's anchor for a heading: lower case, punctuation but hyphens and underscores dropped,
// spaces turned to hyphens, and a repeated anchor numbered.
function anchors(text) {
    const seen = new Map();
    const out = new Set();
    for (const line of text.split("\n")) {
        const m = /^#{1,6}\s+(.*)$/.exec(line);
        if (!m) continue;
        const base = m[1].trim().toLowerCase().replace(/`/g, "")
            .replace(/[^\p{L}\p{N}\s_-]/gu, "").replace(/\s/g, "-");
        const n = seen.get(base) ?? 0;
        seen.set(base, n + 1);
        out.add(n === 0 ? base : `${base}-${n}`);
    }
    return out;
}

// Links outside code: fenced blocks and inline code spans are not links.
function links(text) {
    const prose = text.replace(/```[\s\S]*?```/g, "").replace(/`[^`\n]*`/g, "");
    return [...prose.matchAll(/\]\(([^)\s]+)\)/g)].map((m) => m[1]);
}

const failures = [];
const cache = new Map();
const read = (path) => {
    if (!cache.has(path)) cache.set(path, readFileSync(join(root, path), "utf8"));
    return cache.get(path);
};

for (const page of roots.flatMap(pages)) {
    const text = read(page);
    const lines = text.split("\n").length - (text.endsWith("\n") ? 1 : 0);
    if (lines > 400) failures.push(`${page}: ${lines} lines, over 400`);
    const contents = /^## Contents$/m.test(text);
    if (page.startsWith("docs/") && lines > 100 && !contents) {
        failures.push(`${page}: ${lines} lines and no "## Contents"`);
    }
    for (const link of links(text)) {
        if (/^[a-z]+:/.test(link)) continue;
        const [target, anchor] = link.split("#");
        const path = target === "" ? page : relative(root, resolve(root, dirname(page), target));
        if (!existsSync(join(root, path))) {
            failures.push(`${page}: links to ${link}, which does not exist`);
            continue;
        }
        if (anchor && path.endsWith(".md") && !anchors(read(path)).has(anchor)) {
            failures.push(`${page}: links to ${link}, and ${path} has no such heading`);
        }
    }
}

if (failures.length > 0) {
    for (const failure of failures) console.log(failure);
    process.exit(1);
}
