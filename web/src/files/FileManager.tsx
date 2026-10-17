// SPDX-License-Identifier: AGPL-3.0-or-later

// The file manager (§8 of docs/11-surface-web.md): a client of the workload's file session and
// nothing more, holding no privilege of its own. It lists a directory a page at a time, downloads
// a file as it streams, uploads in mebibyte chunks that resume from the size already written,
// makes directories, renames, and removes after a confirmation; changes are offered only where
// the caller may write.

import { useCallback, useEffect, useRef, useState } from "react";
import { Button } from "react-aria-components";
import { useReach } from "../clients";
import { FileErrorCode, FileKind, type FileEntry } from "../gen/korpis/v1/files_pb";
import { refusal } from "../views/refusal";
import "./files.css";
import { Changes, Row } from "./parts";
import { Ended, type FileSession, Refused } from "./session";
import { blobSink, bytes, download, parent, pickedSink, saveBlob, within } from "./transfer";
import { useFiles } from "./useFiles";

const SAYS: Partial<Record<FileErrorCode, string>> = {
  [FileErrorCode.NOT_FOUND]: "It is not there.",
  [FileErrorCode.EXISTS]: "Something by that name is there already.",
  [FileErrorCode.FULL]: "The volume is full.",
  [FileErrorCode.NOT_A_DIRECTORY]: "A part of the path is not a directory.",
  [FileErrorCode.IS_A_DIRECTORY]: "It is a directory.",
  [FileErrorCode.NOT_EMPTY]: "The directory is not empty.",
  [FileErrorCode.OUTSIDE]: "That is outside the workload's volumes.",
  [FileErrorCode.REFUSED]: "Not allowed here.",
};

/** What a failed request says to the person who asked. */
export function said(e: unknown): string {
  if (e instanceof Refused) return SAYS[e.code] ?? e.message;
  return refusal(e);
}

/** A directory's page, and where the next starts. */
interface Listing {
  readonly path: string;
  readonly entries: FileEntry[];
  readonly next: string;
}

/** A page of directory `path` after `after`. */
async function read(s: FileSession, path: string, after: string): Promise<Listing> {
  const r = await s.ask({ case: "list", value: { path, after, limit: 200 } });
  if (r.case !== "entries") throw new Error("the node answered a listing with no entries");
  return { path, entries: r.value.entries, next: r.value.next };
}

/** The files of workload `id`; changes offered when `writes`. */
export function FileManager(props: { readonly id: string; readonly writes: boolean }) {
  const { clients, relay, metered } = useReach();
  const held = useFiles({ clients, relay, workload: props.id }, props.writes, !metered);
  const { fs, lost } = held;
  const [listing, setListing] = useState<Listing>({ path: "/", entries: [], next: "" });
  const [status, setStatus] = useState<string>();
  const [error, setError] = useState<string>();
  const failed = useCallback(
    (e: unknown) => {
      if (e instanceof Ended) lost(e.message);
      else setError(said(e));
    },
    [lost],
  );
  const shown = useRef(listing);
  useEffect(() => {
    shown.current = listing;
  }, [listing]);
  const show = useCallback(
    (s: FileSession, path: string, after = "") => {
      read(s, path, after).then((page) => {
        setListing((l) => ({
          path,
          entries: after === "" ? page.entries : [...l.entries, ...page.entries],
          next: page.next,
        }));
      }, failed);
    },
    [failed],
  );
  const go = (path: string) => {
    if (fs === undefined) return;
    setError(undefined);
    show(fs, path);
  };
  // The listing is read again whenever a session opens, at the directory last shown.
  useEffect(() => {
    if (fs !== undefined) show(fs, shown.current.path);
  }, [fs, show]);
  const fetchFile = (e: FileEntry) => {
    if (fs === undefined) return;
    const path = within(listing.path, e.name);
    const total = Number(e.size);
    const run = async () => {
      const sink = (await pickedSink(e.name)) ?? blobSink(e.name, saveBlob);
      await download(fs, path, total, sink, (done) => {
        setStatus(`Downloading ${e.name}: ${bytes(done)} of ${bytes(total)}`);
      });
      setStatus(`Downloaded ${e.name}.`);
    };
    run().catch(failed);
  };
  return (
    <section aria-labelledby="files">
      <h2 id="files">Files</h2>
      <p role="status">
        {fs !== undefined
          ? (status ?? (props.writes ? "Open." : "Open to read."))
          : held.ended === undefined
            ? "Opening…"
            : `Not open: ${held.ended}.`}
        {fs === undefined && held.ended !== undefined && (
          <Button onPress={held.reopen}>
            {metered ? "Open it again (uses one of the link's uses)" : "Open it again"}
          </Button>
        )}
      </p>
      <Crumbs path={listing.path} go={go} />
      {fs !== undefined && props.writes && (
        <Changes
          fs={fs}
          dir={listing.path}
          refresh={() => {
            go(listing.path);
          }}
          failed={failed}
          tell={setStatus}
        />
      )}
      <table className="files">
        <thead>
          <tr>
            <th scope="col">Name</th>
            <th scope="col">Size</th>
            <th scope="col">Modified</th>
            {props.writes && <th scope="col">Change</th>}
          </tr>
        </thead>
        <tbody>
          {listing.path !== "/" && (
            <tr>
              <td>
                <Button
                  onPress={() => {
                    go(parent(listing.path));
                  }}
                >
                  ..
                </Button>
              </td>
            </tr>
          )}
          {listing.entries.map((e) => (
            <Row
              key={e.name}
              entry={e}
              fs={props.writes ? fs : undefined}
              dir={listing.path}
              open={() => {
                if (e.kind === FileKind.DIRECTORY) go(within(listing.path, e.name));
                else if (e.kind === FileKind.FILE) fetchFile(e);
              }}
              refresh={() => {
                go(listing.path);
              }}
              failed={failed}
            />
          ))}
        </tbody>
      </table>
      {listing.next !== "" && fs !== undefined && (
        <Button
          onPress={() => {
            show(fs, listing.path, listing.next);
          }}
        >
          More
        </Button>
      )}
      {error !== undefined && <p role="alert">{error}</p>}
    </section>
  );
}

/** The path shown, each directory on it a way back to it. */
function Crumbs(props: { readonly path: string; readonly go: (path: string) => void }) {
  const parts = props.path.split("/").filter((p) => p !== "");
  return (
    <nav aria-label="Path" className="crumbs">
      <Button
        onPress={() => {
          props.go("/");
        }}
      >
        /
      </Button>
      {parts.map((p, i) => (
        <Button
          key={parts.slice(0, i + 1).join("/")}
          onPress={() => {
            props.go(`/${parts.slice(0, i + 1).join("/")}`);
          }}
        >
          {p}/
        </Button>
      ))}
    </nav>
  );
}
