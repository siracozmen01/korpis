// SPDX-License-Identifier: AGPL-3.0-or-later

// Signing in through an identity provider (§9.7 of docs/08-identity.md, §1 of
// docs/11-surface-web.md): a button on the sign-in page for each provider of the installation's
// root organization that signs people in, and the page a provider's sign-in link opens,
// `/sign-in/<organization>/<name>`, which offers that one. Either keeps the verifier in the tab
// and sends the browser to the provider.

import type { Client } from "@connectrpc/connect";
import { useCallback, useState } from "react";
import { Button } from "react-aria-components";
import type { SessionService } from "../gen/korpis/v1/session_pb";
import { type ProviderAt, type Storage, beginExternal } from "../session/external";
import { useLoad } from "./load";
import { refusal } from "./refusal";
import { Brand } from "../theme/useTheme";
import { t } from "../i18n/intl";

/** What beginning a sign-in through a provider needs. */
export interface ExternalProps {
  /** The session service, with no credential. */
  readonly sessions: Client<typeof SessionService>;
  readonly storage: Storage;
  /** Where to go once signed in. */
  readonly then: string;
  /** Sends the browser to the provider. */
  readonly go: (url: string) => void;
}

/** A button that begins a sign-in through `provider`, and what went wrong if it did. */
function Through(props: ExternalProps & { readonly provider: ProviderAt }) {
  const { sessions, storage, then, go, provider } = props;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const begin = async () => {
    setBusy(true);
    setError(undefined);
    try {
      go(await beginExternal(sessions, storage, provider, { then, reauthenticate: false }));
    } catch (e) {
      setError(refusal(e));
      setBusy(false);
    }
  };
  return (
    <>
      <Button isDisabled={busy} onPress={() => void begin()}>
        {t({ defaultMessage: "Sign in with {name}", id: "YzgT39O0" }, { name: provider.name })}
      </Button>
      {error !== undefined && <p role="alert">{error}</p>}
    </>
  );
}

/** The sign-in page's providers; nothing when the installation offers none. */
export function Providers(props: ExternalProps) {
  const { sessions } = props;
  const listed = useLoad(useCallback(() => sessions.listSignInProviders({}), [sessions]));
  if (listed.state !== "done" || listed.value.providers.length === 0) return null;
  return (
    <section aria-labelledby="providers">
      <h2 id="providers">
        {t({ defaultMessage: "Or through your organization", id: "LMbOMhPv" })}
      </h2>
      {listed.value.providers.map((p) => (
        <Through key={`${p.organization}/${p.name}`} {...props} provider={p} />
      ))}
    </section>
  );
}

/** The page a provider's sign-in link opens. */
export function ProviderSignIn(props: ExternalProps & { readonly provider: ProviderAt }) {
  return (
    <main>
      <h1>
        <Brand />
      </h1>
      <h2>
        {t({
          defaultMessage: "Sign in",
          id: "wNuwN10n",
          description: "heading of a provider's sign-in page",
        })}
      </h2>
      <Through {...props} />
    </main>
  );
}

/** The provider a sign-in link's path names, if it names one. */
export function providerAt(path: string): ProviderAt | undefined {
  const [organization, name, ...rest] = path.slice("/sign-in/".length).split("/");
  if (organization === undefined || name === undefined || rest.length > 0) return undefined;
  if (organization === "" || name === "" || organization === "callback") return undefined;
  try {
    return { organization: decodeURIComponent(organization), name: decodeURIComponent(name) };
  } catch {
    return undefined;
  }
}
