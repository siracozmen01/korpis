// SPDX-License-Identifier: AGPL-3.0-or-later

import { create, toBinary } from "@bufbuild/protobuf";
import { Code, ConnectError, createClient, createRouterTransport } from "@connectrpc/connect";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { clientsOver, ReachProvider } from "../clients";
import { GrantService } from "../gen/korpis/v1/api_pb";
import { MeteringService, UsageKind, UsageNowSchema } from "../gen/korpis/v1/metering_pb";
import { RelayService } from "../gen/korpis/v1/relay_pb";
import { Usage } from "./Usage";

const now = BigInt(Math.floor(Date.now() / 1000));

/** A workload measured for an hour and then unheard for one, whose node reads it now. */
function setup(denied = false) {
  const seen: { minted: string[]; asked: string[] } = { minted: [], asked: [] };
  const transport = createRouterTransport(({ service }) => {
    service(MeteringService, {
      listUsage: (r) => {
        if (denied) throw new ConnectError("not yours", Code.PermissionDenied);
        seen.asked.push(r.workload ?? "");
        return {
          records: [
            {
              id: "use_1",
              workload: "wl_1",
              kind: UsageKind.MEASURED,
              from: { seconds: now - 7200n },
              to: { seconds: now - 3600n },
              cpuUsec: 90_000_000n,
              memoryKibSeconds: 1024n * 1024n * 3600n,
            },
            {
              id: "use_2",
              workload: "wl_1",
              kind: UsageKind.GAP,
              cause: "node unreachable",
              from: { seconds: now - 3600n },
              to: { seconds: now },
            },
          ],
        };
      },
    });
    service(GrantService, {
      mintToken: (r) => {
        seen.minted.push(r.actions.join());
        return { token: "tok" };
      },
    });
    service(RelayService, {
      async *watch() {
        yield { message: { case: "opened", value: { session: "s1", sendKey: "k1" } } };
        const reading = create(UsageNowSchema, {
          at: { seconds: now },
          since: { seconds: now - 10n },
          memoryBytes: 512n * 1024n * 1024n,
          cpuMillicores: 1500n,
        });
        yield {
          message: {
            case: "frame",
            value: { frame: { case: "data", value: toBinary(UsageNowSchema, reading) } },
          },
        };
        await new Promise(() => undefined);
      },
    });
  });
  const relay = createClient(RelayService, transport);
  render(
    <ReachProvider
      value={{
        clients: clientsOver(transport),
        base: "",
        metered: false,
        relay: () => Promise.resolve(relay),
      }}
    >
      <Usage id="wl_1" />
    </ReachProvider>,
  );
  return seen;
}

test("what the node reads now is shown beside what was accounted, each with its source", async () => {
  const seen = setup();
  const live = await screen.findByRole("region", { name: "Now" });
  expect(await within(live).findByText("512 MiB")).toBeTruthy();
  expect(within(live).getByText("1.5 cores")).toBeTruthy();
  expect(live.textContent).toContain("each rate over the 10 seconds before. Not billed");
  expect(within(live).queryByText("Disk"), "the node does not read disk at a reading").toBeNull();
  expect(seen.minted).toEqual(["metering.read"]);

  const accounted = screen.getByRole("region", { name: "Accounted" });
  // The day's sum, and its one measured record's own row.
  expect(within(accounted).getAllByText("90 s")).toHaveLength(2);
  expect(within(accounted).getAllByText("1 GiB-hours")).toHaveLength(2);
  expect(accounted.textContent).toContain(
    "1 interval measured, not counting 1 unheard over 1 hours",
  );
  expect(accounted.textContent).toContain("differenced between two samples");
  expect(within(accounted).queryByText("Network received"), "unmeasured, not zero").toBeNull();
  expect(seen.asked).toEqual(["wl_1"]);

  fireEvent.click(within(accounted).getByText("Each of the 2 records"));
  const gap = within(accounted).getByText("unheard: node unreachable").closest("tr");
  expect(gap?.className).toBe("gap");
  expect(gap?.textContent).toContain("-");
});

test("a caller who may not read the usage is told so, and no session is opened", async () => {
  const seen = setup(true);
  expect(await screen.findByRole("alert")).toBeTruthy();
  expect(screen.queryByRole("region", { name: "Now" })).toBeNull();
  expect(seen.minted).toEqual([]);
});
