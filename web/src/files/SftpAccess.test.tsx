// SPDX-License-Identifier: AGPL-3.0-or-later

import { createClient, createRouterTransport } from "@connectrpc/connect";
import { render, screen } from "@testing-library/react";
import { clientsOver, ReachProvider } from "../clients";
import { RelayService } from "../gen/korpis/v1/relay_pb";
import type { MessageInitShape } from "@bufbuild/protobuf";
import { ServerInfoService, type SftpServerSchema } from "../gen/korpis/v1/api_pb";
import { SftpAccess } from "./SftpAccess";

function shown(sftp: MessageInitShape<typeof SftpServerSchema>[]) {
  const transport = createRouterTransport(({ service }) => {
    service(ServerInfoService, { getServerInfo: () => ({ sftp }) });
  });
  const relay = createClient(RelayService, transport);
  return render(
    <ReachProvider
      value={{
        clients: clientsOver(transport),
        base: "",
        metered: false,
        relay: () => Promise.resolve(relay),
      }}
    >
      <SftpAccess id="wl_1" />
    </ReachProvider>,
  );
}

test("each server is shown as a client takes it, with the key to expect", async () => {
  shown([
    {
      address: "relay.example:2022",
      knownHostsName: "[relay.example]:2022",
      hostKey: "ssh-ed25519 AAAA",
      fingerprint: "SHA256:abc",
      relay: "rly_1",
    },
    { address: "cp.example:22", knownHostsName: "cp.example", hostKey: "ssh-ed25519 BBBB" },
  ]);
  expect(await screen.findByText("sftp -P 2022 wl_1@relay.example")).toBeTruthy();
  expect(screen.getByText("Host key SHA256:abc")).toBeTruthy();
  expect(screen.getByText("[relay.example]:2022 ssh-ed25519 AAAA")).toBeTruthy();
  expect(screen.getByText("sftp wl_1@cp.example"), "port 22 is not named").toBeTruthy();
});

test("nothing is shown where no server serves SFTP", async () => {
  const { container } = shown([]);
  await new Promise((r) => setTimeout(r, 50));
  expect(container.textContent).toBe("");
});
