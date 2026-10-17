// SPDX-License-Identifier: AGPL-3.0-or-later

import { Code, ConnectError, createClient, createRouterTransport } from "@connectrpc/connect";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { AccountService } from "../gen/korpis/v1/account_pb";
import { ErrorSchema } from "../gen/korpis/v1/common_pb";
import { ProfileService } from "../gen/korpis/v1/profile_pb";
import { SessionService } from "../gen/korpis/v1/session_pb";
import { Profile } from "./Profile";

function services(seen: string[]) {
  let recent = false;
  let password = true;
  const transport = createRouterTransport(({ service }) => {
    service(ProfileService, {
      getProfile: () => ({
        name: "ada",
        displayName: "Ada",
        password,
        totp: true,
        recoveryCodes: 7,
      }),
      reauthenticateWithPassword: () => {
        recent = true;
        seen.push("reauthenticated");
        return {};
      },
      removePassword: () => {
        if (!recent) {
          throw new ConnectError("sign in again", Code.Aborted, undefined, [
            { desc: ErrorSchema, value: { reason: "session.not_recent" } },
          ]);
        }
        password = false;
        seen.push("password removed");
        return {};
      },
      renewRecoveryCodes: () => ({ recoveryCodes: ["new-1", "new-2"] }),
    });
    service(SessionService, {
      listSessions: () => ({
        sessions: [
          { id: "ses_1", kind: "web", userAgent: "Firefox", source: "192.0.2.1", current: true },
          { id: "ses_2", kind: "cli", userAgent: "korpis/0.1.0", source: "198.51.100.4" },
        ],
      }),
      endOtherSessions: () => {
        seen.push("others ended");
        return { ended: 1n };
      },
    });
  });
  return {
    profiles: createClient(ProfileService, transport),
    sessions: createClient(SessionService, transport),
    accounts: createClient(AccountService, transport),
    assert: vi.fn(),
    create: vi.fn(),
  };
}

test("a change refused as not recent asks for the user again and is then made", async () => {
  const seen: string[] = [];
  render(<Profile services={services(seen)} />);
  fireEvent.click(await screen.findByRole("button", { name: "Remove the password" }));
  const confirm = await screen.findByRole("region", { name: "Confirm it is you" });
  fireEvent.change(within(confirm).getByLabelText("Password"), {
    target: { value: "correct horse" },
  });
  fireEvent.change(within(confirm).getByLabelText("Code"), { target: { value: "123456" } });
  fireEvent.click(within(confirm).getByRole("button", { name: "Confirm" }));
  expect(await screen.findByRole("button", { name: "Set a password" })).toBeTruthy();
  expect(seen).toEqual(["reauthenticated", "password removed"]);
});

test("new recovery codes are shown once, and sessions are listed and ended", async () => {
  const seen: string[] = [];
  render(<Profile services={services(seen)} />);
  expect(await screen.findByText("7 unused.")).toBeTruthy();
  expect(await screen.findByText(/Command line: korpis\/0.1.0, from 198.51.100.4/)).toBeTruthy();
  expect(screen.getByText("(this one)")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "End every other session" }));
  expect(await screen.findByText("1 ended.")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Make new ones" }));
  expect(await screen.findByText("new-1")).toBeTruthy();
  expect(seen).toEqual(["others ended"]);
});
