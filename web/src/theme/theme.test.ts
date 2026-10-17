// SPDX-License-Identifier: AGPL-3.0-or-later

import { create } from "@bufbuild/protobuf";
import html from "../../index.html?raw";
import { afterEach, expect, test, vi } from "vitest";
import { ThemeSchema } from "../gen/korpis/v1/tenancy_pb";
import { logoSource, productName, tokenSheet, wear } from "./theme";

test("only a #rrggbb colour of a token the panel has reaches the sheet", () => {
  const theme = create(ThemeSchema, {
    light: { accent: "#1f5fbf", text: "red; } body { display: none" },
    dark: { background: "#000000" },
  });
  const sheet = tokenSheet(theme);
  expect(sheet).toContain(":root { --accent: #1f5fbf; }");
  expect(sheet).toContain("@media (prefers-color-scheme: dark) { :root { --bg: #000000; } }");
  expect(sheet).not.toContain("display");
  expect(tokenSheet(undefined)).toBe("");
});

test("a logo is shown only as one of the three image types, and the name falls back", () => {
  const svg = {
    $typeName: "korpis.v1.Logo" as const,
    mediaType: "image/svg+xml",
    data: new TextEncoder().encode("<svg/>"),
  };
  expect(logoSource(svg)).toBe(`data:image/svg+xml;base64,${btoa("<svg/>")}`);
  expect(logoSource({ ...svg, mediaType: "text/html" })).toBeUndefined();
  expect(productName(create(ThemeSchema, { productName: "Acme" }))).toBe("Acme");
  expect(productName(create(ThemeSchema, {}))).toBe("Korpis");
});

test("the page's policy runs only its own scripts and loads its own styles", () => {
  const policy = /Content-Security-Policy"\s+content="([^"]+)"/.exec(html)?.[1] ?? "";
  const directives = new Map(
    policy.split(";").map((d) => {
      const [name = "", ...values] = d.trim().split(/\s+/);
      return [name, values.join(" ")];
    }),
  );
  expect(directives.get("script-src")).toBe("'self'");
  expect(directives.get("style-src")).toBe("'self'");
  expect(directives.get("img-src")).toBe("'self' data:");
  expect(directives.get("object-src")).toBe("'none'");
  expect(policy).not.toContain("unsafe");
});

/** A constructed sheet that remembers its text, where the test's browser has none. */
class Sheet {
  text = "";
  replaceSync(text: string) {
    this.text = text;
  }
}

afterEach(() => {
  vi.unstubAllGlobals();
});

test("a theme's stylesheet is worn only where the page allows it; its colours always", () => {
  vi.stubGlobal("CSSStyleSheet", Sheet);
  const doc = { title: "", adoptedStyleSheets: [] as unknown as CSSStyleSheet[] };
  const theme = create(ThemeSchema, {
    productName: "Acme",
    light: { accent: "#1f5fbf" },
    stylesheet: "main { display: none }",
  });
  const texts = () => doc.adoptedStyleSheets.map((s) => (s as unknown as Sheet).text);
  wear(doc as unknown as Document, theme, { stylesheet: true });
  expect(texts()).toEqual([":root { --accent: #1f5fbf; }", "main { display: none }"]);
  wear(doc as unknown as Document, theme, { stylesheet: false });
  expect(texts(), "the last theme's sheets are replaced").toEqual([":root { --accent: #1f5fbf; }"]);
  expect(doc.title).toBe("Acme");
});
