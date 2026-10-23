// SPDX-License-Identifier: AGPL-3.0-or-later

// Metering's figures as the panel shows them (§2 of docs/15-observability.md, §8 of
// docs/11-surface-web.md): each in its unit, and what a window's records come to with what they
// leave out, since a sum over a window that was not heard throughout is a sum of part of it. A
// quantity nothing measured stays absent; it is never shown as a zero.

import { timestampMs } from "@bufbuild/protobuf/wkt";
import { type UsageRecord, UsageKind } from "../gen/korpis/v1/metering_pb";
import { bytes } from "../i18n/format";
import { intl, t } from "../i18n/intl";

/** CPU time, in microseconds, as seconds. */
export function cpuSeconds(usec: bigint): string {
  const seconds = intl.formatNumber(Number(usec) / 1e6, { maximumFractionDigits: 3 });
  return t({ defaultMessage: "{seconds} s", id: "vNzwC4D0", description: "CPU time" }, { seconds });
}

/** Memory held over time, in KiB-seconds, as GiB-hours: what a GiB held for an hour is. */
export function gibHours(kibSeconds: bigint): string {
  const hours = intl.formatNumber(Number(kibSeconds) / (1024 * 1024 * 3600), {
    maximumFractionDigits: 3,
  });
  return t(
    { defaultMessage: "{hours} GiB-hours", id: "sgfAtMMm", description: "memory held over time" },
    { hours },
  );
}

/** CPU in use, in thousandths of a core, as cores. */
export function cores(millicores: bigint): string {
  const n = Number(millicores) / 1000;
  const count = intl.formatNumber(n, { maximumFractionDigits: 2 });
  return t(
    {
      defaultMessage: "{n, plural, one {{count} core} other {{count} cores}}",
      id: "m19Hb+VP",
      description: "CPU in use",
    },
    { n, count },
  );
}

/** A part of a window, in thousandths, as a percentage. */
export function permille(n: bigint): string {
  return intl.formatNumber(Number(n) / 1000, { style: "percent", maximumFractionDigits: 1 });
}

/** Bytes a second. */
export function perSecond(n: bigint): string {
  return t(
    { defaultMessage: "{size}/s", id: "TFzi1qzw", description: "bytes a second" },
    { size: bytes(n) },
  );
}

/** What a window's records come to. */
export interface Totals {
  readonly cpuUsec: bigint | undefined;
  readonly cpuThrottledUsec: bigint | undefined;
  readonly memoryKibSeconds: bigint | undefined;
  readonly memoryPeakBytes: bigint | undefined;
  readonly ioReadBytes: bigint | undefined;
  readonly ioWriteBytes: bigint | undefined;
  readonly netRxBytes: bigint | undefined;
  readonly netTxBytes: bigint | undefined;
  /** What left the workload's link for anything but another workload's on its node. */
  readonly egressBytes: bigint | undefined;
  /** What each endpoint whose accounting is on carried, by name. */
  readonly endpoints: ReadonlyMap<string, EndpointTotal>;
  /** The disk the latest record measured, when its interval ended, and whether it counted what
   * a guest's block devices have allocated rather than what filesystems hold. */
  readonly disk:
    { readonly bytes: bigint; readonly at: number; readonly allocated: boolean } | undefined;
  /** How many records were measured, adjustments among them. */
  readonly measured: number;
  readonly adjustments: number;
  /** How many records are gaps, and the milliseconds they span. */
  readonly gaps: number;
  readonly gapMs: number;
}

/** What one endpoint carried over a window. */
export interface EndpointTotal {
  readonly receivedBytes: bigint;
  readonly sentBytes: bigint;
}

const sum = (a: bigint | undefined, b: bigint | undefined) => (b === undefined ? a : (a ?? 0n) + b);
const max = (a: bigint | undefined, b: bigint | undefined) =>
  b === undefined ? a : a === undefined || b > a ? b : a;

/** What `records` come to: each quantity summed where any measured it, the peak the highest,
 * the disk the latest, and the measured and unheard counted apart. */
export function totals(records: readonly UsageRecord[]): Totals {
  let acc: Totals = {
    cpuUsec: undefined,
    cpuThrottledUsec: undefined,
    memoryKibSeconds: undefined,
    memoryPeakBytes: undefined,
    ioReadBytes: undefined,
    ioWriteBytes: undefined,
    netRxBytes: undefined,
    netTxBytes: undefined,
    egressBytes: undefined,
    endpoints: new Map(),
    disk: undefined,
    measured: 0,
    adjustments: 0,
    gaps: 0,
    gapMs: 0,
  };
  for (const r of records) {
    const span = spanMs(r);
    if (r.kind === UsageKind.GAP) {
      acc = { ...acc, gaps: acc.gaps + 1, gapMs: acc.gapMs + span.to - span.from };
      continue;
    }
    const endpoints = new Map(acc.endpoints);
    for (const e of r.endpoints) {
      const before = endpoints.get(e.endpoint) ?? { receivedBytes: 0n, sentBytes: 0n };
      endpoints.set(e.endpoint, {
        receivedBytes: before.receivedBytes + e.receivedBytes,
        sentBytes: before.sentBytes + e.sentBytes,
      });
    }
    const later = r.diskBytes !== undefined && (acc.disk === undefined || span.to >= acc.disk.at);
    acc = {
      ...acc,
      cpuUsec: sum(acc.cpuUsec, r.cpuUsec),
      cpuThrottledUsec: sum(acc.cpuThrottledUsec, r.cpuThrottledUsec),
      memoryKibSeconds: sum(acc.memoryKibSeconds, r.memoryKibSeconds),
      memoryPeakBytes: max(acc.memoryPeakBytes, r.memoryPeakBytes),
      ioReadBytes: sum(acc.ioReadBytes, r.ioReadBytes),
      ioWriteBytes: sum(acc.ioWriteBytes, r.ioWriteBytes),
      netRxBytes: sum(acc.netRxBytes, r.netRxBytes),
      netTxBytes: sum(acc.netTxBytes, r.netTxBytes),
      egressBytes: sum(acc.egressBytes, r.egressBytes),
      endpoints,
      disk:
        later && r.diskBytes !== undefined
          ? { bytes: r.diskBytes, at: span.to, allocated: r.diskAllocated }
          : acc.disk,
      measured: acc.measured + 1,
      adjustments: acc.adjustments + (r.kind === UsageKind.ADJUSTMENT ? 1 : 0),
    };
  }
  return acc;
}

/** The window a record covers, in milliseconds since the epoch. */
export function spanMs(r: UsageRecord): { readonly from: number; readonly to: number } {
  return {
    from: r.from === undefined ? 0 : timestampMs(r.from),
    to: r.to === undefined ? 0 : timestampMs(r.to),
  };
}
