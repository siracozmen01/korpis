// SPDX-License-Identifier: AGPL-3.0-or-later

import { Code, ConnectError, type Client } from "@connectrpc/connect";
import { fireEvent, render, screen } from "@testing-library/react";
import type { AccountService } from "../gen/korpis/v1/account_pb";
import { Invitation } from "./Invitation";
import { LOST_BEFORE } from "../retrying";
import { lostAnswer } from "./refusal";
import { SignIn } from "./SignIn";

/** What the browser's fetch rejects with when the network loses a call's answer. */
const lost = () => Promise.reject(new TypeError("Failed to fetch"));

/** An account service whose ceremonies' answers are lost on the way back. */
const accounts = {
  getInvitation: () => Promise.resolve({ displayName: "Ada", passkeyOnly: false }),
  beginRedemption: () =>
    Promise.resolve({
      creation: {
        challengeId: "c1",
        challenge: new Uint8Array([1]),
        rpId: "localhost",
        userId: new Uint8Array(16),
        userName: "ada",
        displayName: "Ada",
        excludeCredentials: [],
      },
    }),
  redeemWithPasskey: lost,
  beginSignIn: () =>
    Promise.resolve({ request: { challengeId: "c2", challenge: new Uint8Array([2]), rpId: "x" } }),
  signInWithPasskey: lost,
} as unknown as Client<typeof AccountService>;

const made = () =>
  Promise.resolve({ clientDataJson: new Uint8Array(), attestationObject: new Uint8Array() });

test("a call the network failed, or a retry refused after one, is a lost answer", () => {
  expect(lostAnswer(new TypeError("Failed to fetch"))).toBe(true);
  expect(lostAnswer(new Error("refused"))).toBe(false);
  const retried = new ConnectError("spent", Code.Unauthenticated, { [LOST_BEFORE]: "1" });
  expect(lostAnswer(retried)).toBe(true);
  expect(lostAnswer(new ConnectError("spent", Code.Unauthenticated))).toBe(false);
});

test("a redemption whose answer was lost says the account may exist and how to recover", async () => {
  render(<Invitation held="korpisiv_x" accounts={accounts} create={made} redeemed={vi.fn()} />);
  expect(await screen.findByRole("heading", { name: "Welcome, Ada" })).toBeTruthy();
  fireEvent.change(screen.getByLabelText("Choose a sign-in name"), { target: { value: "ada" } });
  fireEvent.click(screen.getByRole("button", { name: "Register a passkey" }));
  const said = (await screen.findByRole("alert")).textContent;
  expect(said).toContain("your account may have been made");
  expect(said).toContain("the passkey you just registered");
  expect(said).toContain("renew your recovery codes");
});

test("a sign-in whose answer was lost offers to go on rather than ask again", async () => {
  const signedIn = vi.fn();
  const bytes = new Uint8Array([9]);
  const assert = () =>
    Promise.resolve({
      credentialId: bytes,
      clientDataJson: bytes,
      authenticatorData: bytes,
      signature: bytes,
    });
  render(<SignIn accounts={accounts} assert={assert} signedIn={signedIn} />);
  fireEvent.click(screen.getByRole("button", { name: "Sign in with a passkey" }));
  expect((await screen.findByRole("alert")).textContent).toContain("it may have signed you in");
  fireEvent.click(screen.getByRole("button", { name: "Go on" }));
  expect(signedIn).toHaveBeenCalledTimes(1);
});
