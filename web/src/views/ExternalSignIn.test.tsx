// SPDX-License-Identifier: AGPL-3.0-or-later

import { Code, ConnectError, createClient, createRouterTransport } from "@connectrpc/connect";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { SessionService } from "../gen/korpis/v1/session_pb";
import { PENDING_KEY, type Storage, ownPath, takePending } from "../session/external";
import { Callback } from "./Callback";
import { Providers, providerAt } from "./ProviderSignIn";

/** A tab's session storage, in memory. */
function storage(): Storage & { readonly kept: Map<string, string> } {
  const kept = new Map<string, string>();
  return {
    kept,
    getItem: (k) => kept.get(k) ?? null,
    setItem: (k, v) => {
      kept.set(k, v);
    },
    removeItem: (k) => {
      kept.delete(k);
    },
  };
}

interface Seen {
  challenge?: string;
  completed?: { state: string; code: string; codeVerifier: string };
}

function sessions(seen: Seen, refuse = false) {
  const transport = createRouterTransport(({ service }) => {
    service(SessionService, {
      listSignInProviders: () => ({ providers: [{ organization: "org_1", name: "corp" }] }),
      beginExternalSignIn: (r) => {
        seen.challenge = r.codeChallenge;
        return { url: "https://idp.example/authorize?state=s1" };
      },
      completeExternalSignIn: (r) => {
        if (refuse) throw new ConnectError("refused", Code.Unauthenticated);
        seen.completed = { state: r.state, code: r.code, codeVerifier: r.codeVerifier };
        return { binding: "idb_1", session: "ses_1" };
      },
    });
  });
  return createClient(SessionService, transport);
}

test("a provider the installation offers begins a sign-in that keeps its verifier in the tab", async () => {
  const seen: Seen = {};
  const tab = storage();
  const go = vi.fn();
  render(<Providers sessions={sessions(seen)} storage={tab} then="/projects" go={go} />);
  fireEvent.click(await screen.findByRole("button", { name: "Sign in with corp" }));
  await waitFor(() => {
    expect(go).toHaveBeenCalledWith("https://idp.example/authorize?state=s1");
  });
  const pending = takePending(tab);
  expect(pending?.then).toBe("/projects");
  expect(pending?.reauthenticate).toBe(false);
  expect(pending?.verifier).toMatch(/^[A-Za-z0-9_-]{43}$/);
  expect(seen.challenge).toMatch(/^[A-Za-z0-9_-]{43}$/);
  expect(seen.challenge).not.toBe(pending?.verifier);
  expect(takePending(tab)).toBeUndefined();
});

test("the callback completes with what the tab kept and goes where the sign-in began", async () => {
  const seen: Seen = {};
  const tab = storage();
  tab.setItem(PENDING_KEY, JSON.stringify({ verifier: "v".repeat(43), then: "/profile" }));
  const done = vi.fn();
  const anonymous = sessions(seen);
  render(
    <Callback
      query="?state=s1&code=c1"
      storage={tab}
      anonymous={anonymous}
      signedIn={sessions({}, true)}
      done={done}
    />,
  );
  await waitFor(() => {
    expect(done).toHaveBeenCalledWith("/profile");
  });
  expect(seen.completed).toEqual({ state: "s1", code: "c1", codeVerifier: "v".repeat(43) });
  expect(tab.kept.size).toBe(0);
});

test("a re-authentication completes as the session, and returns only to the panel itself", async () => {
  const seen: Seen = {};
  const tab = storage();
  const kept = { verifier: "w".repeat(43), then: "//elsewhere.example", reauthenticate: true };
  tab.setItem(PENDING_KEY, JSON.stringify(kept));
  const done = vi.fn();
  render(
    <Callback
      query="?state=s2&code=c2"
      storage={tab}
      anonymous={sessions({}, true)}
      signedIn={sessions(seen)}
      done={done}
    />,
  );
  await waitFor(() => {
    expect(done).toHaveBeenCalledWith("/");
  });
  expect(seen.completed?.state).toBe("s2");
});

test("a tab that kept nothing completes nothing and says why", async () => {
  const seen: Seen = {};
  render(
    <Callback
      query="?state=s3&code=c3"
      storage={storage()}
      anonymous={sessions(seen)}
      signedIn={sessions(seen)}
      done={vi.fn()}
    />,
  );
  expect((await screen.findByRole("alert")).textContent).toContain("another browser or tab");
  expect(seen.completed).toBeUndefined();
});

test("a provider that refused says so, from its own answer", async () => {
  render(
    <Callback
      query="?error=access_denied&error_description=not%20today"
      storage={storage()}
      anonymous={sessions({})}
      signedIn={sessions({})}
      done={vi.fn()}
    />,
  );
  expect((await screen.findByRole("alert")).textContent).toContain("not today");
});

test("a sign-in link names its provider, and nothing else is one", () => {
  expect(providerAt("/sign-in/org_1/corp")).toEqual({ organization: "org_1", name: "corp" });
  expect(providerAt("/sign-in/callback")).toBeUndefined();
  expect(providerAt("/sign-in/org_1")).toBeUndefined();
  expect(providerAt("/sign-in/org_1/corp/more")).toBeUndefined();
  expect(ownPath("/device?code=X")).toBe("/device?code=X");
  expect(ownPath("https://elsewhere.example")).toBe("/");
});
