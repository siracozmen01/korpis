// SPDX-License-Identifier: AGPL-3.0-or-later

// The form that binds a person or a set at an organization's identity provider, or edits a
// binding's interval and grace against the version shown (§5 of docs/08-identity.md, §5 of
// docs/11-surface-web.md). A field it cannot read is named before anything is sent; the control
// plane's refusal is said in its words.

import { useState } from "react";
import {
  Button,
  Form,
  Input,
  Label,
  RadioButton,
  RadioField,
  RadioGroup,
  TextField,
} from "react-aria-components";
import { useReach } from "../clients";
import type { IdentityBinding } from "../gen/korpis/v1/authority_pb";
import { t } from "../i18n/intl";
import { refusal } from "../views/refusal";
import {
  BLANK,
  type BindingDraft,
  BindingError,
  draftOf,
  subjectOf,
  subjectText,
  termsOf,
} from "./identity";

function wrong(e: BindingError): string {
  switch (e.field) {
    case "subject":
      return t({
        defaultMessage: "Name a person, or a set by its realm and name, without a colon.",
        id: "LjPPHXIQ",
      });
    case "provider":
      return t({ defaultMessage: "Name the identity provider.", id: "RxwBxdfL" });
    case "interval":
      return t({
        defaultMessage: "The interval is a whole number of minutes from 1.",
        id: "zM8m+UKl",
      });
    case "grace":
      return t({ defaultMessage: "The grace is a whole number of minutes.", id: "BnCWRq2n" });
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

/** The interval and grace of a draft. */
function Timing(props: { readonly draft: BindingDraft; readonly set: (d: BindingDraft) => void }) {
  const { draft, set } = props;
  return (
    <>
      <Text
        label={t({ defaultMessage: "Asked every this many minutes", id: "3ELC9iPv" })}
        value={draft.interval}
        set={(interval) => {
          set({ ...draft, interval });
        }}
      />
      <Text
        label={t({ defaultMessage: "Minutes of grace while it cannot be asked", id: "isipEzpo" })}
        value={draft.grace}
        set={(grace) => {
          set({ ...draft, grace });
        }}
      />
    </>
  );
}

/** The fields naming whom a new binding binds. */
function Subject(props: { readonly draft: BindingDraft; readonly set: (d: BindingDraft) => void }) {
  const { draft, set } = props;
  const field = (k: keyof BindingDraft) => (v: string) => {
    set({ ...draft, [k]: v });
  };
  return (
    <>
      <Text
        label={t({ defaultMessage: "Identity provider", id: "ZtFkds/I" })}
        value={draft.provider}
        set={field("provider")}
      />
      <RadioGroup
        value={draft.kind}
        onChange={(v) => {
          set({ ...draft, kind: v === "user" ? "user" : "set" });
        }}
      >
        <Label>{t({ defaultMessage: "Whom it binds", id: "FYBLj+RT" })}</Label>
        <RadioField value="set">
          <RadioButton>
            {t({ defaultMessage: "A set: a group, a role", id: "6wYZOskN" })}
          </RadioButton>
        </RadioField>
        <RadioField value="user">
          <RadioButton>{t({ defaultMessage: "One person", id: "yIdyoAX6" })}</RadioButton>
        </RadioField>
      </RadioGroup>
      {draft.kind === "user" ? (
        <Text
          label={t({ defaultMessage: "Their identifier at the provider", id: "hO8zbGty" })}
          value={draft.user}
          set={field("user")}
        />
      ) : (
        <>
          <Text
            label={t({
              defaultMessage: "Realm: the claim or guild naming the set",
              id: "il62TauY",
            })}
            value={draft.realm}
            set={field("realm")}
          />
          <Text
            label={t({ defaultMessage: "The set's name there", id: "OpZ8xYEa" })}
            value={draft.name}
            set={field("name")}
          />
        </>
      )}
    </>
  );
}

/** A form binding someone new at `organization`, or editing `binding`'s interval and grace. */
export function Editor(props: {
  readonly organization: string;
  readonly binding?: IdentityBinding;
  readonly done: () => void;
}) {
  const { clients } = useReach();
  const { organization, binding, done } = props;
  const [draft, setDraft] = useState(binding === undefined ? BLANK : draftOf(binding));
  const [busy, setBusy] = useState(false);
  const [said, setSaid] = useState<string>();
  const save = async () => {
    setBusy(true);
    setSaid(undefined);
    try {
      if (binding === undefined) {
        if (draft.provider.trim() === "") throw new BindingError("provider");
        await clients.identity.createIdentityBinding({
          mutation: { idempotencyKey: crypto.randomUUID() },
          organization,
          provider: draft.provider.trim(),
          subject: subjectOf(draft, organization),
          terms: termsOf(draft),
        });
      } else {
        await clients.identity.updateIdentityBinding({
          mutation: { idempotencyKey: crypto.randomUUID(), expectedVersion: binding.version },
          id: binding.id,
          terms: termsOf(draft, binding),
        });
      }
      done();
    } catch (e) {
      setSaid(e instanceof BindingError ? wrong(e) : refusal(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Form
      aria-label={
        binding === undefined
          ? t({ defaultMessage: "Bind a person or a set", id: "L+NetjuJ" })
          : t(
              { defaultMessage: "Edit {subject}", id: "K/Rt6UmZ" },
              { subject: subjectText(binding.subject) },
            )
      }
      onSubmit={(event) => {
        event.preventDefault();
        void save();
      }}
    >
      {binding === undefined && <Subject draft={draft} set={setDraft} />}
      <Timing draft={draft} set={setDraft} />
      <Button type="submit" isDisabled={busy}>
        {t({ defaultMessage: "Save the binding", id: "FaFxmjmi" })}
      </Button>
      <Button onPress={done}>{t({ defaultMessage: "Cancel", id: "47FYwba+" })}</Button>
      {said !== undefined && <p role="alert">{said}</p>}
    </Form>
  );
}
