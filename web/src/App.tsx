// SPDX-License-Identifier: AGPL-3.0-or-later

// The panel's shell (§1 of docs/11-surface-web.md): a client of the published API and nothing
// more, routed by the address bar's path. Until the session is known it says so rather than
// showing a screen it may not be allowed; signed out, every route but the public ones leads to
// signing in, and back to where it was going once signed in.

import { Button, Link, RouterProvider } from "react-aria-components";
import {
  accounts,
  cookieSessions,
  linkClients,
  profiles,
  session as shared,
  sessions,
  signedInClients,
} from "./api";
import { ReachProvider } from "./clients";
import { Redirect, navigate, usePath } from "./router";
import { useSession } from "./session/useSession";
import type { Session } from "./session/session";
import { assert, create } from "./session/webauthn";
import { Device } from "./views/Device";
import { forgetFragment, takeFragment } from "./views/fragment";
import { INVITATION_PREFIX, Invitation } from "./views/Invitation";
import { LINK_BASE, LINK_PREFIX, LinkShell } from "./views/LinkShell";
import { Place, placeAt } from "./views/place";
import { Home } from "./views/Places";
import { SignIn } from "./views/SignIn";

/** The signed-in user's clients, and links written from the panel's root. */
const signedInReach = { clients: signedInClients, base: "" };

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

/** The shell, on the session every tab shares unless it is given another. Its links change the
 * address in place rather than loading a page, which would forget a share link held in memory. */
export function App({ session = shared }: { readonly session?: Session }) {
  return (
    <RouterProvider
      navigate={(to) => {
        navigate(to);
      }}
    >
      <Routes session={session} />
    </RouterProvider>
  );
}

/** The page the address names. */
function Routes({ session }: { readonly session: Session }) {
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
  if (path === LINK_BASE || path.startsWith(`${LINK_BASE}/`)) {
    const link = takeFragment(LINK_PREFIX);
    return <LinkShell clients={link === undefined ? undefined : linkClients(link)} path={path} />;
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
  const place = placeAt(path);
  return (
    <ReachProvider value={signedInReach}>
      <header>
        <Link href="/">Korpis</Link>
        <Button
          onPress={() => {
            void cookieSessions.signOut({}).finally(() => {
              session.end();
            });
          }}
        >
          Sign out
        </Button>
      </header>
      {place !== undefined ? (
        <Place entry={place} />
      ) : path === "/" ? (
        <main>
          <h1>Korpis</h1>
          <Home />
        </main>
      ) : (
        <main>
          <h1>Not found</h1>
          <p>The panel has no page at this address.</p>
        </main>
      )}
      <footer>
        {/* The control plane's own page, not the panel's: loaded, not routed. */}
        <a href="/about/source">Source</a>
      </footer>
    </ReachProvider>
  );
}
