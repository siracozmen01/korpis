// SPDX-License-Identifier: AGPL-3.0-or-later

import { Code, ConnectError, createClient, createRouterTransport } from "@connectrpc/connect";
import { fireEvent, render, screen } from "@testing-library/react";
import { AccountService } from "../gen/korpis/v1/account_pb";
import { ErrorSchema } from "../gen/korpis/v1/common_pb";
import { ProfileService } from "../gen/korpis/v1/profile_pb";
import { SessionService } from "../gen/korpis/v1/session_pb";
import { Device } from "./Device";

interface Seen {
  read?: string;
  approved: number;
  denied?: string;
  recent: boolean;
  password?: string;
}

const notRecent = () =>
  new ConnectError("sign in again", Code.Aborted, undefined, [
    { desc: ErrorSchema, value: { reason: "session.not_recent" } },
  ]);

function clients(seen: Seen) {
  const transport = createRouterTransport(({ service }) => {
    service(SessionService, {
      getDeviceAuthorization: (r) => {
        seen.read = r.userCode;
        const typed = r.userCode.toUpperCase().replace(/[^A-Z]/g, "");
        if (typed !== "BCDFGHJK") throw new ConnectError("no such code", Code.NotFound);
        return { userCode: "BCDF-GHJK", userAgent: "korpis/0.1.0", source: "192.0.2.7" };
      },
      approveDevice: () => {
        if (!seen.recent) throw notRecent();
        seen.approved += 1;
        return {};
      },
      denyDevice: (r) => {
        seen.denied = r.userCode;
        return {};
      },
    });
    service(ProfileService, {
      reauthenticateWithPassword: (r) => {
        seen.password = r.password;
        seen.recent = true;
        return {};
      },
    });
  });
  return {
    sessions: createClient(SessionService, transport),
    accounts: createClient(AccountService, transport),
    profiles: createClient(ProfileService, transport),
  };
}

test("a client's request is shown with where it came from, and approving it after re-authenticating signs it in", async () => {
  const seen: Seen = { approved: 0, recent: false };
  render(<Device code="bcdf ghjk" assert={vi.fn()} {...clients(seen)} />);
  expect(await screen.findByText("BCDF-GHJK")).toBeTruthy();
  expect(screen.getByText("korpis/0.1.0")).toBeTruthy();
  expect(screen.getByText("192.0.2.7")).toBeTruthy();
  expect(seen.read).toBe("bcdf ghjk");
  fireEvent.click(screen.getByRole("button", { name: "Approve" }));
  expect(await screen.findByRole("heading", { name: "Confirm it is you" })).toBeTruthy();
  fireEvent.change(screen.getByLabelText("Password"), { target: { value: "correct horse" } });
  fireEvent.change(screen.getByLabelText("Code"), { target: { value: "123456" } });
  fireEvent.click(screen.getByRole("button", { name: "Confirm" }));
  expect(await screen.findByRole("heading", { name: "Signed in" })).toBeTruthy();
  expect([seen.password, seen.approved]).toEqual(["correct horse", 1]);
});

test("a code typed by hand is read, and denying needs no re-authentication", async () => {
  const seen: Seen = { approved: 0, recent: false };
  render(<Device code={undefined} assert={vi.fn()} {...clients(seen)} />);
  const field = screen.getByLabelText("The code your terminal shows");
  fireEvent.change(field, { target: { value: "WRONG" } });
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  expect(await screen.findByRole("alert")).toHaveProperty("textContent", "no such code");
  fireEvent.change(field, { target: { value: "BCDF-GHJK" } });
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  fireEvent.click(await screen.findByRole("button", { name: "Deny" }));
  expect(await screen.findByRole("heading", { name: "Refused" })).toBeTruthy();
  expect([seen.denied, seen.approved]).toEqual(["BCDF-GHJK", 0]);
});
