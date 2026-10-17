// SPDX-License-Identifier: AGPL-3.0-or-later

// Redeeming an invitation (§9.6 of docs/08-identity.md): a sign-in name chosen, and a first
// factor registered with it, a passkey, or, where the invitation allows, a password with an
// authenticator app's codes beside it. The recovery codes are shown once, here, and never again.

import type { Client } from "@connectrpc/connect";
import { useEffect, useState } from "react";
import { Button, Form, Input, Label, TextField } from "react-aria-components";
import type { AccountService, GetInvitationResponse } from "../gen/korpis/v1/account_pb";
import type { Creation, Registered } from "../session/webauthn";
import { field } from "./PasswordForm";
import { Qr } from "./Qr";
import { refusal } from "./refusal";
import { t } from "../i18n/intl";

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
    return error === undefined ? (
      <p>{t({ defaultMessage: "Reading the invitation\u2026", id: "3grWaoK5" })}</p>
    ) : (
      <p role="alert">{error}</p>
    );
  }
  if (step.kind === "codes") return <Codes codes={step.codes} done={redeemed} />;

  const withPasskey = (name: string) => {
    run(async () => {
      const begun = await accounts.beginRedemption({ invitation, name });
      const c = begun.creation;
      if (c === undefined)
        throw new Error(
          t({ defaultMessage: "the control plane sent no registration", id: "XiPLZivE" }),
        );
      const made = await create({ ...c, exclude: c.excludeCredentials });
      const registration = {
        challengeId: c.challengeId,
        label: t({
          defaultMessage: "passkey",
          id: "OJ+KsVog",
          description: "a passkey's label, until renamed",
        }),
        ...made,
      };
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
      <h1>
        {t({ defaultMessage: "Welcome, {name}", id: "msSyC/mx" }, { name: step.offer.displayName })}
      </h1>
      <Form
        onSubmit={(event) => {
          event.preventDefault();
          withPasskey(name);
        }}
      >
        <TextField name="name" isRequired autoComplete="username" value={name} onChange={setName}>
          <Label>{t({ defaultMessage: "Choose a sign-in name", id: "/Sh00btp" })}</Label>
          <Input />
        </TextField>
        <Button type="submit">{t({ defaultMessage: "Register a passkey", id: "iKKCuLhf" })}</Button>
        {!step.offer.passkeyOnly && (
          <Button
            isDisabled={name === ""}
            onPress={() => {
              offerTotp(name);
            }}
          >
            {t({ defaultMessage: "Use a password and an authenticator app", id: "pSEaRCSm" })}
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
      <h1>{t({ defaultMessage: "Set a password", id: "d2sELMI6" })}</h1>
      <p>
        {t({
          defaultMessage:
            "Add this account to your authenticator app, by its QR code or by this address:",
          id: "T0+4f3BN",
        })}
      </p>
      <Qr uri={props.uri} />
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
          <Label>{t({ defaultMessage: "Password, at least 12 characters", id: "c3XoMktx" })}</Label>
          <Input />
        </TextField>
        <TextField name="code" isRequired autoComplete="one-time-code">
          <Label>{t({ defaultMessage: "The code your app shows now", id: "RMzLxhOc" })}</Label>
          <Input />
        </TextField>
        <Button type="submit">
          {t({
            defaultMessage: "Redeem",
            id: "E1h4MoXX",
            description: "button: redeem an invitation",
          })}
        </Button>
      </Form>
      {props.error !== undefined && <p role="alert">{props.error}</p>}
    </main>
  );
}

/** The recovery codes, shown once. */
export function Codes({
  codes,
  done,
}: {
  readonly codes: readonly string[];
  readonly done: () => void;
}) {
  return (
    <main>
      <h1>{t({ defaultMessage: "Your recovery codes", id: "co3A9BSP" })}</h1>
      <p>
        {t({
          defaultMessage:
            "Each code signs you in once beside your password, if you lose your other factors. They are shown here once: keep them somewhere safe now.",
          id: "2NWVup5w",
        })}
      </p>
      <ol>
        {codes.map((c) => (
          <li key={c}>
            <code>{c}</code>
          </li>
        ))}
      </ol>
      <Button onPress={done}>{t({ defaultMessage: "I have kept them", id: "KLW42pdD" })}</Button>
    </main>
  );
}
