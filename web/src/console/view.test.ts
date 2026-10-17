// SPDX-License-Identifier: AGPL-3.0-or-later

import { create, type MessageInitShape } from "@bufbuild/protobuf";
import { ConsoleChunkSchema, StreamRecordKind } from "../gen/korpis/v1/console_pb";
import { EMPTY, WINDOW, append, prepend } from "./view";

const text = (offset: number, body: string) => ({
  offset: BigInt(offset),
  kind: StreamRecordKind.TEXT,
  source: "stdout",
  body: new TextEncoder().encode(body),
});
const chunk = (init: MessageInitShape<typeof ConsoleChunkSchema>) =>
  create(ConsoleChunkSchema, init);

test("missing offsets and gap records are drawn as gaps, never closed up", () => {
  const view = append(
    EMPTY,
    chunk({
      missing: { first: 0n, end: 40n },
      records: [
        text(40, "after the gap"),
        {
          offset: 41n,
          kind: StreamRecordKind.GAP,
          gap: { records: 12n, reason: "write rate exceeded the stream's limit" },
        },
        text(54, "\u001b[31mred"),
      ],
      nextOffset: 55n,
    }),
  );
  expect(
    view.lines.map((l) =>
      l.kind === "text"
        ? l.runs.map((r) => r.text).join("")
        : l.kind === "gap"
          ? `gap: ${l.says}`
          : "",
    ),
  ).toEqual([
    "gap: 40 records are not held",
    "after the gap",
    "gap: 12 records lost: write rate exceeded the stream's limit",
    "red",
  ]);
  expect(view.next).toBe(55n);
});

test("a flood keeps only the window and says how much left it", () => {
  const records = Array.from({ length: WINDOW + 500 }, (_, i) => text(i, `line ${i.toString()}`));
  const view = append(EMPTY, chunk({ records, nextOffset: BigInt(records.length) }));
  expect(view.lines.length).toBe(WINDOW);
  expect(view.dropped).toBe(500);
  expect(view.first).toBe(500n);
});

test("older records read on request go before what is held, once", () => {
  const live = append(
    EMPTY,
    chunk({ records: [text(10, "ten"), text(11, "eleven")], nextOffset: 12n }),
  );
  const older = prepend(
    live,
    chunk({ records: [text(8, "eight"), text(9, "nine"), text(10, "ten")] }),
  );
  expect(older.lines.map((l) => l.offset)).toEqual([8n, 9n, 10n, 11n]);
  expect(older.first).toBe(8n);
});

test("what someone typed is shown with who typed it, never as the record's JSON", () => {
  const typed = (offset: number, body: string) => ({
    offset: BigInt(offset),
    kind: StreamRecordKind.STRUCTURED,
    source: "input",
    body: new TextEncoder().encode(body),
  });
  const view = append(
    EMPTY,
    chunk({
      records: [
        typed(0, '{"by":"user:alex","session":"s1","text":"stop\\n"}'),
        typed(1, "not json"),
      ],
      nextOffset: 2n,
    }),
  );
  expect(view.lines[0]).toEqual({ kind: "input", offset: 0n, by: "user:alex", text: "stop" });
  expect(view.lines[1]?.kind).toBe("text");
});
