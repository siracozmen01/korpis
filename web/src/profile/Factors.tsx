// SPDX-License-Identifier: AGPL-3.0-or-later

// The account's factors (§9.3 and §9.6 of docs/08-identity.md): passkeys added and removed, a
// password set or removed, an authenticator app enrolled or removed, and recovery codes renewed,
// each change needing a recent sign-in (guard.tsx). The control plane refuses any change that
// would leave nothing able to sign in, or a password with nothing beside it
// (`account.last_factor`), and the screen says its refusal as it gave it.

import { timestampDate } from "@bufbuild/protobuf/wkt";
import { useState } from "react";
import { Button, Form, Input, Label, TextField } from "react-aria-components";
import type { GetProfileResponse } from "../gen/korpis/v1/profile_pb";
import { field } from "../views/PasswordForm";
import { Qr } from "../views/Qr";
import { Codes } from "../views/Redeem";
import { useGuard } from "./guard";
import type { ProfileServices } from "./services";
import { intl, t } from "../i18n/intl";

/** The factors of `profile`; `changed` reads it again after a change. */
export function Factors(props: {
  readonly profile: GetProfileResponse;
  readonly services: ProfileServices;
  readonly changed: () => void;
}) {
  const { profile, services, changed } = props;
  const { profiles } = services;
  const guard = useGuard(services);
  const [totp, setTotp] = useState<{ readonly secret: Uint8Array; readonly uri: string }>();
  const [codes, setCodes] = useState<readonly string[]>();
  const then = (work: () => Promise<unknown>) => () => work().then(changed);

  const addPasskey = (label: string) => {
    guard.run(
      then(async () => {
        const c = (await profiles.beginAddPasskey({})).creation;
        if (c === undefined)
          throw new Error(
            t({ defaultMessage: "the control plane sent no registration", id: "XiPLZivE" }),
          );
        const made = await services.create({ ...c, exclude: c.excludeCredentials });
        await profiles.addPasskey({ registration: { challengeId: c.challengeId, label, ...made } });
      }),
    );
  };
  const offerTotp = () => {
    guard.run(async () => {
      const offer = await services.accounts.offerTotp({ name: profile.name });
      setTotp({ secret: offer.secret, uri: offer.uri });
    });
  };

  if (codes !== undefined) {
    return (
      <Codes
        codes={codes}
        done={() => {
          setCodes(undefined);
          changed();
        }}
      />
    );
  }
  return (
    <section aria-labelledby="factors">
      <h2 id="factors">{t({ defaultMessage: "How you sign in", id: "+c3DMOtz" })}</h2>
      <h3>{t({ defaultMessage: "Passkeys", id: "ASrBq85Q" })}</h3>
      <ul>
        {profile.passkeys.map((p) => (
          <li key={p.label + String(p.createdAt?.seconds)}>
            {t(
              {
                defaultMessage:
                  "{label}, added {added}{used, select, yes {, last used {lastUsed}} other {, never used}}{copied, select, yes {, may be copied to your other devices} other {}}",
                id: "S89oXUto",
                description: "one passkey: its label, when added and last used, whether it syncs",
              },
              {
                label: p.label,
                added:
                  p.createdAt === undefined
                    ? ""
                    : intl.formatDate(timestampDate(p.createdAt), { dateStyle: "medium" }),
                used: p.lastUsedAt === undefined ? "no" : "yes",
                lastUsed:
                  p.lastUsedAt === undefined
                    ? ""
                    : intl.formatDate(timestampDate(p.lastUsedAt), { dateStyle: "medium" }),
                copied: p.backupEligible ? "yes" : "no",
              },
            )}
            <Button
              onPress={() => {
                guard.run(then(() => profiles.removePasskey({ credentialId: p.credentialId })));
              }}
            >
              {t({
                defaultMessage: "Remove",
                id: "tnUrzlep",
                description: "button: remove a passkey",
              })}
            </Button>
          </li>
        ))}
      </ul>
      <Form
        onSubmit={(e) => {
          e.preventDefault();
          addPasskey(field(new FormData(e.currentTarget), "label"));
        }}
      >
        <TextField name="label" isRequired maxLength={64}>
          <Label>{t({ defaultMessage: "Name for a new passkey", id: "v8oIEeoT" })}</Label>
          <Input />
        </TextField>
        <Button type="submit">{t({ defaultMessage: "Add a passkey", id: "Ck5euHVk" })}</Button>
      </Form>
      <h3>{t({ defaultMessage: "Password", id: "zuF+uaw8", description: "heading" })}</h3>
      <Form
        onSubmit={(e) => {
          e.preventDefault();
          const password = field(new FormData(e.currentTarget), "password");
          guard.run(then(() => profiles.setPassword({ password })));
        }}
      >
        <TextField
          name="password"
          type="password"
          isRequired
          minLength={12}
          autoComplete="new-password"
        >
          <Label>
            {profile.password
              ? t({ defaultMessage: "A new password, at least 12 characters", id: "OTwZkKc4" })
              : t({ defaultMessage: "A password, at least 12 characters", id: "KMcIYorx" })}
          </Label>
          <Input />
        </TextField>
        <Button type="submit">
          {profile.password
            ? t({ defaultMessage: "Change the password", id: "ytucBzBe" })
            : t({ defaultMessage: "Set a password", id: "d2sELMI6" })}
        </Button>
      </Form>
      {profile.password && (
        <Button
          onPress={() => {
            guard.run(then(() => profiles.removePassword({})));
          }}
        >
          {t({ defaultMessage: "Remove the password", id: "pAyCIq9x" })}
        </Button>
      )}
      <h3>{t({ defaultMessage: "Authenticator app", id: "izYqCARS" })}</h3>
      <p>
        {profile.totp
          ? t({ defaultMessage: "An authenticator app gives you codes.", id: "eSjlm7P3" })
          : t({ defaultMessage: "No authenticator app is set up.", id: "9xEmy1yk" })}
      </p>
      {totp === undefined ? (
        <Button onPress={offerTotp}>
          {profile.totp
            ? t({
                defaultMessage: "Replace it",
                id: "ZjYy6Yll",
                description: "replace the authenticator app",
              })
            : t({
                defaultMessage: "Set one up",
                id: "CnfyIJ6Y",
                description: "set up an authenticator app",
              })}
        </Button>
      ) : (
        <Form
          onSubmit={(e) => {
            e.preventDefault();
            const code = field(new FormData(e.currentTarget), "code");
            guard.run(
              then(async () => {
                await profiles.enrolTotp({ secret: totp.secret, code });
                setTotp(undefined);
              }),
            );
          }}
        >
          <Qr uri={totp.uri} />
          <TextField name="code" isRequired autoComplete="one-time-code">
            <Label>{t({ defaultMessage: "The code your app shows now", id: "RMzLxhOc" })}</Label>
            <Input />
          </TextField>
          <Button type="submit">
            {t({
              defaultMessage: "Enrol it",
              id: "pCb+hggQ",
              description: "button: enrol an authenticator app",
            })}
          </Button>
        </Form>
      )}
      {profile.totp && (
        <Button
          onPress={() => {
            guard.run(then(() => profiles.removeTotp({})));
          }}
        >
          {t({ defaultMessage: "Remove the authenticator app", id: "eN1HQN31" })}
        </Button>
      )}
      <h3>{t({ defaultMessage: "Recovery codes", id: "a0PUqsnM" })}</h3>
      <p>
        {t(
          {
            defaultMessage: "{n, plural, one {# unused code} other {# unused codes}}.",
            id: "gyWfsTF2",
          },
          { n: profile.recoveryCodes },
        )}
      </p>
      <Button
        onPress={() => {
          guard.run(async () => {
            setCodes((await profiles.renewRecoveryCodes({})).recoveryCodes);
          });
        }}
      >
        {t({
          defaultMessage: "Make new ones",
          id: "Ak/ics6v",
          description: "button: make new recovery codes",
        })}
      </Button>
      {guard.said}
    </section>
  );
}
