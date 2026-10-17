// SPDX-License-Identifier: AGPL-3.0-or-later

// Wearing an organization's theme (§7 of docs/11-surface-web.md): its colours replace the panel's
// tokens, its stylesheet follows the panel's own, its name and logo stand in the chrome. Both
// sheets are constructed in the page rather than loaded or inlined, so the page's policy keeps
// `style-src 'self'`; a constructed sheet ignores `@import`, which the control plane refused
// anyway, and what its rules load is still fenced by `img-src`, `font-src`, and `connect-src`.
// Nothing a theme says is trusted twice: a colour is used only if it is `#rrggbb`, a logo only if
// it is one of the three image types, whatever the control plane let through.

import type { Logo, Theme, ThemeColors } from "../gen/korpis/v1/tenancy_pb";

/** Each token a theme may set, and the custom property the panel's stylesheet reads it from. */
const PROPERTY: Record<keyof Omit<ThemeColors, "$typeName" | "$unknown">, string> = {
  background: "--bg",
  text: "--fg",
  muted: "--muted",
  line: "--line",
  accent: "--accent",
  accentText: "--accent-fg",
  danger: "--danger",
  surface: "--surface",
  focus: "--focus",
};

const HEX = /^#[0-9a-fA-F]{6}$/;

/** The declarations `colours` sets, each a token of the panel's with a `#rrggbb` value. */
function declarations(colours: ThemeColors | undefined): string {
  if (colours === undefined) return "";
  return Object.entries(PROPERTY)
    .map(([token, property]) => {
      const value = colours[token as keyof typeof PROPERTY];
      return HEX.test(value) ? `${property}: ${value};` : "";
    })
    .filter((d) => d !== "")
    .join(" ");
}

/** The sheet of `theme`'s colours: the light scheme on the root, the dark in its media query. */
export function tokenSheet(theme: Theme | undefined): string {
  const light = declarations(theme?.light);
  const dark = declarations(theme?.dark);
  const rules: string[] = [];
  if (light !== "") rules.push(`:root { ${light} }`);
  if (dark !== "") rules.push(`@media (prefers-color-scheme: dark) { :root { ${dark} } }`);
  return rules.join("\n");
}

const LOGO_TYPES = new Set(["image/png", "image/webp", "image/svg+xml"]);

/** `logo` as an address an image element shows, or none. An image element runs no script an SVG
 * holds, and the page's policy admits `data:` for images alone. */
export function logoSource(logo: Logo | undefined): string | undefined {
  if (logo === undefined || !LOGO_TYPES.has(logo.mediaType) || logo.data.length === 0) return;
  let binary = "";
  for (const b of logo.data) binary += String.fromCharCode(b);
  return `data:${logo.mediaType};base64,${btoa(binary)}`;
}

/** The name the panel shows for itself under `theme`. */
export function productName(theme: Theme | undefined): string {
  return theme?.productName !== undefined && theme.productName !== ""
    ? theme.productName
    : "Korpis";
}

/** The sheets the page wears for the theme, kept so the next theme replaces them. */
let worn: CSSStyleSheet[] = [];

/** Wears `theme` on `doc`, replacing whatever theme it wore; none wears the panel's own. Its
 * stylesheet is worn only where `stylesheet` is set: a rule can hide, move, or replace any text
 * the panel shows, which a colour checked for contrast cannot. Where the browser cannot construct
 * a sheet, the colours and stylesheet are not applied and the panel's own show, never an inline
 * style the policy would refuse. */
export function wear(
  doc: Document,
  theme: Theme | undefined,
  { stylesheet }: { readonly stylesheet: boolean },
): void {
  doc.title = productName(theme);
  const adopted = (doc as { adoptedStyleSheets?: CSSStyleSheet[] }).adoptedStyleSheets;
  const constructs =
    typeof CSSStyleSheet !== "undefined" && "replaceSync" in CSSStyleSheet.prototype;
  if (adopted === undefined || !constructs) return;
  const kept = adopted.filter((s) => !worn.includes(s));
  worn = [];
  const texts = theme === undefined ? [] : [tokenSheet(theme), stylesheet ? theme.stylesheet : ""];
  for (const text of texts) {
    if (text === "") continue;
    const sheet = new CSSStyleSheet();
    sheet.replaceSync(text);
    worn.push(sheet);
  }
  doc.adoptedStyleSheets = [...kept, ...worn];
}
