// SPDX-License-Identifier: AGPL-3.0-or-later

import { expect, test } from "vitest";
import { intlFor, negotiate, SOURCE, t } from "./intl";

test("the locale is the first preference a catalogue speaks, else English", () => {
  expect(negotiate(["tr-TR", "en"], ["en", "tr"])).toBe("tr");
  expect(negotiate(["pt-br"], ["en", "pt-BR", "pt"])).toBe("pt-BR");
  expect(negotiate(["fr", "de"], ["en", "de"])).toBe("de");
  expect(negotiate(["ja"], ["en"])).toBe(SOURCE);
  expect(negotiate([], ["en"])).toBe(SOURCE);
});

test("a message is shown translated, else in English, never as its identifier", () => {
  const save = { id: "save", defaultMessage: "Save" };
  const files = { id: "files", defaultMessage: "{n, plural, one {# file} other {# files}}" };
  const tr = intlFor("tr", { save: "Kaydet", files: "{n, plural, other {# dosya" });
  expect(tr.formatMessage(save)).toBe("Kaydet");
  expect(tr.formatMessage(files, { n: 2 }), "a translation that does not parse").toBe("2 files");
  expect(tr.formatMessage({ id: "gone", defaultMessage: "Gone" })).toBe("Gone");
  expect(t(files, { n: 1 })).toBe("1 file");
});
