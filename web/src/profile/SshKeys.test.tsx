// SPDX-License-Identifier: AGPL-3.0-or-later

import { Code, ConnectError, createClient, createRouterTransport } from "@connectrpc/connect";
import { fireEvent, render, screen } from "@testing-library/react";
import { AccountService } from "../gen/korpis/v1/account_pb";
import { ErrorSchema } from "../gen/korpis/v1/common_pb";
import { type AddSshKeyRequest, ProfileService } from "../gen/korpis/v1/profile_pb";
import { SessionService } from "../gen/korpis/v1/session_pb";
import { SshKeys } from "./SshKeys";

const KEY = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHYUMOAa8CAodE24mCEXvC+hwFHXgRkuGMAE8eBMqcsx ada";
const SIGNATURE = "-----BEGIN SSH SIGNATURE-----\nU1NIU0lH\n-----END SSH SIGNATURE-----\n";

function services(added: AddSshKeyRequest[], recent: boolean) {
  const keys = [
    { id: "ssk_1", label: "laptop", kind: "ssh-ed25519", fingerprint: "SHA256:I3XZHy63" },
  ];
  const transport = createRouterTransport(({ service }) => {
    service(ProfileService, {
      listSshKeys: () => ({ keys }),
      addSshKey: (r) => {
        if (!recent) {
          throw new ConnectError("sign in again", Code.Aborted, undefined, [
            { desc: ErrorSchema, value: { reason: "session.not_recent" } },
          ]);
        }
        added.push(r);
        return { key: { id: "ssk_2", label: r.label, fingerprint: "SHA256:new" } };
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

function fill() {
  fireEvent.change(screen.getByRole("textbox", { name: "Public key, as in your .pub file" }), {
    target: { value: `  ${KEY}\n` },
  });
  fireEvent.change(screen.getByRole("textbox", { name: "Name for this key" }), {
    target: { value: "desk" },
  });
  fireEvent.change(screen.getByRole("textbox", { name: "The key's signature" }), {
    target: { value: SIGNATURE },
  });
  fireEvent.click(screen.getByRole("button", { name: "Add the key" }));
}

test("a key is listed by its fingerprint, and one pasted is added with its signature", async () => {
  const added: AddSshKeyRequest[] = [];
  render(<SshKeys services={services(added, true)} account="usr_ada" />);
  expect(await screen.findByText("SHA256:I3XZHy63")).toBeTruthy();
  expect(
    screen.getByText(
      "printf %s 'korpis ssh-key add usr_ada' | ssh-keygen -Y sign -n korpis-ssh-key -f ~/.ssh/id_ed25519",
    ),
    "the command that signs this account's name",
  ).toBeTruthy();
  expect(screen.getByText("never used")).toBeTruthy();
  fill();
  await screen.findByText("SHA256:I3XZHy63");
  await vi.waitFor(() => {
    expect(added).toHaveLength(1);
  });
  expect(added[0]?.key, "trimmed, otherwise as pasted").toBe(KEY);
  expect(added[0]?.label).toBe("desk");
  expect(added[0]?.signature, "as pasted").toBe(SIGNATURE);
});

test("adding a key long after signing in asks the user to confirm it is them", async () => {
  const added: AddSshKeyRequest[] = [];
  render(<SshKeys services={services(added, false)} account="usr_ada" />);
  await screen.findByText("SHA256:I3XZHy63");
  fill();
  expect(await screen.findByRole("heading", { name: "Confirm it is you" })).toBeTruthy();
  expect(added).toEqual([]);
});
