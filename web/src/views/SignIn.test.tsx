// SPDX-License-Identifier: AGPL-3.0-or-later

import { Code, ConnectError, createClient, createRouterTransport } from "@connectrpc/connect";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { AccountService } from "../gen/korpis/v1/account_pb";
import { SignIn } from "./SignIn";

const challenge = new Uint8Array([1, 2, 3]);

function accounts(seen: { passkey?: Uint8Array; password?: string }) {
  const transport = createRouterTransport(({ service }) => {
    service(AccountService, {
      beginSignIn: () => ({ request: { challengeId: "c1", challenge, rpId: "localhost" } }),
      signInWithPasskey: (r) => {
        seen.passkey = r.signature;
        return { account: "usr_1", session: "ses_1" };
      },
      signInWithPassword: (r) => {
        seen.password = r.password;
        throw new ConnectError("refused", Code.Unauthenticated);
      },
    });
  });
  return createClient(AccountService, transport);
}

test("a passkey's answer to the control plane's challenge signs in", async () => {
  const seen: { passkey?: Uint8Array } = {};
  const signedIn = vi.fn();
  const assert = vi.fn((c: Uint8Array, rpId: string) => {
    expect([Array.from(c), rpId]).toEqual([[1, 2, 3], "localhost"]);
    const bytes = new Uint8Array([9]);
    return Promise.resolve({
      credentialId: bytes,
      clientDataJson: bytes,
      authenticatorData: bytes,
      signature: new Uint8Array([7, 7]),
    });
  });
  render(<SignIn accounts={accounts(seen)} assert={assert} signedIn={signedIn} />);
  fireEvent.click(screen.getByRole("button", { name: "Sign in with a passkey" }));
  await waitFor(() => {
    expect(signedIn).toHaveBeenCalled();
  });
  expect(Array.from(seen.passkey ?? [])).toEqual([7, 7]);
});

test("a refused password says only that it did not sign in", async () => {
  const seen: { password?: string } = {};
  const signedIn = vi.fn();
  render(<SignIn accounts={accounts(seen)} assert={vi.fn()} signedIn={signedIn} />);
  fireEvent.change(screen.getByLabelText("Sign-in name"), { target: { value: "friend" } });
  fireEvent.change(screen.getByLabelText("Password"), { target: { value: "not it at all" } });
  fireEvent.change(screen.getByLabelText("Code"), { target: { value: "123456" } });
  fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
  expect(await screen.findByRole("alert")).toHaveProperty(
    "textContent",
    "That did not sign you in.",
  );
  expect(seen.password).toBe("not it at all");
  expect(signedIn).not.toHaveBeenCalled();
});
