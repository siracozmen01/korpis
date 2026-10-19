// SPDX-License-Identifier: AGPL-3.0-or-later

// A quota set as the panel reads and edits it (§5.1 of docs/05-scheduling.md): its dimensions,
// an entry's shape as the labels its two selectors match, what an organization has given below
// of each entry's shape, and the set an edit of it asks for. Pure, so what the screen shows is
// tested apart from the screen.

import { create } from "@bufbuild/protobuf";
import { type LabelSelector, LabelSelectorSchema } from "../gen/korpis/v1/common_pb";
import {
  type QuotaBounds,
  QuotaBoundsSchema,
  type QuotaEntry,
  QuotaEntrySchema,
  type QuotaSet,
  QuotaSetSchema,
} from "../gen/korpis/v1/tenancy_pb";

/** A quota dimension, as a field of `QuotaBounds`. */
export type Dimension =
  | "cpuMillis"
  | "memoryBytes"
  | "diskBytes"
  | "diskReadBytesPerSec"
  | "diskWriteBytesPerSec"
  | "diskReadOpsPerSec"
  | "diskWriteOpsPerSec"
  | "bandwidthBitsPerSec"
  | "workloads"
  | "volumes"
  | "endpoints"
  | "dedicatedIpv4"
  | "nodePorts";

/** Every dimension, with its name on the wire, which overcommit is keyed by. */
export const DIMENSIONS: readonly (readonly [Dimension, string])[] = [
  ["cpuMillis", "cpu_millis"],
  ["memoryBytes", "memory_bytes"],
  ["diskBytes", "disk_bytes"],
  ["diskReadBytesPerSec", "disk_read_bytes_per_sec"],
  ["diskWriteBytesPerSec", "disk_write_bytes_per_sec"],
  ["diskReadOpsPerSec", "disk_read_ops_per_sec"],
  ["diskWriteOpsPerSec", "disk_write_ops_per_sec"],
  ["bandwidthBitsPerSec", "bandwidth_bits_per_sec"],
  ["workloads", "workloads"],
  ["volumes", "volumes"],
  ["endpoints", "endpoints"],
  ["dedicatedIpv4", "dedicated_ipv4"],
  ["nodePorts", "node_ports"],
];

/** The dimensions counted in bytes, shown as sizes. */
export const IN_BYTES: ReadonlySet<Dimension> = new Set(["memoryBytes", "diskBytes"]);

/** The labels a selector matches, as `key=value` pairs joined by commas; empty for none. */
export function labelsText(s: LabelSelector | undefined): string {
  return Object.entries(s?.matchLabels ?? {})
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([k, v]) => `${k}=${v}`)
    .join(",");
}

/** The labels `text` names, `key=value` pairs separated by commas; undefined if it is not
 * that. Empty text names none. */
export function parseLabels(text: string): Record<string, string> | undefined {
  const labels: Record<string, string> = {};
  for (const part of text
    .split(",")
    .map((p) => p.trim())
    .filter((p) => p !== "")) {
    const at = part.indexOf("=");
    const key = part.slice(0, at).trim();
    if (at <= 0 || key === "") return undefined;
    labels[key] = part.slice(at + 1).trim();
  }
  return labels;
}

/** Whether two entries count the same workloads on the same nodes: one shape. Expressions,
 * which the panel does not edit, count too. */
export function sameShape(a: QuotaEntry, b: QuotaEntry): boolean {
  const expressions = (s: LabelSelector | undefined) => JSON.stringify(s?.matchExpressions ?? []);
  return (
    labelsText(a.selector) === labelsText(b.selector) &&
    labelsText(a.nodes) === labelsText(b.nodes) &&
    expressions(a.selector) === expressions(b.selector) &&
    expressions(a.nodes) === expressions(b.nodes) &&
    (a.selector === undefined) === (b.selector === undefined) &&
    (a.nodes === undefined) === (b.nodes === undefined)
  );
}

/** What `children` were given of `entry`'s shape: the reservations and the limits, summed. */
export function givenBelow(
  entry: QuotaEntry,
  children: readonly QuotaSet[],
): { reserved: Map<Dimension, bigint>; sold: Map<Dimension, bigint> } {
  const reserved = new Map<Dimension, bigint>();
  const sold = new Map<Dimension, bigint>();
  const add = (into: Map<Dimension, bigint>, bounds: QuotaBounds | undefined) => {
    for (const [d] of DIMENSIONS) {
      const n = bounds?.[d];
      if (n !== undefined) into.set(d, (into.get(d) ?? 0n) + n);
    }
  };
  for (const set of children) {
    for (const theirs of set.entries.filter((e) => sameShape(e, entry))) {
      add(reserved, theirs.reservationBounds);
      add(sold, theirs.limitBounds);
    }
  }
  return { reserved, sold };
}

/** What `entry`'s limit in `d` lets the organization sell below, with the overcommit `set`
 * permits; undefined where its limit is unbounded. */
export function maySell(set: QuotaSet, entry: QuotaEntry, d: Dimension): bigint | undefined {
  const limit = entry.limitBounds?.[d];
  if (limit === undefined) return undefined;
  const wire = DIMENSIONS.find(([field]) => field === d)?.[1] ?? "";
  const percent = BigInt(set.childOvercommitPercent[wire] ?? 100);
  return (limit * percent) / 100n;
}

/** One entry as an edit holds it: its shape as label text, and each bound as text, empty for
 * unbounded. The selectors it was read with are kept, so the label expressions the panel does
 * not edit, and a selector present with nothing in it, are written back as they were. */
export interface DraftEntry {
  readonly workloads: string;
  readonly nodes: string;
  readonly read?: { readonly selector?: LabelSelector; readonly nodes?: LabelSelector };
  readonly reserved: Readonly<Partial<Record<Dimension, string>>>;
  readonly sold: Readonly<Partial<Record<Dimension, string>>>;
}

/** A set as an edit holds it. */
export interface Draft {
  readonly entries: readonly DraftEntry[];
  readonly overcommit: Readonly<Record<string, string>>;
}

/** The edit of `set`. */
export function draftOf(set: QuotaSet | undefined): Draft {
  const text = (b: QuotaBounds | undefined) =>
    Object.fromEntries(
      DIMENSIONS.flatMap(([d]) => (b?.[d] === undefined ? [] : [[d, String(b[d])]])),
    ) as Partial<Record<Dimension, string>>;
  return {
    entries: (set?.entries ?? []).map((e) => ({
      workloads: e.selector === undefined ? "" : labelsText(e.selector),
      nodes: e.nodes === undefined ? "" : labelsText(e.nodes),
      reserved: text(e.reservationBounds),
      sold: text(e.limitBounds),
      read: {
        ...(e.selector === undefined ? {} : { selector: e.selector }),
        ...(e.nodes === undefined ? {} : { nodes: e.nodes }),
      },
    })),
    overcommit: Object.fromEntries(
      Object.entries(set?.childOvercommitPercent ?? {}).map(([d, p]) => [d, String(p)]),
    ),
  };
}

/** Why an edit cannot be asked for: the field it names. */
export class DraftError extends Error {
  constructor(readonly field: string) {
    super(field);
  }
}

function bound(text: string | undefined, field: string): bigint | undefined {
  const t = (text ?? "").trim();
  if (t === "") return undefined;
  if (!/^\d+$/.test(t)) throw new DraftError(field);
  return BigInt(t);
}

/** The selector `text` names, keeping the expressions of the one it was read as and, while its
 * labels are unchanged, that selector itself. */
function selector(
  text: string,
  read: LabelSelector | undefined,
  field: string,
): LabelSelector | undefined {
  if (read !== undefined && text.trim() === labelsText(read)) return read;
  const matchExpressions = read?.matchExpressions ?? [];
  if (text.trim() === "" && matchExpressions.length === 0) return undefined;
  const matchLabels = parseLabels(text);
  if (matchLabels === undefined) throw new DraftError(field);
  return create(LabelSelectorSchema, { matchLabels, matchExpressions });
}

/** The set `draft` asks for; throws `DraftError` naming the first field that is not a number
 * or not labels. */
export function setOf(draft: Draft): QuotaSet {
  const entries = draft.entries.map((e, i) => {
    const bounds = (side: "reserved" | "sold") =>
      create(
        QuotaBoundsSchema,
        Object.fromEntries(
          DIMENSIONS.flatMap(([d]) => {
            const n = bound(e[side][d], `${String(i)}.${side}.${d}`);
            return n === undefined ? [] : [[d, n]];
          }),
        ),
      );
    return create(QuotaEntrySchema, {
      selector: selector(e.workloads, e.read?.selector, `${String(i)}.workloads`),
      nodes: selector(e.nodes, e.read?.nodes, `${String(i)}.nodes`),
      reservationBounds: bounds("reserved"),
      limitBounds: bounds("sold"),
    });
  });
  const childOvercommitPercent: Record<string, number> = {};
  for (const [d, text] of Object.entries(draft.overcommit)) {
    const p = bound(text, `overcommit.${d}`);
    if (p !== undefined) childOvercommitPercent[d] = Number(p);
  }
  return create(QuotaSetSchema, { entries, childOvercommitPercent });
}
