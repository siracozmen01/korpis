// SPDX-License-Identifier: AGPL-3.0-or-later

// Re-authenticating the session (§9.4 of docs/08-identity.md): a change of factors, or approving
// a command-line client's sign-in, needs a sign-in within the last ten minutes, and a session
// older than that proves itself again with a passkey or the password and its second factor. The
// account is the session's, so no sign-in name is asked for.

import type { Client } from "@connectrpc/connect";
import { useState } from "react";
import { Button } from "react-aria-components";
import type { AccountService } from "../gen/korpis/v1/account_pb";
import type { ProfileService } from "../gen/korpis/v1/profile_pb";
import type { Assertion } from "../session/webauthn";
import { type Credentials, PasswordForm } from "./PasswordForm";
import { refusal } from "./refusal";

/** What re-authenticating needs. */
export interface ReauthenticateProps {
  readonly accounts: Client<typeof AccountService>;
  readonly profiles: Client<typeof ProfileService>;
  readonly assert: (challenge: Uint8Array, rpId: string) => Promise<Assertion>;
  readonly done: () => void;
}

/** The re-authentication step, shown in place of the action that asked for it. */
export function Reauthenticate({ accounts, profiles, assert, done }: ReauthenticateProps) {
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);

  const attempt = async (run: () => Promise<unknown>) => {
    setBusy(true);
    setError(undefined);
    try {
      await run();
      done();
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
      await profiles.reauthenticateWithPasskey({ challengeId: request.challengeId, ...a });
    });

  const withPassword = (c: Credentials) =>
    attempt(() => profiles.reauthenticateWithPassword({ password: c.password, second: c.second }));

  return (
    <section aria-labelledby="reauthenticate">
      <h2 id="reauthenticate">Confirm it is you</h2>
      <p>Your last sign-in was more than ten minutes ago. Sign in again to continue.</p>
      <Button isDisabled={busy} onPress={() => void withPasskey()}>
        Use a passkey
      </Button>
      <PasswordForm
        withName={false}
        busy={busy}
        action="Confirm"
        submit={(c) => void withPassword(c)}
      />
      {error !== undefined && <p role="alert">{error}</p>}
    </section>
  );
}
