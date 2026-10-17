// SPDX-License-Identifier: AGPL-3.0-or-later

// Signing in (§9.6 of docs/08-identity.md): a passkey, which the browser offers for this site
// without a name being typed; or a sign-in name and password with a code from an authenticator
// app or a recovery code beside it. Every refusal reads the same, whichever part was wrong.

import type { Client } from "@connectrpc/connect";
import { useState } from "react";
import { Button } from "react-aria-components";
import type { AccountService } from "../gen/korpis/v1/account_pb";
import type { Assertion } from "../session/webauthn";
import { type Credentials, PasswordForm } from "./PasswordForm";
import { refusal } from "./refusal";
import { Brand, useWorn } from "../theme/useTheme";

/** What signing in needs: the account service and the browser's passkey ceremony. */
export interface SignInProps {
  readonly accounts: Client<typeof AccountService>;
  readonly assert: (challenge: Uint8Array, rpId: string) => Promise<Assertion>;
  readonly signedIn: () => void;
}

/** The sign-in screen. */
export function SignIn({ accounts, assert, signedIn }: SignInProps) {
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);

  const attempt = async (run: () => Promise<unknown>) => {
    setBusy(true);
    setError(undefined);
    try {
      await run();
      signedIn();
    } catch (e) {
      setError(refusal(e));
    } finally {
      setBusy(false);
    }
  };

  const withPasskey = () =>
    attempt(async () => {
      const begun = await accounts.beginSignIn({});
      const request = begun.request;
      if (request === undefined) throw new Error("the control plane sent no challenge");
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
      <h2>Sign in</h2>
      {worn?.signInText !== undefined && worn.signInText !== "" && (
        <p className="sign-in-text">{worn.signInText}</p>
      )}
      <Button isDisabled={busy} onPress={() => void withPasskey()}>
        Sign in with a passkey
      </Button>
      <h2>Or with a password</h2>
      <PasswordForm withName busy={busy} action="Sign in" submit={(c) => void withPassword(c)} />
      {error !== undefined && <p role="alert">{error}</p>}
    </main>
  );
}
