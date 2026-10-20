// SPDX-License-Identifier: AGPL-3.0-or-later

// Where an identity provider returns a person to (§9.7 of docs/08-identity.md, §1 of
// docs/11-surface-web.md): `/sign-in/callback`, open signed out. It completes the sign-in this
// tab began with the state and code the provider returned and the verifier the tab kept, then
// goes where the sign-in was begun from. A sign-in that re-authenticates completes with the
// session's access token, so the session is confirmed rather than another begun. A tab that kept
// nothing, because another browser began the sign-in, completes nothing and says so.

import type { Client } from "@connectrpc/connect";
import { useEffect, useState } from "react";
import type { SessionService } from "../gen/korpis/v1/session_pb";
import { type Storage, ownPath, takePending } from "../session/external";
import { lostAnswer, refusal } from "./refusal";
import { t } from "../i18n/intl";

/** What completing needs. */
export interface CallbackProps {
  /** The callback's query, as the provider wrote it. */
  readonly query: string;
  readonly storage: Storage;
  /** The session service with no credential, and as the signed-in session. */
  readonly anonymous: Client<typeof SessionService>;
  readonly signedIn: Client<typeof SessionService>;
  /** Called once completed, with where to go. */
  readonly done: (then: string) => void;
}

/** A refusal that is not the control plane's, said as it is. */
class Said extends Error {}

/** The completions this page began, by query: a state completes once, however often the page
 * renders. */
const completions = new Map<string, Promise<string>>();

async function complete(p: CallbackProps): Promise<string> {
  const q = new URLSearchParams(p.query);
  const error = q.get("error");
  if (error !== null) {
    const why = q.get("error_description") ?? error;
    throw new Said(
      t(
        { defaultMessage: "Your identity provider did not sign you in: {why}", id: "FUKsmHmt" },
        { why },
      ),
    );
  }
  const pending = takePending(p.storage);
  if (pending === undefined) {
    throw new Said(
      t({
        defaultMessage:
          "This sign-in was begun in another browser or tab, so it cannot be completed here. Sign in again from this one.",
        id: "TpH+2TQO",
      }),
    );
  }
  const asked = {
    state: q.get("state") ?? "",
    code: q.get("code") ?? "",
    codeVerifier: pending.verifier,
  };
  await (pending.reauthenticate ? p.signedIn : p.anonymous).completeExternalSignIn(asked);
  return ownPath(pending.then);
}

/** The callback page. */
export function Callback(props: CallbackProps) {
  const [said, setSaid] = useState<string>();
  const [lost, setLost] = useState(false);
  const { query, done } = props;
  useEffect(() => {
    let current = true;
    let begun = completions.get(query);
    if (begun === undefined) {
      begun = complete(props);
      completions.set(query, begun);
    }
    begun.then(
      (then) => {
        if (current) done(then);
      },
      (e: unknown) => {
        if (!current) return;
        if (e instanceof Said) setSaid(e.message);
        else if (lostAnswer(e)) setLost(true);
        else setSaid(refusal(e));
      },
    );
    return () => {
      current = false;
    };
  }, [query, done, props]);
  return (
    <main>
      <h1>{t({ defaultMessage: "Signing in", id: "FDCCkvlv" })}</h1>
      {lost ? (
        <div role="alert">
          <p>
            {t({
              defaultMessage:
                "The answer did not reach this browser, so it may have signed you in. Go on to find out; if it did not, sign in again.",
              id: "78wEWTnD",
            })}
          </p>
          <a href="/">{t({ defaultMessage: "Go on", id: "RTVXrAOw" })}</a>
        </div>
      ) : said === undefined ? (
        <p>
          {t({ defaultMessage: "Completing the sign-in through your provider…", id: "B4hht6h4" })}
        </p>
      ) : (
        <>
          <p role="alert">{said}</p>
          <a href="/sign-in">{t({ defaultMessage: "Back to signing in", id: "IEq/J+Xb" })}</a>
        </>
      )}
    </main>
  );
}
