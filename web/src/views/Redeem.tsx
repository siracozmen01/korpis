// SPDX-License-Identifier: AGPL-3.0-or-later

// Redeeming an invitation (§9.6 of docs/08-identity.md): a sign-in name chosen, and a first
// factor registered with it, a passkey, or, where the invitation allows, a password with an
// authenticator app's codes beside it. The recovery codes are shown once, here, and never again.

import type { Client } from "@connectrpc/connect";
import { useEffect, useState } from "react";
import { Button, Form, Input, Label, TextField } from "react-aria-components";
import { renderSVG } from "uqr";
import type { AccountService, GetInvitationResponse } from "../gen/korpis/v1/account_pb";
import type { Creation, Registered } from "../session/webauthn";
import { field } from "./PasswordForm";
import { refusal } from "./refusal";

/** What redeeming needs. */
export interface RedeemProps {
  readonly invitation: string;
  readonly accounts: Client<typeof AccountService>;
  readonly create: (c: Creation) => Promise<Registered>;
  readonly redeemed: () => void;
}

type Step =
  | { readonly kind: "read" }
  | { readonly kind: "open"; readonly offer: GetInvitationResponse }
  | {
      readonly kind: "totp";
      readonly name: string;
      readonly secret: Uint8Array;
      readonly uri: string;
    }
  | { readonly kind: "codes"; readonly codes: readonly string[] };

/** The redemption screen. */
export function Redeem({ invitation, accounts, create, redeemed }: RedeemProps) {
  const [step, setStep] = useState<Step>({ kind: "read" });
  const [error, setError] = useState<string>();
  const run = (work: () => Promise<void>) => {
    setError(undefined);
    work().catch((e: unknown) => {
      setError(refusal(e));
    });
  };

  const [name, setName] = useState("");
  useEffect(() => {
    let current = true;
    accounts.getInvitation({ invitation }).then(
      (offer) => {
        if (current) setStep({ kind: "open", offer });
      },
      (e: unknown) => {
        if (current) setError(refusal(e));
      },
    );
    return () => {
      current = false;
    };
  }, [accounts, invitation]);

  if (step.kind === "read") {
    return error === undefined ? <p>Reading the invitation…</p> : <p role="alert">{error}</p>;
  }
  if (step.kind === "codes") return <Codes codes={step.codes} done={redeemed} />;

  const withPasskey = (name: string) => {
    run(async () => {
      const begun = await accounts.beginRedemption({ invitation, name });
      const c = begun.creation;
      if (c === undefined) throw new Error("the control plane sent no registration");
      const made = await create({ ...c, exclude: c.excludeCredentials });
      const registration = { challengeId: c.challengeId, label: "passkey", ...made };
      const r = await accounts.redeemWithPasskey({ invitation, name, registration });
      setStep({ kind: "codes", codes: r.recoveryCodes });
    });
  };
  const offerTotp = (name: string) => {
    run(async () => {
      const offer = await accounts.offerTotp({ name });
      setStep({ kind: "totp", name, secret: offer.secret, uri: offer.uri });
    });
  };

  if (step.kind === "totp") {
    return (
      <Password
        uri={step.uri}
        error={error}
        submit={(password, code) => {
          run(async () => {
            const r = await accounts.redeemWithPassword({
              invitation,
              name: step.name,
              password,
              totpSecret: step.secret,
              totpCode: code,
            });
            setStep({ kind: "codes", codes: r.recoveryCodes });
          });
        }}
      />
    );
  }
  return (
    <main>
      <h1>Welcome, {step.offer.displayName}</h1>
      <Form
        onSubmit={(event) => {
          event.preventDefault();
          withPasskey(name);
        }}
      >
        <TextField name="name" isRequired autoComplete="username" value={name} onChange={setName}>
          <Label>Choose a sign-in name</Label>
          <Input />
        </TextField>
        <Button type="submit">Register a passkey</Button>
        {!step.offer.passkeyOnly && (
          <Button
            isDisabled={name === ""}
            onPress={() => {
              offerTotp(name);
            }}
          >
            Use a password and an authenticator app
          </Button>
        )}
      </Form>
      {error !== undefined && <p role="alert">{error}</p>}
    </main>
  );
}

/** The password, and the code that shows the authenticator app read its secret. */
function Password(props: {
  readonly uri: string;
  readonly error: string | undefined;
  readonly submit: (password: string, code: string) => void;
}) {
  return (
    <main>
      <h1>Set a password</h1>
      <p>Add this account to your authenticator app, by its QR code or by this address:</p>
      <img
        alt="QR code of the authenticator address"
        src={`data:image/svg+xml;utf8,${encodeURIComponent(renderSVG(props.uri))}`}
      />
      <p>
        <code>{props.uri}</code>
      </p>
      <Form
        onSubmit={(event) => {
          event.preventDefault();
          const form = new FormData(event.currentTarget);
          props.submit(field(form, "password"), field(form, "code"));
        }}
      >
        <TextField
          name="password"
          type="password"
          isRequired
          autoComplete="new-password"
          minLength={12}
        >
          <Label>Password, at least 12 characters</Label>
          <Input />
        </TextField>
        <TextField name="code" isRequired autoComplete="one-time-code">
          <Label>The code your app shows now</Label>
          <Input />
        </TextField>
        <Button type="submit">Redeem</Button>
      </Form>
      {props.error !== undefined && <p role="alert">{props.error}</p>}
    </main>
  );
}

/** The recovery codes, shown once. */
function Codes({ codes, done }: { readonly codes: readonly string[]; readonly done: () => void }) {
  return (
    <main>
      <h1>Your recovery codes</h1>
      <p>
        Each code signs you in once beside your password, if you lose your other factors. They are
        shown here once: keep them somewhere safe now.
      </p>
      <ol>
        {codes.map((c) => (
          <li key={c}>
            <code>{c}</code>
          </li>
        ))}
      </ol>
      <Button onPress={done}>I have kept them</Button>
    </main>
  );
}
