// SPDX-License-Identifier: AGPL-3.0-or-later

// The panel's shell (§1 of docs/11-surface-web.md): a client of the published API and nothing
// more, routed by the address bar's path. Until the session is known it says so rather than
// showing a screen it may not be allowed; signed out, every route but the public ones leads to
// signing in, and back to where it was going once signed in.

import { Button, Link } from "react-aria-components";
import { accounts, cookieSessions, profiles, session as shared, sessions } from "./api";
import { Redirect, navigate, usePath } from "./router";
import { useSession } from "./session/useSession";
import type { Session } from "./session/session";
import { assert, create } from "./session/webauthn";
import { Device } from "./views/Device";
import { forgetFragment, takeFragment } from "./views/fragment";
import { INVITATION_PREFIX, Invitation } from "./views/Invitation";
import { SignIn } from "./views/SignIn";

/** Where signing in returns to: a path of this panel's own, never another site's. */
export function returnTo(search: string): string {
  const then = new URLSearchParams(search).get("then");
  if (then?.startsWith("/") !== true || then.startsWith("//") || then.includes("\\")) return "/";
  return then;
}

/** The sign-in page's address, returning to `path` (with its query) once signed in. */
function signInFor(path: string, search: string): string {
  return path === "/" ? "/sign-in" : `/sign-in?then=${encodeURIComponent(path + search)}`;
}

/** The shell, on the session every tab shares unless it is given another. */
export function App({ session = shared }: { readonly session?: Session }) {
  const path = usePath();
  const { standing, retry } = useSession(session);
  const signedIn = (to: string) => {
    void session.renew().then(() => {
      navigate(to, { replace: true });
    });
  };
  if (path === "/sign-in") {
    const to = returnTo(window.location.search);
    return (
      <SignIn
        accounts={accounts}
        assert={assert}
        signedIn={() => {
          signedIn(to);
        }}
      />
    );
  }
  if (path === "/invitation") {
    const redeemed = () => {
      forgetFragment(INVITATION_PREFIX);
      signedIn("/");
    };
    const held = takeFragment(INVITATION_PREFIX);
    return <Invitation held={held} accounts={accounts} create={create} redeemed={redeemed} />;
  }
  if (standing === "unknown") return <p>Signing in…</p>;
  if (standing === "unreachable") {
    return (
      <main>
        <p role="alert">The control plane cannot be reached.</p>
        <Button onPress={retry}>Try again</Button>
      </main>
    );
  }
  if (standing === "signed-out") return <Redirect to={signInFor(path, window.location.search)} />;
  if (path === "/device") {
    const code = new URLSearchParams(window.location.search).get("code") ?? undefined;
    return (
      <Device
        code={code}
        sessions={sessions}
        accounts={accounts}
        profiles={profiles}
        assert={assert}
      />
    );
  }
  return (
    <main>
      <h1>Korpis</h1>
      <p>You are signed in. The panel is a client of the API and holds nothing of its own.</p>
      <Button
        onPress={() => {
          void cookieSessions.signOut({}).finally(() => {
            session.end();
          });
        }}
      >
        Sign out
      </Button>
      <footer>
        <Link href="/about/source">Source</Link>
      </footer>
    </main>
  );
}
