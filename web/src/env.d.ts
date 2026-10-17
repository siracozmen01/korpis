// SPDX-License-Identifier: AGPL-3.0-or-later

// What the build may be told (`vite build` reads `VITE_` variables from the environment).

interface ImportMetaEnv {
  /** The control plane, when the panel is served from another origin (§1 of
   * docs/11-surface-web.md); unset, the panel calls the origin it was served from. */
  readonly VITE_KORPIS_API?: string;
  /** The relays sessions are opened at, comma-separated, for a panel served apart from its
   * control plane; they are named in the page's policy (§3.1 of docs/02-architecture.md). */
  readonly VITE_KORPIS_RELAYS?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
