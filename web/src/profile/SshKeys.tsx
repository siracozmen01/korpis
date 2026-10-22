// SPDX-License-Identifier: AGPL-3.0-or-later

// The SSH keys the signed-in user reaches their files with over SFTP (§9.9 of
// docs/08-identity.md): each by its label, kind, and fingerprint, added by pasting a public key
// as `ssh-keygen` writes it, and removed. Adding and removing are changes to how the user reaches
// their account, so each needs a recent sign-in, which the guard asks for.

import { useCallback, useState } from "react";
import { Button, Form, Input, Label, TextArea, TextField } from "react-aria-components";
import type { ProfileSshKey } from "../gen/korpis/v1/profile_pb";
import { t } from "../i18n/intl";
import { useLoad } from "../views/load";
import { Shown } from "../views/Shown";
import { useGuard } from "./guard";
import type { ProfileServices } from "./services";

function KeyItem(props: { readonly k: ProfileSshKey; readonly remove: () => void }) {
  const { k, remove } = props;
  return (
    <li className="ssh-key">
      <strong>{k.label === "" ? k.kind : k.label}</strong>
      <code>{k.fingerprint}</code>
      <span className="aside">
        {k.lastUsedAt === undefined
          ? t({ defaultMessage: "never used", id: "aCxmC+O6" })
          : t({ defaultMessage: "used to reach files", id: "u0MKtjoc" })}
      </span>
      <Button onPress={remove}>
        {t(
          { defaultMessage: "Remove {label}", id: "/Aln2mIq" },
          { label: k.label || k.fingerprint },
        )}
      </Button>
    </li>
  );
}

/** The user's SSH keys, read again after each change, by remounting. */
export function SshKeys({ services }: { readonly services: ProfileServices }) {
  const [round, setRound] = useState(0);
  return (
    <SshKeysRead
      key={round}
      services={services}
      changed={() => {
        setRound((r) => r + 1);
      }}
    />
  );
}

function SshKeysRead(props: { readonly services: ProfileServices; readonly changed: () => void }) {
  const { services, changed } = props;
  const { profiles } = services;
  const keys = useLoad(useCallback(async () => profiles.listSshKeys({}), [profiles]));
  const guard = useGuard(services);
  const [key, setKey] = useState("");
  const [label, setLabel] = useState("");
  const add = () => {
    guard.run(async () => {
      await profiles.addSshKey({ key: key.trim(), label: label.trim() });
      setKey("");
      setLabel("");
      changed();
    });
  };
  return (
    <section aria-labelledby="ssh-keys">
      <h2 id="ssh-keys">{t({ defaultMessage: "SSH keys, for SFTP", id: "lHRqokhh" })}</h2>
      <p>
        {t({
          defaultMessage:
            "An SFTP client reaches your servers' files with one of these keys. A key opens files alone: it signs you in nowhere.",
          id: "ZUkqNXoj",
        })}
      </p>
      <Shown
        loaded={keys}
        reading={t({ defaultMessage: "Reading your SSH keys…", id: "uONZVEnu" })}
        done={(r) =>
          r.keys.length === 0 ? (
            <p>{t({ defaultMessage: "You have no SSH keys.", id: "v3j7Fquw" })}</p>
          ) : (
            <ul>
              {r.keys.map((k) => (
                <KeyItem
                  key={k.id}
                  k={k}
                  remove={() => {
                    guard.run(async () => {
                      await profiles.removeSshKey({ id: k.id });
                      changed();
                    });
                  }}
                />
              ))}
            </ul>
          )
        }
      />
      <Form
        aria-label={t({ defaultMessage: "Add an SSH key", id: "/5dfS4z/" })}
        onSubmit={(e) => {
          e.preventDefault();
          add();
        }}
      >
        <TextField value={key} onChange={setKey}>
          <Label>{t({ defaultMessage: "Public key, as in your .pub file", id: "vtivBNbt" })}</Label>
          <TextArea />
        </TextField>
        <TextField value={label} onChange={setLabel}>
          <Label>{t({ defaultMessage: "Name for this key", id: "ASqOsl6/" })}</Label>
          <Input />
        </TextField>
        <Button type="submit">{t({ defaultMessage: "Add the key", id: "8qBmPI7I" })}</Button>
      </Form>
      {guard.said}
    </section>
  );
}
