// SPDX-License-Identifier: AGPL-3.0-or-later

// A workload's usage (§8 of docs/11-surface-web.md, §2 of docs/15-observability.md): what it
// uses now, as its node read it moments ago, beside what was accounted over the last day, the
// figures it is billed on. Each says where it came from, over what window, and how it was
// measured, and the two are shown side by side, never one reconciled to the other: a reading is
// ten seconds of the node's meter, a record an interval of its counters differenced. A span the
// control plane did not hear is shown as unheard, and a sum over the day says it leaves it out.

import { timestampMs } from "@bufbuild/protobuf/wkt";
import { type ReactNode, useEffect, useState } from "react";
import { useReach } from "../clients";
import type { UsageNow, UsageRecord } from "../gen/korpis/v1/metering_pb";
import { bytes } from "../i18n/format";
import { intl, t } from "../i18n/intl";
import { refusal } from "../views/refusal";
import { Intervals } from "./Intervals";
import {
  type EndpointTotal,
  cores,
  coreHours,
  cpuSeconds,
  deviceHours,
  gibHours,
  perSecond,
  permille,
  totals,
} from "./figures";
import { useUsageNow } from "./useUsageNow";

/** The window accounted: the last day. */
const DAY_MS = 86_400_000;
/** The most records read for it: a day of one-minute intervals. */
const MOST = 1440;

function at(ms: number): string {
  return intl.formatDate(new Date(ms), { dateStyle: "medium", timeStyle: "medium" });
}

/** Rows of a figure list, an absent figure left out. */
function Figures({ rows }: { readonly rows: readonly [string, string | undefined][] }) {
  const shown = rows.filter((r): r is [string, string] => r[1] !== undefined);
  return (
    <dl>
      {shown.map(([name, value]) => (
        <div key={name}>
          <dt>{name}</dt>
          <dd>{value}</dd>
        </div>
      ))}
    </dl>
  );
}

const NAMES = {
  cpu: () => t({ defaultMessage: "CPU", id: "nMyqQDPP", description: "a usage figure" }),
  throttled: () =>
    t({
      defaultMessage: "Throttled",
      id: "l+Q2Uzc5",
      description: "a usage figure: CPU throttled",
    }),
  memory: () => t({ defaultMessage: "Memory", id: "Dr+dcwVH", description: "a usage figure" }),
  peak: () => t({ defaultMessage: "Peak memory", id: "eDZcmQn6", description: "a usage figure" }),
  read: () => t({ defaultMessage: "Block read", id: "HCO4YTa2", description: "a usage figure" }),
  written: () =>
    t({ defaultMessage: "Block written", id: "TlViedNL", description: "a usage figure" }),
  received: () =>
    t({ defaultMessage: "Network received", id: "FqyUjrsq", description: "a usage figure" }),
  sent: () => t({ defaultMessage: "Network sent", id: "RpzP/389", description: "a usage figure" }),
  egress: () =>
    t({
      defaultMessage: "Egress",
      id: "6vnOVE2b",
      description: "a usage figure: bytes sent beyond the node",
    }),
  disk: () => t({ defaultMessage: "Disk", id: "UYHJnj7x", description: "a usage figure" }),
  cores: () =>
    t({
      defaultMessage: "Dedicated cores",
      id: "2HoDOHU6",
      description: "a usage figure: dedicated cores held",
    }),
  devices: () =>
    t({
      defaultMessage: "Devices",
      id: "/tS7AnHp",
      description: "a usage figure: devices held",
    }),
};

const map = <T, U>(v: T | undefined, f: (v: T) => U) => (v === undefined ? undefined : f(v));

/** What the workload's node read of it last. */
function Now({ id }: { readonly id: string }) {
  const { clients, relay, metered } = useReach();
  const u = useUsageNow({ clients, relay, workload: id, automatic: !metered });
  const now: UsageNow | undefined = u.now;
  const heading = t({
    defaultMessage: "Now",
    id: "/IQrq2KV",
    description: "usage as the node reads it",
  });
  let body: ReactNode;
  if (now === undefined) {
    body = (
      <p role="status">
        {u.ended ?? t({ defaultMessage: "Waiting for the node's next reading…", id: "C1xXK205" })}
      </p>
    );
  } else {
    const read = now.at === undefined ? 0 : timestampMs(now.at);
    const since = now.since === undefined ? undefined : timestampMs(now.since);
    body = (
      <>
        <Figures
          rows={[
            [NAMES.memory(), map(now.memoryBytes, bytes)],
            [NAMES.cpu(), map(now.cpuMillicores, cores)],
            [NAMES.throttled(), map(now.cpuThrottledPermille, permille)],
            [NAMES.read(), map(now.ioReadBytesPerSecond, perSecond)],
            [NAMES.written(), map(now.ioWriteBytesPerSecond, perSecond)],
            [NAMES.received(), map(now.netRxBytesPerSecond, perSecond)],
            [NAMES.sent(), map(now.netTxBytesPerSecond, perSecond)],
          ]}
        />
        <p className="aside">
          {since === undefined
            ? t(
                {
                  defaultMessage:
                    "Read by the workload's node at {at}: memory in use then. Rates follow from its next reading.",
                  id: "+METW+EQ",
                },
                { at: at(read) },
              )
            : t(
                {
                  defaultMessage:
                    "Read by the workload's node at {at}, by its clock: memory in use then, and each rate over the {seconds, plural, one {# second} other {# seconds}} before. Not billed, and not the figure below.",
                  id: "+uBQ8wO1",
                },
                { at: at(read), seconds: Math.round((read - since) / 1000) },
              )}
        </p>
        {u.ended !== undefined && <p role="status">{u.ended}</p>}
      </>
    );
  }
  return (
    <section aria-label={heading}>
      <h3>{heading}</h3>
      {body}
    </section>
  );
}

/** What each endpoint whose accounting is on carried; nothing where none is counted. */
function Endpoints({ endpoints }: { readonly endpoints: ReadonlyMap<string, EndpointTotal> }) {
  if (endpoints.size === 0) {
    return null;
  }
  const heading = t({
    defaultMessage: "By endpoint",
    id: "RtRRxOmg",
    description: "what each endpoint of the workload carried",
  });
  const rows = [...endpoints].map(([name, e]): [string, string] => [
    name,
    t(
      {
        defaultMessage: "{received} received, {sent} sent",
        id: "0CdU+wF7",
        description: "an endpoint's bytes",
      },
      { received: bytes(e.receivedBytes), sent: bytes(e.sentBytes) },
    ),
  ]);
  return (
    <section aria-label={heading}>
      <h4>{heading}</h4>
      <Figures rows={rows} />
    </section>
  );
}

/** What the last day's records of the workload come to, and each of them. */
function Accounted({ records, from }: { readonly records: UsageRecord[]; readonly from: number }) {
  const sum = totals(records);
  const heading = t({
    defaultMessage: "Accounted",
    id: "axg3rbL5",
    description: "usage the workload is billed on",
  });
  return (
    <section aria-label={heading}>
      <h3>{heading}</h3>
      <p>
        {t(
          {
            defaultMessage:
              "From {from} to now: {measured, plural, one {# interval} other {# intervals}} measured{gaps, plural, =0 {} other {, not counting # unheard over {hours} hours}}.",
            id: "Adiqi3MR",
          },
          {
            from: at(from),
            measured: sum.measured,
            gaps: sum.gaps,
            hours: intl.formatNumber(sum.gapMs / 3_600_000, { maximumFractionDigits: 1 }),
          },
        )}
      </p>
      <Figures
        rows={[
          [NAMES.cpu(), map(sum.cpuUsec, cpuSeconds)],
          [NAMES.throttled(), map(sum.cpuThrottledUsec, cpuSeconds)],
          [NAMES.memory(), map(sum.memoryKibSeconds, gibHours)],
          [NAMES.peak(), map(sum.memoryPeakBytes, bytes)],
          [NAMES.read(), map(sum.ioReadBytes, bytes)],
          [NAMES.written(), map(sum.ioWriteBytes, bytes)],
          [NAMES.received(), map(sum.netRxBytes, bytes)],
          [NAMES.sent(), map(sum.netTxBytes, bytes)],
          [NAMES.egress(), map(sum.egressBytes, bytes)],
          [NAMES.cores(), map(sum.dedicatedCoreSeconds, coreHours)],
          [
            NAMES.devices(),
            sum.deviceSeconds.size === 0 ? undefined : deviceHours(sum.deviceSeconds),
          ],
          [
            NAMES.disk(),
            map(sum.disk, (d) =>
              d.allocated
                ? t(
                    {
                      defaultMessage: "{size} allocated at {at}",
                      id: "s21iDKaW",
                      description:
                        "a microVM's disk measured then: what its block devices have allocated, which grows as it writes and does not shrink when it deletes",
                    },
                    { size: bytes(d.bytes), at: at(d.at) },
                  )
                : t(
                    {
                      defaultMessage: "{size} at {at}",
                      id: "/0MCIc2b",
                      description: "disk measured then",
                    },
                    { size: bytes(d.bytes), at: at(d.at) },
                  ),
            ),
          ],
        ]}
      />
      <Endpoints endpoints={sum.endpoints} />
      <p className="aside">
        {t({
          defaultMessage:
            "Measured by the workload's node and recorded by the control plane an interval at a time. CPU, throttled time, and block and network bytes are the kernel's counters, differenced between two samples; memory is integrated from readings every ten seconds; disk is what the volumes' filesystem accounted at an interval's end, or, said to be allocated, what a microVM's block devices have allocated, which grows as it writes and does not shrink when it deletes. Egress, and each endpoint whose accounting is on, are counted in the node's firewall, out of the workload's reach; egress is what left for anything but another workload on its node. A gap is an interval nothing was heard over, never filled in.",
          id: "0BoBaABh",
        })}
      </p>
      <Intervals records={records} />
    </section>
  );
}

/** The usage of workload `id`. */
export function Usage({ id }: { readonly id: string }) {
  const { clients } = useReach();
  const [read, setRead] = useState<
    | { readonly kind: "records"; readonly records: UsageRecord[]; readonly from: number }
    | { readonly kind: "refused"; readonly why: string }
    | undefined
  >(undefined);
  useEffect(() => {
    const stop = new AbortController();
    const from = Date.now() - DAY_MS;
    void (async () => {
      try {
        const records: UsageRecord[] = [];
        let pageToken = "";
        do {
          const r = await clients.metering.listUsage(
            {
              workload: id,
              from: { seconds: BigInt(Math.floor(from / 1000)) },
              page: { pageSize: 500, pageToken },
            },
            { signal: stop.signal },
          );
          records.push(...r.records);
          pageToken = r.page?.nextPageToken ?? "";
        } while (pageToken !== "" && records.length < MOST);
        if (!stop.signal.aborted) setRead({ kind: "records", records, from });
      } catch (e) {
        if (!stop.signal.aborted) setRead({ kind: "refused", why: refusal(e) });
      }
    })();
    return () => {
      stop.abort();
    };
  }, [clients.metering, id]);
  const heading = t({
    defaultMessage: "Usage",
    id: "wBsEapDn",
    description: "what a workload used",
  });
  return (
    <section aria-label={heading}>
      <h2>{heading}</h2>
      {read === undefined && (
        <p role="status">{t({ defaultMessage: "Reading what it used…", id: "nkWrtaLz" })}</p>
      )}
      {read?.kind === "refused" && <p role="alert">{read.why}</p>}
      {read?.kind === "records" && (
        <>
          <Now id={id} />
          <Accounted records={read.records} from={read.from} />
        </>
      )}
    </section>
  );
}
