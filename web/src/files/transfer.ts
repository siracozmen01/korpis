// SPDX-License-Identifier: AGPL-3.0-or-later

// Moving a file's bytes over a file session (§3.2 of docs/06-storage.md and §8 of
// docs/11-surface-web.md): a mebibyte per request, a few requests sent ahead of their answers so
// a distant relay does not set the pace, and never the whole file in the tab. An upload resumes
// from the size a stat reports; a download is written to the file the browser lets the person
// pick as it arrives, or, where a browser offers no such file, kept as a Blob, which the browser
// may hold on disk rather than in memory, and saved at the end.

import { FileErrorCode, FileKind } from "../gen/korpis/v1/files_pb";
import { type FileSession, Refused } from "./session";

/** The most bytes one request moves. */
export const CHUNK = 1 << 20;
/** The most requests sent ahead of their answers. */
const AHEAD = 4;

/** Told how many bytes of how many are done. */
export type Progress = (done: number, total: number) => void;

/** The size of the file at `path` if it is there and smaller than `size`: where an upload of
 * `size` bytes could resume from. Whether it is the same file is the person's to say. */
export async function partial(
  fs: FileSession,
  path: string,
  size: number,
): Promise<number | undefined> {
  try {
    const said = await fs.ask({ case: "stat", value: { path } });
    if (said.case !== "entry" || said.value.kind !== FileKind.FILE) return undefined;
    const there = Number(said.value.size);
    return there > 0 && there < size ? there : undefined;
  } catch (e) {
    if (e instanceof Refused && e.code === FileErrorCode.NOT_FOUND) return undefined;
    throw e;
  }
}

/** Waits for the oldest of `pending` while more than `most` are; all of them when `most` is 0. */
async function settle(pending: Promise<void>[], most: number): Promise<void> {
  while (pending.length > most) await pending.shift();
}

/** Writes `file` to `path` from byte `from`, `chunk` bytes a request; from 0, it replaces what is
 * there. */
export async function upload(
  fs: FileSession,
  file: Blob,
  path: string,
  from: number,
  progress: Progress,
  chunk = CHUNK,
): Promise<void> {
  let done = from;
  progress(done, file.size);
  if (file.size === 0) {
    const value = { path, data: new Uint8Array(), create: true, truncate: true };
    await fs.ask({ case: "write", value });
    return;
  }
  const pending: Promise<void>[] = [];
  for (let at = from; at < file.size; at += chunk) {
    const data = new Uint8Array(await file.slice(at, at + chunk).arrayBuffer());
    const value = { path, offset: BigInt(at), data, create: true, truncate: at === 0 };
    const written = fs.ask({ case: "write", value }).then((said) => {
      if (said.case !== "written" || Number(said.value) !== data.length)
        throw new Error(`the node wrote less than was sent at ${at.toString()}`);
      done += data.length;
      progress(done, file.size);
    });
    // A failure is thrown where it is awaited, below; until then it is not unhandled.
    written.catch(() => undefined);
    pending.push(written);
    await settle(pending, AHEAD);
  }
  await settle(pending, 0);
}

/** Where a download is written. */
export interface Sink {
  write(chunk: Uint8Array): Promise<void>;
  close(): Promise<void>;
}

/** Reads `size` bytes of `path` into `sink`, in order, `chunk` bytes a request. */
export async function download(
  fs: FileSession,
  path: string,
  size: number,
  sink: Sink,
  progress: Progress,
  chunk = CHUNK,
): Promise<void> {
  const read = (at: number) =>
    fs.ask({ case: "read", value: { path, offset: BigInt(at), length: chunk } }).then((said) => {
      if (said.case !== "data") throw new Error("the node answered a read with no data");
      return said.value;
    });
  const ahead: Promise<Uint8Array>[] = [];
  let asked = 0;
  let done = 0;
  progress(done, size);
  while (done < size) {
    while (asked < size && ahead.length < AHEAD) {
      const r = read(asked);
      r.catch(() => undefined);
      ahead.push(r);
      asked += chunk;
    }
    const next = ahead.shift();
    if (next === undefined) break;
    const data = await next;
    // A file that shrank while it was read ends where it now ends.
    if (data.length === 0) break;
    await sink.write(data);
    done += data.length;
    progress(done, size);
  }
  await sink.close();
}

/** A sink keeping the download as a Blob, saved as `name` when it is closed. */
export function blobSink(name: string, save: (blob: Blob, name: string) => void): Sink {
  const parts: BlobPart[] = [];
  return {
    write: (chunk) => {
      parts.push(chunk.slice());
      return Promise.resolve();
    },
    close: () => {
      save(new Blob(parts), name);
      return Promise.resolve();
    },
  };
}

/** Saves `blob` as `name` through a link the browser downloads. */
export function saveBlob(blob: Blob, name: string): void {
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.click();
  setTimeout(() => {
    URL.revokeObjectURL(url);
  }, 60_000);
}

interface Picked {
  createWritable(): Promise<{ write(b: BufferSource): Promise<void>; close(): Promise<void> }>;
}
type Picker = (o: { suggestedName: string }) => Promise<Picked>;

/** A sink writing to a file the person picks, where the browser offers one; none otherwise. */
export async function pickedSink(name: string): Promise<Sink | undefined> {
  const picker = (window as unknown as { showSaveFilePicker?: Picker }).showSaveFilePicker;
  if (picker === undefined) return undefined;
  const writable = await (await picker({ suggestedName: name })).createWritable();
  return {
    write: (chunk) => writable.write(chunk.slice()),
    close: () => writable.close(),
  };
}

/** `name` in the directory `dir`. */
export function within(dir: string, name: string): string {
  return dir === "/" ? `/${name}` : `${dir}/${name}`;
}

/** The directory `path` is in. */
export function parent(path: string): string {
  const at = path.lastIndexOf("/");
  return at <= 0 ? "/" : path.slice(0, at);
}

/** A byte count as a person reads it. */
export function bytes(n: number): string {
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let v = n;
  let u = 0;
  while (v >= 1024 && u < units.length - 1) {
    v /= 1024;
    u += 1;
  }
  return u === 0 ? `${n.toString()} B` : `${v.toFixed(1)} ${units[u] ?? "B"}`;
}
