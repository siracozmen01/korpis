// SPDX-License-Identifier: AGPL-3.0-or-later

// What the console shows (§8 of docs/11-surface-web.md, §2 and §3 of docs/14-streams.md): the
// last records of the stream in offset order, each line its runs of styled text, and a gap
// wherever records are missing, whether the node said so with a gap record or the offsets
// simply were not there. Only a window of the newest lines is kept, so a workload printing
// thousands of lines a second cannot take the tab down; what falls out of the window is counted
// and said, never dropped silently. Older records are read on request, and prepended.

import {
  type ConsoleChunk,
  StreamRecordKind,
  type StreamRecord,
} from "../gen/korpis/v1/console_pb";
import { type Run, type Style, runs } from "./ansi";
import { t } from "../i18n/intl";

/** A line of the console: text, a gap, or what someone typed and who. */
export type Line =
  | {
      readonly kind: "text";
      readonly offset: bigint;
      readonly source: string;
      readonly runs: readonly Run[];
    }
  | { readonly kind: "gap"; readonly offset: bigint; readonly says: string }
  | { readonly kind: "input"; readonly offset: bigint; readonly by: string; readonly text: string };

/** The console as shown. */
export interface View {
  readonly lines: readonly Line[];
  /** Lines that fell out of the window since the screen opened. */
  readonly dropped: number;
  /** The first offset the view holds, and the next one it expects. */
  readonly first: bigint | undefined;
  readonly next: bigint | undefined;
  readonly style: Style;
}

/** The most lines kept. */
export const WINDOW = 2000;

export const EMPTY: View = { lines: [], dropped: 0, first: undefined, next: undefined, style: {} };

const decoder = new TextDecoder();

function gapSays(r: StreamRecord): string {
  const g = r.gap;
  if (g === undefined)
    return t({
      defaultMessage: "records were lost here",
      id: "KK2vTsR3",
      description: "a gap in a console",
    });
  const what =
    g.records > 0n ? `${g.records.toString()} records lost` : `${g.bytes.toString()} bytes lost`;
  return g.reason === "" ? what : `${what}: ${g.reason}`;
}

/** The lines of the records of `chunk`, from `style`. */
/** Who typed an input record and what (§6 of docs/14-streams.md): its body is the JSON the
 * stream writer wrote, `{"by", "session", "text"}`; `undefined` for anything else. */
function inputOf(r: StreamRecord): { by: string; text: string } | undefined {
  try {
    const v: unknown = JSON.parse(decoder.decode(r.body));
    if (typeof v !== "object" || v === null) return undefined;
    const { by, text } = v as Record<string, unknown>;
    if (typeof by !== "string" || typeof text !== "string") return undefined;
    return { by, text: text.replace(/\r?\n$/, "") };
  } catch {
    return undefined;
  }
}

function linesOf(chunk: ConsoleChunk, style: Style): { lines: Line[]; style: Style } {
  const lines: Line[] = [];
  const missing = chunk.missing;
  if (missing !== undefined && missing.end > missing.first) {
    const n = missing.end - missing.first;
    lines.push({
      kind: "gap",
      offset: missing.first,
      says: `${n.toString()} records are not held`,
    });
  }
  let current = style;
  for (const r of chunk.records) {
    if (r.kind === StreamRecordKind.GAP) {
      lines.push({ kind: "gap", offset: r.offset, says: gapSays(r) });
      continue;
    }
    if (r.kind === StreamRecordKind.CONTROL) continue;
    const typed = r.source === "input" ? inputOf(r) : undefined;
    if (typed !== undefined) {
      lines.push({ kind: "input", offset: r.offset, ...typed });
      continue;
    }
    const parsed = runs(decoder.decode(r.body), current);
    current = parsed.style;
    lines.push({ kind: "text", offset: r.offset, source: r.source, runs: parsed.runs });
  }
  return { lines, style: current };
}

/** The view after a chunk read live, at the end; the oldest lines leave the window, counted. */
export function append(view: View, chunk: ConsoleChunk): View {
  const { lines, style } = linesOf(chunk, view.style);
  const all = [...view.lines, ...lines];
  const over = Math.max(0, all.length - WINDOW);
  const kept = over > 0 ? all.slice(over) : all;
  return {
    lines: kept,
    dropped: view.dropped + over,
    first: kept[0]?.offset ?? view.first,
    next: chunk.nextOffset > 0n ? chunk.nextOffset : view.next,
    style,
  };
}

/** The view with older records, read on request, before what it holds. */
export function prepend(view: View, chunk: ConsoleChunk): View {
  const { lines } = linesOf(chunk, {});
  const before =
    view.first === undefined ? lines : lines.filter((l) => l.offset < (view.first ?? 0n));
  const all = [...before, ...view.lines];
  return {
    ...view,
    lines: all.slice(0, Math.max(WINDOW, view.lines.length)),
    first: all[0]?.offset ?? view.first,
  };
}
