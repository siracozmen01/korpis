// SPDX-License-Identifier: AGPL-3.0-or-later

import { create, fromBinary, toBinary, type MessageInitShape } from "@bufbuild/protobuf";
import { Code, ConnectError, createClient, createRouterTransport } from "@connectrpc/connect";
import { fireEvent, render, screen, waitFor, within as inside } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";
import { clientsOver, ReachProvider } from "../clients";
import { GrantService } from "../gen/korpis/v1/api_pb";
import {
  FileErrorCode,
  FileKind,
  FileRequestSchema,
  FileResponseSchema,
  type FileRequest,
} from "../gen/korpis/v1/files_pb";
import { RelayService } from "../gen/korpis/v1/relay_pb";
import { FileManager } from "./FileManager";

type Node = Uint8Array | "dir";
type Answer = NonNullable<MessageInitShape<typeof FileResponseSchema>["response"]>;

/** A workload's files, as a node's worker would answer for them. */
function answer(files: Map<string, Node>, r: FileRequest): Answer {
  const refused = (code: FileErrorCode): Answer => ({ case: "error", value: { code } });
  const q = r.request;
  switch (q.case) {
    case "list": {
      const prefix = q.value.path === "/" ? "/" : `${q.value.path}/`;
      const entries = [...files.keys()]
        .filter(
          (p) => p.startsWith(prefix) && p !== prefix && !p.slice(prefix.length).includes("/"),
        )
        .sort()
        .map((p) => {
          const n = files.get(p);
          const kind = n === "dir" ? FileKind.DIRECTORY : FileKind.FILE;
          const size = n === "dir" || n === undefined ? 0n : BigInt(n.length);
          return { name: p.slice(prefix.length), kind, size };
        });
      return { case: "entries", value: { entries, next: "" } };
    }
    case "stat": {
      const n = files.get(q.value.path);
      if (n === undefined) return refused(FileErrorCode.NOT_FOUND);
      const kind = n === "dir" ? FileKind.DIRECTORY : FileKind.FILE;
      return { case: "entry", value: { kind, size: n === "dir" ? 0n : BigInt(n.length) } };
    }
    case "read": {
      const n = files.get(q.value.path);
      if (n === undefined || n === "dir") return refused(FileErrorCode.NOT_FOUND);
      const at = Number(q.value.offset);
      return { case: "data", value: n.slice(at, at + q.value.length) };
    }
    case "write": {
      const old = files.get(q.value.path);
      if (old === "dir") return refused(FileErrorCode.IS_A_DIRECTORY);
      const base = q.value.truncate || old === undefined ? new Uint8Array() : old;
      const at = Number(q.value.offset);
      const next = new Uint8Array(Math.max(base.length, at + q.value.data.length));
      next.set(base);
      next.set(q.value.data, at);
      files.set(q.value.path, next);
      return { case: "written", value: BigInt(q.value.data.length) };
    }
    case "mkdir":
      files.set(q.value.path, "dir");
      return { case: "done", value: true };
    case "rename": {
      const n = files.get(q.value.from);
      if (n === undefined) return refused(FileErrorCode.NOT_FOUND);
      files.delete(q.value.from);
      files.set(q.value.to, n);
      return { case: "done", value: true };
    }
    case "remove":
      for (const p of [...files.keys()])
        if (p === q.value.path || p.startsWith(`${q.value.path}/`)) files.delete(p);
      return { case: "done", value: true };
    default:
      return refused(FileErrorCode.REFUSED);
  }
}

/** The file manager over `files`, writing if `writes`; the first request `lose` matches is lost
 * on its way to the relay, as on a network that drops it. */
function setup(writes: boolean, files: Map<string, Node>, lose?: (r: FileRequest) => boolean) {
  let lost = false;
  const seen: { requests: FileRequest[]; minted: string[] } = { requests: [], minted: [] };
  const answers: Uint8Array[] = [];
  let wake: (() => void) | undefined;
  const transport = createRouterTransport(({ service }) => {
    service(GrantService, {
      mintToken: (r) => {
        seen.minted.push(r.actions.join());
        return { token: "tok", route: "r" };
      },
    });
    service(RelayService, {
      async *watch() {
        yield { message: { case: "opened", value: { session: "s1", sendKey: "k1" } } };
        for (;;) {
          const next = answers.shift();
          if (next === undefined) {
            await new Promise<void>((resolve) => {
              wake = resolve;
            });
            continue;
          }
          yield { message: { case: "frame", value: { frame: { case: "data", value: next } } } };
        }
      },
      send: (r) => {
        for (const f of r.frames) {
          if (f.frame.case !== "data") continue;
          const request = fromBinary(FileRequestSchema, f.frame.value);
          if (!lost && lose?.(request) === true) {
            lost = true;
            throw new ConnectError("the network dropped it", Code.Unavailable);
          }
          seen.requests.push(request);
          const response = create(FileResponseSchema, {
            id: request.id,
            response: answer(files, request),
          });
          answers.push(toBinary(FileResponseSchema, response));
        }
        wake?.();
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
      <FileManager id="wl_1" writes={writes} />
    </ReachProvider>,
  );
  return seen;
}

const bytesOf = (s: string) => new TextEncoder().encode(s);

afterEach(() => {
  vi.restoreAllMocks();
});

test("a directory is listed, entered, and a file in it downloaded as it streams", async () => {
  const files = new Map<string, Node>([
    ["/data", "dir"],
    ["/data/eula.txt", bytesOf("eula=true")],
  ]);
  let saved: Blob | undefined;
  Object.assign(URL, {
    createObjectURL: (b: Blob) => {
      saved = b;
      return "blob:x";
    },
    revokeObjectURL: () => undefined,
  });
  vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => undefined);
  const seen = setup(false, files);
  fireEvent.click(await screen.findByRole("button", { name: "Open data" }));
  fireEvent.click(await screen.findByRole("button", { name: "Download eula.txt" }));
  await waitFor(() => {
    expect(saved).toBeDefined();
  });
  expect(await saved?.text()).toBe("eula=true");
  expect(seen.minted).toEqual(["workload.file.read"]);
  expect(screen.queryByText("Upload files")).toBeNull();
  expect(screen.queryByRole("button", { name: "Remove eula.txt" })).toBeNull();
});

test("a directory opened as its session is lost is shown once another opens", async () => {
  const files = new Map<string, Node>([
    ["/data", "dir"],
    ["/data/eula.txt", bytesOf("eula=true")],
  ]);
  setup(false, files, (r) => r.request.case === "list" && r.request.value.path === "/data");
  fireEvent.click(await screen.findByRole("button", { name: "Open data" }));
  // The session that lost the request is closed, and the next opens after its pause.
  const entered = await screen.findByRole(
    "button",
    { name: "Download eula.txt" },
    {
      timeout: 8_000,
    },
  );
  expect(entered).toBeDefined();
  expect(screen.getByRole("navigation", { name: "Path" }).textContent).toContain("data");
}, 10_000);

test("an upload onto a smaller file of its name resumes from what is there", async () => {
  const files = new Map<string, Node>([["/world.zip", bytesOf("012")]]);
  const seen = setup(true, files);
  await screen.findByRole("button", { name: "Download world.zip" });
  const file = new File(["01234567"], "world.zip");
  fireEvent.change(screen.getByLabelText("Upload files"), { target: { files: [file] } });
  const asking = await screen.findByRole("group", { name: "Upload world.zip" });
  expect(asking.textContent).toContain("3 B of 8 B");
  fireEvent.click(inside(asking).getByRole("button", { name: "Resume" }));
  await waitFor(() => {
    expect(new TextDecoder().decode(files.get("/world.zip") as Uint8Array)).toBe("01234567");
  });
  const writes = seen.requests.flatMap((r) =>
    r.request.case === "write" ? [r.request.value] : [],
  );
  expect(writes.map((w) => [w.offset, new TextDecoder().decode(w.data), w.truncate])).toEqual([
    [3n, "34567", false],
  ]);
  expect(seen.minted).toEqual(["workload.file.write"]);
});

test("a directory is made, a file renamed, and a removal asked about first", async () => {
  const files = new Map<string, Node>([["/a.txt", bytesOf("a")]]);
  setup(true, files);
  await screen.findByRole("button", { name: "Download a.txt" });
  fireEvent.click(screen.getByRole("button", { name: "New directory" }));
  fireEvent.change(screen.getByLabelText("New directory"), { target: { value: "plugins" } });
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  expect(await screen.findByRole("button", { name: "Open plugins" })).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: "Rename a.txt" }));
  fireEvent.change(screen.getByLabelText("New name for a.txt"), { target: { value: "b.txt" } });
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  expect(await screen.findByRole("button", { name: "Download b.txt" })).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: "Remove b.txt" }));
  const asked = screen.getByRole("group", { name: "Remove b.txt" });
  expect(files.has("/b.txt")).toBe(true);
  expect(asked.textContent).toContain("cannot be undone");
  fireEvent.click(inside(asked).getByRole("button", { name: "Remove" }));
  await waitFor(() => {
    expect(files.has("/b.txt")).toBe(false);
  });
});
