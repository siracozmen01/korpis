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
import { bytes } from "../i18n/format";
import {
  PARENT,
  SEPARATOR,
  blobSink,
  download,
  parent,
  pickedSink,
  saveBlob,
  within,
} from "./transfer";
import { useFiles } from "./useFiles";
import { t } from "../i18n/intl";

const SAYS: Partial<Record<FileErrorCode, string>> = {
  [FileErrorCode.NOT_FOUND]: t({ defaultMessage: "It is not there.", id: "WRNdUk9E" }),
  [FileErrorCode.EXISTS]: t({
    defaultMessage: "Something by that name is there already.",
    id: "1KC4ynaR",
  }),
  [FileErrorCode.FULL]: t({ defaultMessage: "The volume is full.", id: "lxaBrkGd" }),
  [FileErrorCode.NOT_A_DIRECTORY]: t({
    defaultMessage: "A part of the path is not a directory.",
    id: "iQQgmW3M",
  }),
  [FileErrorCode.IS_A_DIRECTORY]: t({ defaultMessage: "It is a directory.", id: "DDAS95WS" }),
  [FileErrorCode.NOT_EMPTY]: t({ defaultMessage: "The directory is not empty.", id: "YgBq8K12" }),
  [FileErrorCode.OUTSIDE]: t({
    defaultMessage: "That is outside the workload's volumes.",
    id: "q7jGRPKA",
  }),
  [FileErrorCode.REFUSED]: t({ defaultMessage: "Not allowed here.", id: "9lS0MI/F" }),
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
  if (r.case !== "entries")
    throw new Error(
      t({ defaultMessage: "the node answered a listing with no entries", id: "WLC8/hBB" }),
    );
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
  // The directory last asked for, until it is shown: one asked for while no session is open, or
  // whose listing lost the session, is shown once another opens rather than dropped, and a
  // listing that arrives after another directory was asked for is not shown over it.
  const asked = useRef<string>(undefined);
  const show = useCallback(
    (s: FileSession, path: string, after = "") => {
      read(s, path, after).then(
        (page) => {
          if (asked.current !== undefined && asked.current !== path) return;
          asked.current = undefined;
          setListing((l) => ({
            path,
            entries: after === "" ? page.entries : [...l.entries, ...page.entries],
            next: page.next,
          }));
        },
        (e: unknown) => {
          // Refused, it is not asked again; lost, it is, once another session opens.
          if (!(e instanceof Ended) && asked.current === path) asked.current = undefined;
          failed(e);
        },
      );
    },
    [failed],
  );
  const go = (path: string) => {
    asked.current = path;
    setError(undefined);
    if (fs !== undefined) show(fs, path);
  };
  // The listing is read again whenever a session opens, at the directory asked for or else the
  // one last shown.
  useEffect(() => {
    if (fs !== undefined) show(fs, asked.current ?? shown.current.path);
  }, [fs, show]);
  const fetchFile = (e: FileEntry) => {
    if (fs === undefined) return;
    const path = within(listing.path, e.name);
    const total = Number(e.size);
    let got = 0;
    const run = async () => {
      const sink = (await pickedSink(e.name)) ?? blobSink(e.name, saveBlob);
      await download(fs, path, total, sink, (done) => {
        got = done;
        setStatus(
          t(
            { defaultMessage: "Downloading {name}: {done} of {total}", id: "nIafWUcM" },
            { name: e.name, done: bytes(done), total: bytes(total) },
          ),
        );
      });
      setStatus(t({ defaultMessage: "Downloaded {name}.", id: "aKY5Nsss" }, { name: e.name }));
    };
    run().catch((err: unknown) => {
      setStatus(
        t(
          {
            defaultMessage: "The download of {name} stopped at {done} of {total}.",
            id: "oZyTX6w6",
          },
          { name: e.name, done: bytes(got), total: bytes(total) },
        ),
      );
      failed(err);
    });
  };
  return (
    <section aria-labelledby="files">
      <h2 id="files">
        {t({ defaultMessage: "Files", id: "uBUvoH5J", description: "a workload's files" })}
      </h2>
      <p role="status">
        {fs !== undefined
          ? (status ??
            (props.writes
              ? t({
                  defaultMessage: "Open.",
                  id: "b3rCwi2e",
                  description: "a file session is open",
                })
              : t({
                  defaultMessage: "Open to read.",
                  id: "Usf2nt81",
                  description: "a read-only file session",
                })))
          : held.ended === undefined
            ? t({
                defaultMessage: "Opening…",
                id: "oxCCI1AN",
                description: "a file session opening",
              })
            : t({ defaultMessage: "Not open: {why}.", id: "5yTPpEp5" }, { why: held.ended })}
        {fs === undefined && held.ended !== undefined && (
          <Button onPress={held.reopen}>
            {metered
              ? t({ defaultMessage: "Open it again (uses one of the link's uses)", id: "acNLq/ZK" })
              : t({ defaultMessage: "Open it again", id: "T6KHmZeW" })}
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
            <th scope="col">
              {t({ defaultMessage: "Name", id: "c+plKHGA", description: "column: a file's name" })}
            </th>
            <th scope="col">
              {t({ defaultMessage: "Size", id: "bazDyBRA", description: "column: a file's size" })}
            </th>
            <th scope="col">
              {t({
                defaultMessage: "Modified",
                id: "FNNGdHd/",
                description: "column: when a file was modified",
              })}
            </th>
            {props.writes && (
              <th scope="col">
                {t({
                  defaultMessage: "Change",
                  id: "Nz+sqiZJ",
                  description: "column: changes to a file",
                })}
              </th>
            )}
          </tr>
        </thead>
        <tbody>
          {listing.path !== "/" && (
            <tr>
              <td>
                <Button
                  aria-label={t({ defaultMessage: "The directory above", id: "hkRoo7vP" })}
                  onPress={() => {
                    go(parent(listing.path));
                  }}
                >
                  {PARENT}
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
          {t({
            defaultMessage: "More",
            id: "KJXGpJqL",
            description: "button: read more of a directory",
          })}
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
    <nav
      aria-label={t({ defaultMessage: "Path", id: "Msrt5CyF", description: "a directory's path" })}
      className="crumbs"
    >
      <Button
        aria-label={t({ defaultMessage: "The top directory", id: "xQ+xIgWb" })}
        onPress={() => {
          props.go("/");
        }}
      >
        {SEPARATOR}
      </Button>
      {parts.map((p, i) => (
        <Button
          key={parts.slice(0, i + 1).join("/")}
          onPress={() => {
            props.go(`/${parts.slice(0, i + 1).join("/")}`);
          }}
        >
          {p}
          {SEPARATOR}
        </Button>
      ))}
    </nav>
  );
}
