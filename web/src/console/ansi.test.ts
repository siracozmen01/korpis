// SPDX-License-Identifier: AGPL-3.0-or-later

import { runs } from "./ansi";

test("colours and weights become classes from a fixed set", () => {
  expect(runs("\u001b[1;31mERROR\u001b[0m ok").runs).toEqual([
    { text: "ERROR", classes: "ansi-fg-1 ansi-bold" },
    { text: " ok", classes: "" },
  ]);
  expect(runs("\u001b[92mbright\u001b[39m").runs).toEqual([
    { text: "bright", classes: "ansi-fg-10" },
  ]);
});

test("every other sequence and control character is dropped, never acted on", () => {
  const hostile =
    "\u001b]0;pwned title\u0007" + // set the window title
    "\u001b]8;;https://evil.example\u001b\\click\u001b]8;;\u001b\\" + // a hyperlink
    "\u001b]52;c;Y2xpcA==\u0007" + // write the clipboard
    "\u001b[2J\u001b[H" + // clear and home
    "a\u0008b\u0007c\td\n";
  expect(
    runs(hostile)
      .runs.map((r) => r.text)
      .join(""),
  ).toBe("clickabc\td\n");
});

test("an extended colour's numbers are not read as other attributes", () => {
  expect(runs("\u001b[38;5;1mx").runs).toEqual([{ text: "x", classes: "" }]);
  expect(runs("\u001b[38;2;1;4;3mx").runs).toEqual([{ text: "x", classes: "" }]);
});

test("a style carries from one record to the next", () => {
  const first = runs("\u001b[33mwarn");
  expect(runs(" more", first.style).runs).toEqual([{ text: " more", classes: "ansi-fg-3" }]);
});
