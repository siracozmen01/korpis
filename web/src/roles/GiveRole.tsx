// SPDX-License-Identifier: AGPL-3.0-or-later

// Giving someone a role here (§4 of docs/08-identity.md, §5 of docs/11-surface-web.md): issuing a
// grant from a template at this place, to someone invited now or to an account that exists. The
// template says what the grant allows and passes on; the version given is the one shown, so a
// role edited meanwhile is refused rather than silently substituted. Issuing is attenuating the
// giver's own grant, so the control plane refuses a role the giver cannot pass on, and says why.
// An invitation is shown once and stored nowhere by the panel.

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
import type { GrantTemplate } from "../gen/korpis/v1/authority_pb";
import { t } from "../i18n/intl";
import { refusal } from "../views/refusal";

/** Where a role is given: an organization, a project, or a workload. */
export interface Place {
  readonly case: "organization" | "project" | "workload";
  readonly id: string;
}

/** The form giving `template` at `place`. */
export function GiveRole(props: { readonly template: GrantTemplate; readonly place: Place }) {
  const { clients } = useReach();
  const { template, place } = props;
  const [who, setWho] = useState<"new" | "account">("new");
  const [name, setName] = useState("");
  const [account, setAccount] = useState("");
  const [busy, setBusy] = useState(false);
  const [said, setSaid] = useState<string>();
  const [refused, setRefused] = useState<string>();
  const [invitation, setInvitation] = useState<string>();
  const give = async () => {
    setBusy(true);
    setSaid(undefined);
    setRefused(undefined);
    try {
      const holder =
        who === "new"
          ? { toNewUser: { displayName: name.trim() } }
          : { subject: { kind: { case: "user" as const, value: account.trim() } } };
      const r = await clients.grants.issueGrant({
        mutation: { idempotencyKey: crypto.randomUUID() },
        scope: { kind: { case: place.case, value: place.id } },
        template: { template: template.id, version: template.version },
        ...holder,
      });
      if (r.invitation !== "") {
        setInvitation(`${window.location.origin}/invitation#${r.invitation}`);
      } else {
        setSaid(t({ defaultMessage: "Given.", id: "fLKiBscu" }));
      }
    } catch (e) {
      setRefused(refusal(e));
    } finally {
      setBusy(false);
    }
  };
  if (invitation !== undefined) {
    return (
      <section aria-label={t({ defaultMessage: "The invitation", id: "MUFpIc0k" })}>
        <p>
          {t({
            defaultMessage:
              "Send this to them. It is shown once, works for seven days, and lets them make their account and holds the role from then on.",
            id: "w5Ys9pKK",
          })}
        </p>
        <output>{invitation}</output>
      </section>
    );
  }
  return (
    <Form
      aria-label={t({ defaultMessage: "Give {role}", id: "/Uw0s6vS" }, { role: template.name })}
      onSubmit={(event) => {
        event.preventDefault();
        void give();
      }}
    >
      <RadioGroup
        value={who}
        onChange={(v) => {
          setWho(v === "account" ? "account" : "new");
        }}
      >
        <Label>{t({ defaultMessage: "To", id: "9j3hXO4O" })}</Label>
        <RadioField value="new">
          <RadioButton>{t({ defaultMessage: "someone invited now", id: "UI92y43Y" })}</RadioButton>
        </RadioField>
        <RadioField value="account">
          <RadioButton>
            {t({ defaultMessage: "an account that exists", id: "4XcIK1Cw" })}
          </RadioButton>
        </RadioField>
      </RadioGroup>
      {who === "new" ? (
        <TextField value={name} onChange={setName} isRequired maxLength={100}>
          <Label>
            {t({ defaultMessage: "Their name, as they will be shown", id: "T7Woi4PP" })}
          </Label>
          <Input />
        </TextField>
      ) : (
        <TextField value={account} onChange={setAccount} isRequired>
          <Label>{t({ defaultMessage: "The account, usr_…", id: "Wefo2UWC" })}</Label>
          <Input />
        </TextField>
      )}
      <Button type="submit" isDisabled={busy}>
        {t({ defaultMessage: "Give {role}", id: "/Uw0s6vS" }, { role: template.name })}
      </Button>
      {said !== undefined && <p role="status">{said}</p>}
      {refused !== undefined && <p role="alert">{refused}</p>}
    </Form>
  );
}
