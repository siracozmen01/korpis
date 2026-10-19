// SPDX-License-Identifier: AGPL-3.0-or-later

// Making, editing, and retiring an organization's roles (§4 of docs/08-identity.md), offered to a
// signed-in caller holding `template.write` there. An edit is made against the version shown, so
// a role edited meanwhile is a conflict, said in the control plane's words; a field the panel
// cannot read is named before anything is sent. Editing a role widens nothing already given: the
// grants issued from it keep what they hold.

import { useState } from "react";
import {
  Button,
  CheckboxButton,
  CheckboxField,
  Form,
  Input,
  Label,
  RadioButton,
  RadioField,
  RadioGroup,
  TextField,
} from "react-aria-components";
import { useReach } from "../clients";
import type { GrantTemplate } from "../gen/korpis/v1/authority_pb";
import { ObjectKind } from "../gen/korpis/v1/common_pb";
import { t } from "../i18n/intl";
import { refusal } from "../views/refusal";
import { type RoleDraft, RoleError, bodyOf, draftOf } from "./roles";

/** The kinds a role is given on, by the value their choice carries. */
const KINDS: Readonly<Record<string, ObjectKind>> = {
  [String(ObjectKind.ORGANIZATION)]: ObjectKind.ORGANIZATION,
  [String(ObjectKind.PROJECT)]: ObjectKind.PROJECT,
  [String(ObjectKind.WORKLOAD)]: ObjectKind.WORKLOAD,
};

/** What is wrong with the field a `RoleError` names. */
function wrong(e: RoleError): string {
  switch (e.field) {
    case "name":
      return t({ defaultMessage: "A role has a name.", id: "zyYnOoZA" });
    case "actions":
      return t({ defaultMessage: "A role allows at least one action.", id: "Px9wrp6g" });
    case "labels":
      return t({
        defaultMessage: "Labels are written key=value, separated by commas.",
        id: "Fj9NVJ2m",
      });
    case "depth":
      return t({
        defaultMessage: "How many levels deep is a whole number from 1.",
        id: "6aNXS4YN",
      });
    case "days":
      return t({
        defaultMessage: "How many days it lasts is a whole number from 1.",
        id: "Lp0Gkby7",
      });
    case "uses":
      return t({ defaultMessage: "Its uses are a whole number from 1.", id: "ROGy03Hk" });
  }
}

function Text(props: {
  readonly label: string;
  readonly value: string;
  readonly set: (v: string) => void;
}) {
  return (
    <TextField value={props.value} onChange={props.set}>
      <Label>{props.label}</Label>
      <Input />
    </TextField>
  );
}

function Fields(props: { readonly draft: RoleDraft; readonly set: (d: RoleDraft) => void }) {
  const { draft, set } = props;
  const field = (k: keyof RoleDraft) => (v: string) => {
    set({ ...draft, [k]: v });
  };
  return (
    <>
      <Text
        label={t({ defaultMessage: "Name", id: "HAlOn1Zs" })}
        value={draft.name}
        set={field("name")}
      />
      <Text
        label={t({ defaultMessage: "Actions it allows, separated by commas", id: "WnTGCnhk" })}
        value={draft.actions}
        set={field("actions")}
      />
      <RadioGroup
        value={String(draft.kind)}
        onChange={(v) => {
          set({ ...draft, kind: KINDS[v] ?? draft.kind });
        }}
      >
        <Label>{t({ defaultMessage: "Given on", id: "vNKET4df" })}</Label>
        <RadioField value={String(ObjectKind.ORGANIZATION)}>
          <RadioButton>{t({ defaultMessage: "an organization", id: "8N/xR7Qv" })}</RadioButton>
        </RadioField>
        <RadioField value={String(ObjectKind.PROJECT)}>
          <RadioButton>{t({ defaultMessage: "a project", id: "MWd3cfIN" })}</RadioButton>
        </RadioField>
        <RadioField value={String(ObjectKind.WORKLOAD)}>
          <RadioButton>{t({ defaultMessage: "a workload", id: "hdO3gq5y" })}</RadioButton>
        </RadioField>
      </RadioGroup>
      <Text
        label={t({ defaultMessage: "Only the workloads labelled (optional)", id: "wPj0Ogg+" })}
        value={draft.labels}
        set={field("labels")}
      />
      <Text
        label={t({ defaultMessage: "Actions its holder may pass on (optional)", id: "EppLcnga" })}
        value={draft.delegates}
        set={field("delegates")}
      />
      <Text
        label={t({ defaultMessage: "How many levels deep", id: "ivDcqiC1" })}
        value={draft.depth}
        set={field("depth")}
      />
      <Text
        label={t({ defaultMessage: "Lasts this many days (optional)", id: "ALEHPwnw" })}
        value={draft.days}
        set={field("days")}
      />
      <Text
        label={t({ defaultMessage: "Uses (optional)", id: "/oK6MZ9J" })}
        value={draft.uses}
        set={field("uses")}
      />
      <CheckboxField
        isSelected={draft.mfa}
        onChange={(mfa) => {
          set({ ...draft, mfa });
        }}
      >
        <CheckboxButton>
          {t({ defaultMessage: "Every use needs a second factor", id: "yMzzPzXC" })}
        </CheckboxButton>
      </CheckboxField>
    </>
  );
}

/** The editor of a role `organization` owns, or of a new one when `role` is absent. */
export function RoleEditor(props: {
  readonly organization: string;
  readonly role?: GrantTemplate;
  readonly done: () => void;
}) {
  const { clients } = useReach();
  const { organization, role, done } = props;
  const [draft, setDraft] = useState(draftOf(role));
  const [busy, setBusy] = useState(false);
  const [said, setSaid] = useState<string>();
  const call = async (ask: () => Promise<unknown>) => {
    setBusy(true);
    setSaid(undefined);
    try {
      await ask();
      done();
    } catch (e) {
      setSaid(e instanceof RoleError ? wrong(e) : refusal(e));
    } finally {
      setBusy(false);
    }
  };
  const save = () =>
    call(async () => {
      const template = bodyOf(draft);
      if (role === undefined) {
        const mutation = { idempotencyKey: crypto.randomUUID() };
        return clients.grants.createGrantTemplate({ mutation, organization, template });
      }
      const mutation = { idempotencyKey: crypto.randomUUID(), expectedVersion: role.version };
      return clients.grants.updateGrantTemplate({ mutation, id: role.id, template });
    });
  const retire = (r: GrantTemplate) =>
    call(() =>
      clients.grants.retireGrantTemplate({
        mutation: { idempotencyKey: crypto.randomUUID() },
        id: r.id,
      }),
    );
  return (
    <Form
      aria-label={
        role === undefined
          ? t({ defaultMessage: "A new role", id: "ramQHEyT" })
          : t({ defaultMessage: "Edit {role}", id: "AyRVbIDi" }, { role: role.name })
      }
      onSubmit={(event) => {
        event.preventDefault();
        void save();
      }}
    >
      <Fields draft={draft} set={setDraft} />
      <Button type="submit" isDisabled={busy}>
        {t({ defaultMessage: "Save the role", id: "P1vuzQl1" })}
      </Button>
      {role !== undefined && (
        <Button isDisabled={busy} onPress={() => void retire(role)}>
          {t({ defaultMessage: "Retire it", id: "ttY/jSVX" })}
        </Button>
      )}
      <Button onPress={done}>{t({ defaultMessage: "Cancel", id: "47FYwba+" })}</Button>
      {said !== undefined && <p role="alert">{said}</p>}
    </Form>
  );
}
