// SPDX-License-Identifier: AGPL-3.0-or-later

import { createClient, createRouterTransport } from "@connectrpc/connect";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { AccountService } from "../gen/korpis/v1/account_pb";
import { forgetFragment, takeFragment } from "./fragment";
import { INVITATION_PREFIX, Invitation } from "./Invitation";

const CODES = ["aaaa-bbbb", "cccc-dddd"];

function accounts(seen: { name?: string; password?: string; secret?: Uint8Array }) {
  const transport = createRouterTransport(({ service }) => {
    service(AccountService, {
      getInvitation: (r) => ({
        displayName: r.invitation === "korpisiv_x" ? "Ada" : "?",
        passkeyOnly: false,
      }),
      beginRedemption: (r) => ({
        creation: {
          challengeId: "c1",
          challenge: new Uint8Array([1]),
          rpId: "localhost",
          userId: new Uint8Array(16),
          userName: r.name,
          displayName: "Ada",
        },
      }),
      redeemWithPasskey: (r) => {
        seen.name = r.name;
        return { account: "usr_1", recoveryCodes: CODES, session: "ses_1" };
      },
      offerTotp: () => ({ secret: new Uint8Array([4, 2]), uri: "otpauth://totp/korpis:ada" }),
      redeemWithPassword: (r) => {
        seen.password = r.password;
        seen.secret = r.totpSecret;
        return { account: "usr_1", recoveryCodes: CODES, session: "ses_1" };
      },
    });
  });
  return createClient(AccountService, transport);
}

const made = () =>
  Promise.resolve({ clientDataJson: new Uint8Array(), attestationObject: new Uint8Array() });

test("the fragment's invitation is taken from the address bar and kept while the page renders", () => {
  window.history.replaceState(null, "", "/invitation#korpisiv_x");
  expect(takeFragment(INVITATION_PREFIX)).toBe("korpisiv_x");
  expect(window.location.hash).toBe("");
  expect(takeFragment(INVITATION_PREFIX)).toBe("korpisiv_x");
  forgetFragment(INVITATION_PREFIX);
  expect(takeFragment(INVITATION_PREFIX)).toBeUndefined();
});

test("a passkey redeems the invitation and the recovery codes are shown once", async () => {
  const seen: { name?: string } = {};
  const redeemed = vi.fn();
  render(
    <Invitation held="korpisiv_x" accounts={accounts(seen)} create={made} redeemed={redeemed} />,
  );
  expect(await screen.findByRole("heading", { name: "Welcome, Ada" })).toBeTruthy();
  fireEvent.change(screen.getByLabelText("Choose a sign-in name"), { target: { value: "ada" } });
  fireEvent.click(screen.getByRole("button", { name: "Register a passkey" }));
  expect(await screen.findByText("aaaa-bbbb")).toBeTruthy();
  expect(seen.name).toBe("ada");
  fireEvent.click(screen.getByRole("button", { name: "I have kept them" }));
  expect(redeemed).toHaveBeenCalled();
});

test("a pasted invitation may be redeemed with a password, the TOTP secret going back with it", async () => {
  const seen: { password?: string; secret?: Uint8Array } = {};
  render(
    <Invitation held={undefined} accounts={accounts(seen)} create={made} redeemed={vi.fn()} />,
  );
  fireEvent.change(screen.getByLabelText("The invitation"), { target: { value: " korpisiv_x " } });
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  expect(await screen.findByRole("heading", { name: "Welcome, Ada" })).toBeTruthy();
  fireEvent.change(screen.getByLabelText("Choose a sign-in name"), { target: { value: "ada" } });
  fireEvent.click(screen.getByRole("button", { name: "Use a password and an authenticator app" }));
  expect(await screen.findByText("otpauth://totp/korpis:ada")).toBeTruthy();
  fireEvent.change(screen.getByLabelText("Password, at least 12 characters"), {
    target: { value: "a long enough password" },
  });
  fireEvent.change(screen.getByLabelText("The code your app shows now"), {
    target: { value: "123456" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Redeem" }));
  expect(await screen.findByText("cccc-dddd")).toBeTruthy();
  await waitFor(() => {
    expect(Array.from(seen.secret ?? [])).toEqual([4, 2]);
  });
  expect(seen.password).toBe("a long enough password");
});
