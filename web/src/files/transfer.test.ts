// SPDX-License-Identifier: AGPL-3.0-or-later

import { expect, test } from "vitest";
import type { Asked, FileSession, Said } from "./session";
import { bytes } from "../i18n/format";
import { download, parent, upload, within } from "./transfer";

/** A chunk small enough that the tests move bytes, not mebibytes. */
const CHUNK = 8;

/** A session over one file, answering each request after the ones before it. */
function over(file: { data: Uint8Array }) {
  const asked: Asked[] = [];
  let inFlight = 0;
  let most = 0;
  const ask = async (r: Asked): Promise<Said> => {
    asked.push(r);
    inFlight += 1;
    most = Math.max(most, inFlight);
    await new Promise((resolve) => setTimeout(resolve, 1));
    inFlight -= 1;
    if (r.case === "write") {
      const v = r.value;
      const data = v.data ?? new Uint8Array();
      const at = Number(v.offset ?? 0n);
      const base = v.truncate === true ? new Uint8Array() : file.data;
      const next = new Uint8Array(Math.max(base.length, at + data.length));
      next.set(base);
      next.set(data, at);
      file.data = next;
      return { case: "written", value: BigInt(data.length) };
    }
    if (r.case === "read") {
      const at = Number(r.value.offset ?? 0n);
      return { case: "data", value: file.data.slice(at, at + (r.value.length ?? 0)) };
    }
    return { case: undefined, value: undefined };
  };
  return { fs: { ask } as unknown as FileSession, asked, most: () => most };
}

const big = new Uint8Array(CHUNK * 2 + 5).map((_, i) => i % 251);

test("an upload moves a chunk per request, a few ahead, replacing only from its start", async () => {
  const file = { data: new Uint8Array([9, 9, 9]) };
  const { fs, asked, most } = over(file);
  const seen: number[] = [];
  await upload(
    fs,
    new Blob([big]),
    "/data/world.zip",
    0,
    (done) => {
      seen.push(done);
    },
    CHUNK,
  );
  expect(file.data).toEqual(big);
  const writes = asked.flatMap((r) => (r.case === "write" ? [r.value] : []));
  // Each at the end, so one lost leaves the rest refused rather than written past a hole.
  expect(writes.map((w) => [w.offset, w.data?.length, w.truncate, w.atEnd])).toEqual([
    [0n, CHUNK, true, true],
    [BigInt(CHUNK), CHUNK, false, true],
    [BigInt(CHUNK * 2), 5, false, true],
  ]);
  expect(most()).toBeGreaterThan(1);
  expect(seen.at(-1)).toBe(big.length);
});

test("a resumed upload sends only what is not there", async () => {
  const file = { data: big.slice(0, CHUNK) };
  const { fs, asked } = over(file);
  await upload(fs, new Blob([big]), "/w", CHUNK, () => undefined, CHUNK);
  expect(file.data).toEqual(big);
  const offsets = asked.flatMap((r) => (r.case === "write" ? [r.value.offset] : []));
  expect(offsets).toEqual([BigInt(CHUNK), BigInt(CHUNK * 2)]);
});

test("a download arrives in order, and a file that shrank ends where it ends", async () => {
  const { fs } = over({ data: big });
  const got: Uint8Array[] = [];
  let closed = false;
  const sink = {
    write: (c: Uint8Array) => {
      got.push(c);
      return Promise.resolve();
    },
    close: () => {
      closed = true;
      return Promise.resolve();
    },
  };
  await download(fs, "/w", big.length + CHUNK, sink, () => undefined, CHUNK);
  const joined = new Uint8Array(got.reduce((n, c) => n + c.length, 0));
  let at = 0;
  for (const c of got) {
    joined.set(c, at);
    at += c.length;
  }
  expect(joined).toEqual(big);
  expect(closed).toBe(true);
});

test("paths and sizes read as a person reads them", () => {
  expect(within("/", "data")).toBe("/data");
  expect(within("/data", "a")).toBe("/data/a");
  expect(parent("/data/a")).toBe("/data");
  expect(parent("/data")).toBe("/");
  expect(bytes(512)).toBe("512 B");
  expect(bytes(3 * 1024 * 1024)).toBe("3 MiB");
});
