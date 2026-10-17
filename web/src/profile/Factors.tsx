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
        if (c === undefined) throw new Error("the control plane sent no registration");
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
      <h2 id="factors">How you sign in</h2>
      <h3>Passkeys</h3>
      <ul>
        {profile.passkeys.map((p) => (
          <li key={p.label + String(p.createdAt?.seconds)}>
            {p.label}, added {p.createdAt && timestampDate(p.createdAt).toLocaleDateString()}
            {p.lastUsedAt === undefined
              ? ", never used"
              : `, last used ${timestampDate(p.lastUsedAt).toLocaleDateString()}`}
            {p.backupEligible && ", may be copied to your other devices"}
            <Button
              onPress={() => {
                guard.run(then(() => profiles.removePasskey({ credentialId: p.credentialId })));
              }}
            >
              Remove
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
          <Label>Name for a new passkey</Label>
          <Input />
        </TextField>
        <Button type="submit">Add a passkey</Button>
      </Form>
      <h3>Password</h3>
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
            {profile.password ? "A new password" : "A password"}, at least 12 characters
          </Label>
          <Input />
        </TextField>
        <Button type="submit">{profile.password ? "Change the password" : "Set a password"}</Button>
      </Form>
      {profile.password && (
        <Button
          onPress={() => {
            guard.run(then(() => profiles.removePassword({})));
          }}
        >
          Remove the password
        </Button>
      )}
      <h3>Authenticator app</h3>
      <p>
        {profile.totp ? "An authenticator app gives you codes." : "No authenticator app is set up."}
      </p>
      {totp === undefined ? (
        <Button onPress={offerTotp}>{profile.totp ? "Replace it" : "Set one up"}</Button>
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
            <Label>The code your app shows now</Label>
            <Input />
          </TextField>
          <Button type="submit">Enrol it</Button>
        </Form>
      )}
      {profile.totp && (
        <Button
          onPress={() => {
            guard.run(then(() => profiles.removeTotp({})));
          }}
        >
          Remove the authenticator app
        </Button>
      )}
      <h3>Recovery codes</h3>
      <p>{profile.recoveryCodes.toString()} unused.</p>
      <Button
        onPress={() => {
          guard.run(async () => {
            setCodes((await profiles.renewRecoveryCodes({})).recoveryCodes);
          });
        }}
      >
        Make new ones
      </Button>
      {guard.said}
    </section>
  );
}
