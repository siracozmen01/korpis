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
  relay,
  serverInfo,
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
import { Profile } from "./profile/Profile";
import { Brand, type Source, Themed } from "./theme/useTheme";
import { t } from "./i18n/intl";
import { Unreachable } from "./offline/Unreachable";

/** The signed-in user's clients, and links written from the panel's root. */
const signedInReach = { clients: signedInClients, base: "", metered: false, relay };

/** The theme the pages before signing in wear: the installation root's. */
const signInTheme: Source = { kind: "sign-in", info: serverInfo };

/** The theme a signed-in viewer wears on every page: their provider's, not the place's. */
const viewerTheme: Source = { kind: "viewer", clients: signedInClients, info: serverInfo };

/** What the profile screens call. */
const profileServices = { profiles, sessions, accounts, assert, create };

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
      <Themed source={signInTheme} stylesheet>
        <SignIn
          accounts={accounts}
          assert={assert}
          signedIn={() => {
            signedIn(to);
          }}
        />
      </Themed>
    );
  }
  if (path === "/invitation") {
    const redeemed = () => {
      forgetFragment(INVITATION_PREFIX);
      signedIn("/");
    };
    const held = takeFragment(INVITATION_PREFIX);
    return (
      <Themed source={signInTheme} stylesheet>
        <Invitation held={held} accounts={accounts} create={create} redeemed={redeemed} />
      </Themed>
    );
  }
  if (path === LINK_BASE || path.startsWith(`${LINK_BASE}/`)) {
    const link = takeFragment(LINK_PREFIX);
    return <LinkShell clients={link === undefined ? undefined : linkClients(link)} path={path} />;
  }
  if (standing === "unknown")
    return <p>{t({ defaultMessage: "Signing in\u2026", id: "s4yZXgis" })}</p>;
  if (standing === "unreachable") return <Unreachable retry={retry} />;
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
      <Themed source={viewerTheme} stylesheet={path !== "/profile"}>
        <header>
          <Link href="/">
            <Brand />
          </Link>
          <Link href="/profile">
            {t({
              defaultMessage: "Profile",
              id: "cy+bnRnd",
              description: "link to the viewer's own profile",
            })}
          </Link>
          <Button
            onPress={() => {
              void cookieSessions.signOut({}).finally(() => {
                session.end();
              });
            }}
          >
            {t({ defaultMessage: "Sign out", id: "xXbJsopy" })}
          </Button>
        </header>
        {path === "/profile" ? (
          <Profile services={profileServices} />
        ) : place !== undefined ? (
          <Place entry={place} />
        ) : path === "/" ? (
          <main>
            <h1>
              <Brand />
            </h1>
            <Home />
          </main>
        ) : (
          <main>
            <h1>{t({ defaultMessage: "Not found", id: "TThIOMeM" })}</h1>
            <p>{t({ defaultMessage: "The panel has no page at this address.", id: "GDi3Iw2v" })}</p>
          </main>
        )}
        <footer>
          {/* The control plane's own page, not the panel's: loaded, not routed. */}
          <a href="/about/source">
            {t({
              defaultMessage: "Source",
              id: "pl9W7HkD",
              description: "link to the panel's source code notice",
            })}
          </a>
        </footer>
      </Themed>
    </ReachProvider>
  );
}
