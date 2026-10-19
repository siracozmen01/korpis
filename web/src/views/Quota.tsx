// SPDX-License-Identifier: AGPL-3.0-or-later

// An organization's quota (§5.1 of docs/05-scheduling.md): each entry, what it counts, what it
// guarantees and has sold beside what its workloads hold, and what it may sell below, its limit
// times the overcommit it was permitted; on asking, what it has given below of each entry's
// shape, read from every organization beneath it; and, for a signed-in caller holding
// `org.quota.write` above it, the editor. Every bound shown is held: none is advisory (P4).

import { useState } from "react";
import { Button } from "react-aria-components";
import { useReach } from "../clients";
import type { Organization, QuotaEntry, QuotaSet } from "../gen/korpis/v1/tenancy_pb";
import { bytes } from "../i18n/format";
import { t } from "../i18n/intl";
import { dimensionName } from "./dimension";
import { PAGE_SIZE } from "./Pages";
import { DIMENSIONS, type Dimension, IN_BYTES, givenBelow, labelsText, maySell } from "./quota";
import { QuotaEditor } from "./QuotaEditor";
import { refusal } from "./refusal";

/** An amount in `d`; a bound absent is unbounded. */
function amount(d: Dimension, n: bigint | undefined): string {
  if (n === undefined) return t({ defaultMessage: "unbounded", id: "J2la8Jv5" });
  return IN_BYTES.has(d) ? bytes(n) : n.toString();
}

/** What `e` counts, as a person reads it. */
function shape(e: QuotaEntry): string {
  const workloads =
    e.selector === undefined
      ? t({ defaultMessage: "every workload", id: "Na/D2y5U" })
      : t(
          { defaultMessage: "workloads labelled {labels}", id: "SGSyVNmC" },
          { labels: labelsText(e.selector) },
        );
  return e.nodes === undefined
    ? workloads
    : t(
        { defaultMessage: "{workloads}, on nodes labelled {labels}", id: "TvxMA+Lg" },
        { workloads, labels: labelsText(e.nodes) },
      );
}

/** The dimensions `e` bounds. */
function bounded(e: QuotaEntry): Dimension[] {
  return DIMENSIONS.filter(
    ([d]) => e.reservationBounds?.[d] !== undefined || e.limitBounds?.[d] !== undefined,
  ).map(([d]) => d);
}

/** What `e` guarantees in `d`, what is reserved of it, what it sells, and what is limited. */
function Held({ e, d }: { readonly e: QuotaEntry; readonly d: Dimension }) {
  const held = e.held;
  const wire = DIMENSIONS.find(([f]) => f === d)?.[1] ?? "";
  return (
    <>
      <td>{amount(d, e.reservationBounds?.[d])}</td>
      <td>{amount(d, held?.reservations?.[d] ?? 0n)}</td>
      <td>{amount(d, e.limitBounds?.[d])}</td>
      <td>
        {held?.unbounded.includes(wire) ? amount(d, undefined) : amount(d, held?.limits?.[d] ?? 0n)}
      </td>
    </>
  );
}

function Rows(props: { readonly set: QuotaSet; readonly below: readonly QuotaSet[] | undefined }) {
  const { set, below } = props;
  return set.entries.flatMap((e, i) => {
    const given = below === undefined ? undefined : givenBelow(e, below);
    return bounded(e).map((d) => (
      <tr key={`${String(i)}.${d}`}>
        <td>{shape(e)}</td>
        <th scope="row">{dimensionName(d)}</th>
        <Held e={e} d={d} />
        <td>{amount(d, maySell(set, e, d))}</td>
        {given !== undefined && <td>{amount(d, given.reserved.get(d) ?? 0n)}</td>}
        {given !== undefined && <td>{amount(d, given.sold.get(d) ?? 0n)}</td>}
      </tr>
    ));
  });
}

/** An organization beneath, and its set. */
interface Child {
  readonly name: string;
  readonly set: QuotaSet;
}

/** What each organization beneath was given and holds. */
function Below({ organizations }: { readonly organizations: readonly Child[] }) {
  const rows = organizations.flatMap((c, i) =>
    c.set.entries.flatMap((e, j) =>
      bounded(e).map((d) => (
        <tr key={`${String(i)}.${String(j)}.${d}`}>
          <th scope="row">{c.name}</th>
          <td>{shape(e)}</td>
          <td>{dimensionName(d)}</td>
          <Held e={e} d={d} />
        </tr>
      )),
    ),
  );
  if (rows.length === 0) {
    return (
      <p>
        {t({
          defaultMessage: "Nothing beneath it that you can read holds a quota.",
          id: "LqRHP7sp",
        })}
      </p>
    );
  }
  return (
    <table aria-label={t({ defaultMessage: "Below it", id: "2MR6/PnK" })}>
      <thead>
        <tr>
          <th scope="col">{t({ defaultMessage: "Organization", id: "K56Dims7" })}</th>
          <th scope="col">{t({ defaultMessage: "Counts", id: "R6cxLNsE" })}</th>
          <th scope="col">{t({ defaultMessage: "Dimension", id: "OtefaeX2" })}</th>
          <th scope="col">{t({ defaultMessage: "Guaranteed", id: "Zj8bMqyt" })}</th>
          <th scope="col">{t({ defaultMessage: "Reserved now", id: "v132UQcc" })}</th>
          <th scope="col">{t({ defaultMessage: "Sold", id: "dMvvqrTm" })}</th>
          <th scope="col">{t({ defaultMessage: "Limited now", id: "jkeZBWE3" })}</th>
        </tr>
      </thead>
      <tbody>{rows}</tbody>
    </table>
  );
}

function QuotaTable(props: {
  readonly set: QuotaSet;
  readonly below: readonly QuotaSet[] | undefined;
}) {
  const { set, below } = props;
  return (
    <table aria-label={t({ defaultMessage: "Its quota", id: "AwMTvklc" })}>
      <thead>
        <tr>
          <th scope="col">{t({ defaultMessage: "Counts", id: "R6cxLNsE" })}</th>
          <th scope="col">{t({ defaultMessage: "Dimension", id: "OtefaeX2" })}</th>
          <th scope="col">{t({ defaultMessage: "Guaranteed", id: "Zj8bMqyt" })}</th>
          <th scope="col">{t({ defaultMessage: "Reserved now", id: "v132UQcc" })}</th>
          <th scope="col">{t({ defaultMessage: "Sold", id: "dMvvqrTm" })}</th>
          <th scope="col">{t({ defaultMessage: "Limited now", id: "jkeZBWE3" })}</th>
          <th scope="col">{t({ defaultMessage: "May sell below", id: "OUob3d7M" })}</th>
          {below !== undefined && (
            <th scope="col">{t({ defaultMessage: "Guaranteed below", id: "NeGvAH8o" })}</th>
          )}
          {below !== undefined && (
            <th scope="col">{t({ defaultMessage: "Sold below", id: "fb8GdyOM" })}</th>
          )}
        </tr>
      </thead>
      <tbody>
        <Rows set={set} below={below} />
      </tbody>
    </table>
  );
}

/** The quota of `organization`, which carries its set. */
export function QuotaView({ organization }: { readonly organization: Organization }) {
  const { clients } = useReach();
  const [set, setSet] = useState<QuotaSet | undefined>(organization.quota);
  const [below, setBelow] = useState<readonly Child[]>();
  const [said, setSaid] = useState<string>();
  const [reading, setReading] = useState(false);
  const readBelow = async () => {
    setReading(true);
    setSaid(undefined);
    try {
      const sets: Child[] = [];
      let pageToken = "";
      do {
        const page = { pageSize: PAGE_SIZE, pageToken };
        const r = await clients.organizations.listOrganizations({ parent: organization.id, page });
        for (const child of r.organizations) {
          const got = await clients.organizations.getOrganization({ id: child.id });
          const quota = got.organization?.quota;
          if (quota !== undefined) sets.push({ name: child.name, set: quota });
        }
        pageToken = r.page?.nextPageToken ?? "";
      } while (pageToken !== "");
      setBelow(sets);
    } catch (e) {
      setSaid(refusal(e));
    } finally {
      setReading(false);
    }
  };
  const root = organization.parent === undefined;
  return (
    <section aria-labelledby="quota">
      <h2 id="quota">{t({ defaultMessage: "Quota", id: "rjT4uxJR" })}</h2>
      {root ? (
        <p>
          {t({
            defaultMessage:
              "A root organization holds no quota: what it holds is the installation's.",
            id: "MGkTzanz",
          })}
        </p>
      ) : (set?.entries.length ?? 0) === 0 ? (
        <p>
          {t({
            defaultMessage:
              "No quota is set here. What runs beneath it is bounded only by the organizations above it.",
            id: "RFEbngEl",
          })}
        </p>
      ) : (
        set !== undefined && <QuotaTable set={set} below={below?.map((c) => c.set)} />
      )}
      {below === undefined && (set?.entries.length ?? 0) > 0 && (
        <Button isDisabled={reading} onPress={() => void readBelow()}>
          {t({ defaultMessage: "Show what it gave below", id: "gslhnC3m" })}
        </Button>
      )}
      {below !== undefined && <Below organizations={below} />}
      {said !== undefined && <p role="alert">{said}</p>}
      {!root && <QuotaEditor organization={organization} set={set} changed={setSet} />}
    </section>
  );
}
