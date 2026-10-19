// SPDX-License-Identifier: AGPL-3.0-or-later

// Setting an organization's quota (§5.1 of docs/05-scheduling.md), offered to a signed-in caller
// whose grants hold `org.quota.write` over an organization above it: a quota is set from above,
// never by the organization it bounds. Each entry is a shape, the labels of the workloads it
// counts and of the nodes it counts them on, with a guaranteed and a sold bound per dimension,
// empty for unbounded; the set carries the overcommit its organization may sell below at. The
// control plane checks the set against the sets around it and says which bound it breaks.

import { useCallback, useState } from "react";
import { Button, Form, Input, Label, TextField } from "react-aria-components";
import { useReach } from "../clients";
import type { Organization, QuotaSet } from "../gen/korpis/v1/tenancy_pb";
import { t } from "../i18n/intl";
import { everyGrant, holds, upward } from "./held";
import { useLoad } from "./load";
import {
  DIMENSIONS,
  type Dimension,
  type Draft,
  type DraftEntry,
  DraftError,
  draftOf,
  setOf,
} from "./quota";
import { dimensionName } from "./dimension";
import { refusal } from "./refusal";

const EMPTY: DraftEntry = { workloads: "", nodes: "", reserved: {}, sold: {} };

/** What is wrong with the field a `DraftError` names, as the editor says it. */
function wrong(field: string): string {
  const [at, side, d] = field.split(".");
  if (at === "overcommit") {
    const name = DIMENSIONS.find(([, wire]) => wire === side)?.[0];
    return t(
      {
        defaultMessage: "The overcommit of {dimension} is not a whole percentage.",
        id: "M6S6vD/N",
      },
      { dimension: name === undefined ? (side ?? "") : dimensionName(name) },
    );
  }
  const entry = String(Number(at) + 1);
  if (side === "workloads" || side === "nodes") {
    return t(
      {
        defaultMessage: "Entry {entry}: labels are written key=value, separated by commas.",
        id: "YAJJ9RyF",
      },
      { entry },
    );
  }
  return t(
    { defaultMessage: "Entry {entry}: {dimension} is not a whole number.", id: "I2PQCQOv" },
    { entry, dimension: dimensionName(d as Dimension) },
  );
}

/** One bound's field, empty for unbounded. */
function Bound(props: {
  readonly label: string;
  readonly value: string | undefined;
  readonly set: (v: string) => void;
}) {
  return (
    <TextField
      aria-label={props.label}
      value={props.value ?? ""}
      onChange={props.set}
      inputMode="numeric"
    >
      <Input />
    </TextField>
  );
}

function EntryFields(props: {
  readonly index: number;
  readonly entry: DraftEntry;
  readonly set: (e: DraftEntry) => void;
  readonly remove: () => void;
}) {
  const { index, entry, set } = props;
  const n = String(index + 1);
  const expressions = [entry.read?.selector, entry.read?.nodes].some(
    (s) => (s?.matchExpressions.length ?? 0) > 0,
  );
  const side = (s: "reserved" | "sold", d: Dimension) => (v: string) => {
    set({ ...entry, [s]: { ...entry[s], [d]: v } });
  };
  return (
    <fieldset>
      <legend>{t({ defaultMessage: "Entry {n}", id: "nh0uJD+E" }, { n })}</legend>
      {expressions && (
        <p>
          {t({
            defaultMessage:
              "Its selectors also hold label expressions, which are kept as they are; `korpis quota set` edits them.",
            id: "r8P/1+7/",
          })}
        </p>
      )}
      <TextField
        value={entry.workloads}
        onChange={(workloads) => {
          set({ ...entry, workloads });
        }}
      >
        <Label>
          {t({ defaultMessage: "Workloads labelled (empty for every workload)", id: "9frcY4W1" })}
        </Label>
        <Input
          placeholder={t({
            defaultMessage: "tier=gold",
            id: "FV4Vc2PH",
            description: "an example of workload labels",
          })}
        />
      </TextField>
      <TextField
        value={entry.nodes}
        onChange={(nodes) => {
          set({ ...entry, nodes });
        }}
      >
        <Label>
          {t({ defaultMessage: "On nodes labelled (empty for every node)", id: "bUErCSWv" })}
        </Label>
        <Input
          placeholder={t({
            defaultMessage: "region=eu",
            id: "efex8u1S",
            description: "an example of node labels",
          })}
        />
      </TextField>
      <table>
        <thead>
          <tr>
            <th scope="col">{t({ defaultMessage: "Dimension", id: "OtefaeX2" })}</th>
            <th scope="col">{t({ defaultMessage: "Guaranteed", id: "Zj8bMqyt" })}</th>
            <th scope="col">{t({ defaultMessage: "Sold", id: "dMvvqrTm" })}</th>
          </tr>
        </thead>
        <tbody>
          {DIMENSIONS.map(([d]) => (
            <tr key={d}>
              <th scope="row">{dimensionName(d)}</th>
              <td>
                <Bound
                  label={t(
                    { defaultMessage: "Entry {n}, guaranteed {dimension}", id: "UOwcGikb" },
                    { n, dimension: dimensionName(d) },
                  )}
                  value={entry.reserved[d]}
                  set={side("reserved", d)}
                />
              </td>
              <td>
                <Bound
                  label={t(
                    { defaultMessage: "Entry {n}, sold {dimension}", id: "/E9XnKAE" },
                    { n, dimension: dimensionName(d) },
                  )}
                  value={entry.sold[d]}
                  set={side("sold", d)}
                />
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <Button onPress={props.remove}>
        {t({ defaultMessage: "Remove entry {n}", id: "5uej/eiK" }, { n })}
      </Button>
    </fieldset>
  );
}

function Overcommit(props: { readonly draft: Draft; readonly set: (d: Draft) => void }) {
  const { draft, set } = props;
  return (
    <fieldset>
      <legend>
        {t({
          defaultMessage: "May sell below at (percent of each limit; empty for 100)",
          id: "GqwU/lFt",
        })}
      </legend>
      {DIMENSIONS.map(([d, wire]) => (
        <TextField
          key={wire}
          value={draft.overcommit[wire] ?? ""}
          onChange={(v) => {
            set({ ...draft, overcommit: { ...draft.overcommit, [wire]: v } });
          }}
          inputMode="numeric"
        >
          <Label>{dimensionName(d)}</Label>
          <Input />
        </TextField>
      ))}
    </fieldset>
  );
}

/** The editor of `organization`'s quota, for a caller holding it above; nothing for anyone else. */
export function QuotaEditor(props: {
  readonly organization: Organization;
  readonly set: QuotaSet | undefined;
  readonly changed: (s: QuotaSet | undefined) => void;
}) {
  const { clients, allowed } = useReach();
  const { organization, set, changed } = props;
  const parent = organization.parent;
  const may = useLoad(
    useCallback(async () => {
      // A share link sets no quota, and a root holds none.
      if (allowed !== undefined || parent === undefined) return false;
      const [grants, above] = await Promise.all([everyGrant(clients), upward(clients, parent)]);
      return holds(grants, "org.quota.write", above);
    }, [clients, allowed, parent]),
  );
  const [draft, setDraft] = useState<Draft>();
  const [busy, setBusy] = useState(false);
  const [said, setSaid] = useState<string>();
  if (may.state !== "done" || !may.value) return null;
  if (draft === undefined) {
    return (
      <Button
        onPress={() => {
          setDraft(draftOf(set));
        }}
      >
        {t({ defaultMessage: "Set its quota", id: "U4Y8SVJC" })}
      </Button>
    );
  }
  const save = async () => {
    setSaid(undefined);
    let quota: QuotaSet;
    try {
      quota = setOf(draft);
    } catch (e) {
      if (e instanceof DraftError) {
        setSaid(wrong(e.field));
        return;
      }
      throw e;
    }
    setBusy(true);
    try {
      const r = await clients.organizations.setQuota({
        mutation: { idempotencyKey: crypto.randomUUID() },
        organization: organization.id,
        quota,
      });
      changed(r.organization?.quota);
      setDraft(undefined);
    } catch (e) {
      setSaid(refusal(e));
    } finally {
      setBusy(false);
    }
  };
  const entries = draft.entries;
  const put = (i: number) => (e: DraftEntry) => {
    setDraft({ ...draft, entries: entries.map((x, j) => (j === i ? e : x)) });
  };
  return (
    <Form
      aria-label={t({ defaultMessage: "Set its quota", id: "U4Y8SVJC" })}
      onSubmit={(event) => {
        event.preventDefault();
        void save();
      }}
    >
      {entries.map((e, i) => (
        <EntryFields
          key={i}
          index={i}
          entry={e}
          set={put(i)}
          remove={() => {
            setDraft({ ...draft, entries: entries.filter((_, j) => j !== i) });
          }}
        />
      ))}
      <Button
        onPress={() => {
          setDraft({ ...draft, entries: [...entries, EMPTY] });
        }}
      >
        {t({ defaultMessage: "Add an entry", id: "r7VLZjTn" })}
      </Button>
      <Overcommit draft={draft} set={setDraft} />
      <Button type="submit" isDisabled={busy}>
        {t({ defaultMessage: "Save the quota", id: "OvkS9T6p" })}
      </Button>
      <Button
        onPress={() => {
          setDraft(undefined);
        }}
      >
        {t({ defaultMessage: "Cancel", id: "47FYwba+" })}
      </Button>
      {said !== undefined && <p role="alert">{said}</p>}
    </Form>
  );
}
