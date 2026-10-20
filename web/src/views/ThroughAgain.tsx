// SPDX-License-Identifier: AGPL-3.0-or-later

// Re-authenticating a person signed in through an identity provider (§9.7 of
// docs/08-identity.md): they hold no factors here, so they confirm it is them by signing in
// through their provider again. The callback completes with the session's access token, so the
// session is confirmed rather than another begun, and returns to the page that asked.

import type { Client } from "@connectrpc/connect";
import { useState } from "react";
import { Button } from "react-aria-components";
import type { IdentityBinding } from "../gen/korpis/v1/authority_pb";
import type { SessionService } from "../gen/korpis/v1/session_pb";
import { type Storage, beginExternal } from "../session/external";
import { refusal } from "./refusal";
import { t } from "../i18n/intl";

/** What signing in again through a provider needs. */
export interface Again {
  /** The session service, with no credential. */
  readonly sessions: Client<typeof SessionService>;
  readonly storage: Storage;
  /** Where the browser is now, to return to. */
  readonly here: string;
  /** Sends the browser to the provider. */
  readonly go: (url: string) => void;
}

/** The step, for the person `person` binds. */
export function ThroughAgain(props: Again & { readonly person: IdentityBinding }) {
  const { sessions, storage, here, go, person } = props;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const begin = async () => {
    setBusy(true);
    setError(undefined);
    try {
      const provider = { organization: person.organization, name: person.provider };
      go(await beginExternal(sessions, storage, provider, { then: here, reauthenticate: true }));
    } catch (e) {
      setError(refusal(e));
      setBusy(false);
    }
  };
  return (
    <>
      <Button isDisabled={busy} onPress={() => void begin()}>
        {t(
          { defaultMessage: "Sign in with {name} again", id: "EIodbIJ7" },
          { name: person.provider },
        )}
      </Button>
      {error !== undefined && <p role="alert">{error}</p>}
    </>
  );
}
