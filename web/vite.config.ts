// SPDX-License-Identifier: AGPL-3.0-or-later

// The panel's build (§9 of docs/21-stack.md): static files with hashed names, no server runtime.

import react from "@vitejs/plugin-react";
import { execFileSync } from "node:child_process";
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
 * The control plane in the panel's own policy: `index.html` lets the page connect to its own
 * origin, and, when the build names the control plane elsewhere (`VITE_KORPIS_API`), to that
 * origin too and nothing else, so a panel served apart from its control plane can reach it.
 */
function controlPlanePolicy(named: string | undefined): Plugin {
  const origin = named === undefined || named === "" ? undefined : new URL(named).origin;
  return {
    name: "korpis-control-plane-policy",
    transformIndexHtml(html) {
      if (origin === undefined) return html;
      if (!html.includes("connect-src 'self';")) {
        throw new Error("index.html's policy has no connect-src 'self' to extend");
      }
      return html.replace("connect-src 'self';", `connect-src 'self' ${origin};`);
    },
  };
}

export default defineConfig(({ mode }) => ({
  plugins: [
    react(),
    sourceNotice(),
    controlPlanePolicy(loadEnv(mode, process.cwd(), "VITE_").VITE_KORPIS_API),
  ],
  build: {
    outDir: "dist",
    assetsDir: "assets",
    sourcemap: true,
  },
  test: {
    environment: "jsdom",
    globals: true,
    include: ["src/**/*.test.{ts,tsx}"],
  },
}));
