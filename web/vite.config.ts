// SPDX-License-Identifier: AGPL-3.0-or-later

// The panel's build (§9 of docs/21-stack.md): static files with hashed names, no server runtime.

import react from "@vitejs/plugin-react";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { fileURLToPath } from "node:url";
import { loadEnv, type Plugin } from "vite";
import { defineConfig } from "vitest/config";

/** Where the panel's source is published, as the control plane's build says of its own. */
const SOURCE_URL = "https://github.com/siracozmen01/korpis";

/** The revision this build is made from: `KORPIS_SOURCE_REVISION`, or the checkout's. */
function revision(): string {
  const named = process.env.KORPIS_SOURCE_REVISION;
  if (named !== undefined && named !== "") return named;
  try {
    return execFileSync("git", ["rev-parse", "HEAD"], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
  } catch {
    return "unknown";
  }
}

/**
 * The panel's own source notice (§7 of docs/11-surface-web.md, §2 of docs/19-governance.md):
 * `about/source.json`, which a control plane serving the bundle names on its `/about/source`,
 * and `about/source/index.html`, the same said unstyled for a panel served from anywhere else.
 */
function sourceNotice(): Plugin {
  return {
    name: "korpis-source-notice",
    generateBundle() {
      const rev = revision();
      this.emitFile({
        type: "asset",
        fileName: "about/source.json",
        source: JSON.stringify({ revision: rev, source: SOURCE_URL }) + "\n",
      });
      this.emitFile({
        type: "asset",
        fileName: "about/source/index.html",
        source:
          `<!doctype html>\n<html lang="en"><head><meta charset="utf-8"><title>Source</title>` +
          `</head><body><h1>Source</h1><p>This panel is free software under the GNU Affero ` +
          `General Public License, version 3 or later. It was built from revision ` +
          `<code>${rev}</code> of <a href="${SOURCE_URL}">${SOURCE_URL}</a>.</p></body></html>\n`,
      });
    },
  };
}

/**
 * The control plane and its relays in the panel's own policy: `index.html` lets the page connect
 * to its own origin. When the build names the control plane elsewhere (`VITE_KORPIS_API`) and the
 * relays a console or a file session is opened at (`VITE_KORPIS_RELAYS`, comma-separated), their
 * origins are added and nothing else. A control plane serving the bundle itself writes the relays
 * it names into the policy as it serves the page (§3.1 of docs/02-architecture.md).
 */
function connectPolicy(named: readonly (string | undefined)[]): Plugin {
  const origins = named
    .flatMap((n) => (n ?? "").split(","))
    .map((n) => n.trim())
    .filter((n) => n !== "")
    .map((n) => new URL(n).origin);
  return {
    name: "korpis-connect-policy",
    transformIndexHtml(html) {
      if (origins.length === 0) return html;
      if (!html.includes("connect-src 'self';")) {
        throw new Error("index.html's policy has no connect-src 'self' to extend");
      }
      return html.replace("connect-src 'self';", `connect-src 'self' ${origins.join(" ")};`);
    },
  };
}

/** The libraries the panel is built on, each family in a chunk of its own, so a release that
 * changes only the panel's code leaves a browser's copy of them valid. */
const VENDORS: readonly (readonly [string, RegExp])[] = [
  ["react", /[\\/]node_modules[\\/](react|react-dom|scheduler)[\\/]/],
  [
    "aria",
    /[\\/]node_modules[\\/](react-aria-components|@react-aria|@react-stately|@react-types|@internationalized|@swc)[\\/]/,
  ],
  ["api", /[\\/]node_modules[\\/](@bufbuild|@connectrpc)[\\/]/],
  ["intl", /[\\/]node_modules[\\/](react-intl|@formatjs|intl-messageformat)[\\/]/],
];

/** The chunk a module of a library belongs in; none for the panel's own. */
function vendorChunk(id: string): string | undefined {
  return VENDORS.find(([, pattern]) => pattern.test(id))?.[0];
}

/**
 * The panel's service worker (question 3 of §10 of docs/11-surface-web.md): built from
 * `src/sw/worker.ts` as `sw.js` beside the page, with the files of this bundle written into it and
 * its cache named for them, so a new bundle is a new worker. It is loaded as a classic script, so
 * a worker that would import or export anything fails the build rather than the browser.
 */
function shellWorker(): Plugin {
  return {
    name: "korpis-shell-worker",
    // After the page is written, so the bundle it lists holds it.
    enforce: "post",
    generateBundle(_, bundle) {
      const worker = bundle["sw.js"];
      if (worker?.type !== "chunk") throw new Error("the build made no sw.js");
      const files = Object.keys(bundle)
        .filter((f) => f !== "sw.js" && !f.endsWith(".map") && !f.startsWith("about/"))
        .sort();
      if (!files.includes("index.html")) throw new Error("the bundle has no index.html");
      const version = createHash("sha256").update(files.join("\n")).digest("hex").slice(0, 16);
      const code = worker.code
        .replace('["__KORPIS_SHELL__"]', JSON.stringify(files))
        .replace("__KORPIS_VERSION__", version);
      if (code.includes("__KORPIS_")) throw new Error("sw.js kept a placeholder");
      if (/^\s*(import|export)\b/m.test(code)) throw new Error("sw.js imports or exports");
      worker.code = code;
    },
  };
}

export default defineConfig(({ mode }) => ({
  plugins: [
    react(),
    sourceNotice(),
    shellWorker(),
    connectPolicy(
      ((env) => [env.VITE_KORPIS_API, env.VITE_KORPIS_RELAYS])(
        loadEnv(mode, process.cwd(), "VITE_"),
      ),
    ),
  ],
  build: {
    outDir: "dist",
    assetsDir: "assets",
    sourcemap: true,
    rollupOptions: {
      input: {
        index: fileURLToPath(new URL("index.html", import.meta.url)),
        sw: fileURLToPath(new URL("src/sw/worker.ts", import.meta.url)),
      },
      output: {
        entryFileNames: (chunk) => (chunk.name === "sw" ? "sw.js" : "assets/[name]-[hash].js"),
        manualChunks: vendorChunk,
      },
    },
  },
  test: {
    environment: "jsdom",
    globals: true,
    include: ["src/**/*.test.{ts,tsx}"],
    setupFiles: ["src/test-setup.ts"],
    testTimeout: 15_000,
  },
}));
