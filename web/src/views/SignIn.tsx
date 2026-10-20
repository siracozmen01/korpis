// SPDX-License-Identifier: AGPL-3.0-or-later

// Signing in (§9.6 of docs/08-identity.md): a passkey, which the browser offers for this site
// without a name being typed; or a sign-in name and password with a code from an authenticator
// app or a recovery code beside it. Every refusal reads the same, whichever part was wrong.
// Beside them, the identity providers a person may sign in through instead (§9.7). A sign-in
// whose answer never arrived may have begun a session all the same, so it says so and offers to
// go on rather than ask for the factor again, whose proof was spent.

import type { Client } from "@connectrpc/connect";
import { type ReactNode, useState } from "react";
import { Button } from "react-aria-components";
import type { AccountService } from "../gen/korpis/v1/account_pb";
import type { Assertion } from "../session/webauthn";
import { type Credentials, PasswordForm } from "./PasswordForm";
import { lostAnswer, refusal } from "./refusal";
import { Brand, useWorn } from "../theme/useTheme";
import { t } from "../i18n/intl";

/** What signing in needs: the account service and the browser's passkey ceremony. */
export interface SignInProps {
  readonly accounts: Client<typeof AccountService>;
  readonly assert: (challenge: Uint8Array, rpId: string) => Promise<Assertion>;
  readonly signedIn: () => void;
  /** The identity providers a person may sign in through instead, if any are offered. */
  readonly providers?: ReactNode;
}

/** The sign-in screen. */
export function SignIn({ accounts, assert, signedIn, providers }: SignInProps) {
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [lost, setLost] = useState(false);

  const attempt = async (run: () => Promise<unknown>) => {
    setBusy(true);
    setError(undefined);
    try {
      await run();
      signedIn();
    } catch (e) {
      if (lostAnswer(e)) setLost(true);
      else setError(refusal(e));
    } finally {
      setBusy(false);
    }
  };

  const withPasskey = () =>
    attempt(async () => {
      const begun = await accounts.beginSignIn({});
      const request = begun.request;
      if (request === undefined)
        throw new Error(
          t({ defaultMessage: "the control plane sent no challenge", id: "9u4qdqD4" }),
        );
      const a = await assert(request.challenge, request.rpId);
      await accounts.signInWithPasskey({ challengeId: request.challengeId, ...a });
    });

  const withPassword = (c: Credentials) => attempt(() => accounts.signInWithPassword(c));

  const worn = useWorn();
  return (
    <main>
      <h1>
        <Brand />
      </h1>
      <h2>
        {t({
          defaultMessage: "Sign in",
          id: "+3ifsPpL",
          description: "heading of the sign-in page",
        })}
      </h2>
      {worn?.signInText !== undefined && worn.signInText !== "" && (
        <p className="sign-in-text">{worn.signInText}</p>
      )}
      <Button isDisabled={busy} onPress={() => void withPasskey()}>
        {t({ defaultMessage: "Sign in with a passkey", id: "sn+Rj5s3" })}
      </Button>
      <h2>{t({ defaultMessage: "Or with a password", id: "y09OFmzc" })}</h2>
      <PasswordForm
        withName
        busy={busy}
        action={t({ defaultMessage: "Sign in", id: "Bg2qLKJO", description: "button: sign in" })}
        submit={(c) => void withPassword(c)}
      />
      {error !== undefined && <p role="alert">{error}</p>}
      {lost && (
        <div role="alert">
          <p>
            {t({
              defaultMessage:
                "The answer did not reach this browser, so it may have signed you in. Go on to find out; if it did not, sign in again.",
              id: "78wEWTnD",
            })}
          </p>
          <Button onPress={signedIn}>{t({ defaultMessage: "Go on", id: "RTVXrAOw" })}</Button>
        </div>
      )}
      {providers}
    </main>
  );
}
