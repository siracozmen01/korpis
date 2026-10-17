// SPDX-License-Identifier: AGPL-3.0-or-later

import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { createClient, createRouterTransport } from "@connectrpc/connect";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { clientsOver, ReachProvider } from "../clients";
import { GrantService } from "../gen/korpis/v1/api_pb";
import {
  ConsoleChunkSchema,
  ConsoleInputSchema,
  ConsoleReadSchema,
  StreamRecordKind,
} from "../gen/korpis/v1/console_pb";
import { RelayService } from "../gen/korpis/v1/relay_pb";
import { Console } from "./Console";

const encode = (s: string) => new TextEncoder().encode(s);

function setup() {
  const seen: { reads: bigint[]; typed: string[]; minted: string[] } = {
    reads: [],
    typed: [],
    minted: [],
  };
  let more: (() => void) | undefined;
  const transport = createRouterTransport(({ service }) => {
    service(GrantService, {
      mintToken: (r) => {
        seen.minted.push(r.actions.join());
        return { token: `tok-${r.actions.join()}` };
      },
    });
    service(RelayService, {
      async *watch(r) {
        yield { message: { case: "opened", value: { session: "s1", sendKey: "k1" } } };
        if (r.action === "workload.console.write") {
          await new Promise(() => undefined);
          return;
        }
        seen.reads.push(fromBinary(ConsoleReadSchema, r.params).last);
        const chunk = create(ConsoleChunkSchema, {
          records: [
            {
              offset: 7n,
              kind: StreamRecordKind.TEXT,
              source: "stdout",
              body: encode("\u001b[32mready\u001b[0m"),
            },
            { offset: 8n, kind: StreamRecordKind.GAP, gap: { records: 3n, reason: "too fast" } },
          ],
          nextOffset: 12n,
        });
        yield {
          message: {
            case: "frame",
            value: { frame: { case: "data", value: toBinary(ConsoleChunkSchema, chunk) } },
          },
        };
        await new Promise<void>((resolve) => {
          more = resolve;
        });
        const later = create(ConsoleChunkSchema, {
          records: [
            { offset: 12n, kind: StreamRecordKind.TEXT, source: "stdout", body: encode("later") },
          ],
          nextOffset: 13n,
        });
        yield {
          message: {
            case: "frame",
            value: { frame: { case: "data", value: toBinary(ConsoleChunkSchema, later) } },
          },
        };
        await new Promise(() => undefined);
      },
      send: (r) => {
        for (const f of r.frames) {
          if (f.frame.case === "data") {
            const input = fromBinary(ConsoleInputSchema, f.frame.value).input;
            if (input.case === "data") seen.typed.push(new TextDecoder().decode(input.value));
          }
        }
        return { delivered: r.first + BigInt(r.frames.length) };
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
      <Console id="wl_1" typing />
    </ReachProvider>,
  );
  return { seen, more: () => more?.() };
}

test("the console streams styled lines and draws a gap where records were lost", async () => {
  const { seen } = setup();
  expect(await screen.findByText("ready")).toHaveProperty("className", "ansi-fg-2");
  expect(screen.getByText("[3 records lost: too fast]")).toBeTruthy();
  expect(screen.getByRole("status").textContent).toContain("Live.");
  expect(seen.reads).toEqual([200n]);
  expect(seen.minted).toEqual(["workload.console.read"]);
});

test("reading mode holds the view still and says how much arrived meanwhile", async () => {
  const { more } = setup();
  await screen.findByText("ready");
  fireEvent.click(screen.getByRole("button", { name: "Reading mode" }));
  expect(screen.getByRole("log").getAttribute("aria-live")).toBe("off");
  more();
  expect(await screen.findByRole("button", { name: "Back to live (1 new lines)" })).toBeTruthy();
  expect(screen.queryByText("later")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: /Back to live/ }));
  expect(screen.getByText("later")).toBeTruthy();
});

test("a typed line goes through a writing session of its own", async () => {
  const { seen } = setup();
  await screen.findByText("ready");
  fireEvent.change(screen.getByLabelText("Send a command"), { target: { value: "say hi" } });
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await waitFor(() => {
    expect(seen.typed).toEqual(["say hi\n"]);
  });
  expect(seen.minted).toEqual(["workload.console.read", "workload.console.write"]);
});
