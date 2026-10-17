// SPDX-License-Identifier: AGPL-3.0-or-later

// The file manager's parts that change things: a row with its rename and its remove, which asks
// first and says when a directory goes with all it holds; and the directory's own changes, a new
// directory and uploads, an upload onto a smaller file of the same name offered to resume from
// what is there or to replace it.

import { useState } from "react";
import { Button, Form, Input, Label, TextField } from "react-aria-components";
import { FileKind, type FileEntry } from "../gen/korpis/v1/files_pb";
import type { FileSession } from "./session";
import { bytes } from "../i18n/format";
import { SEPARATOR, partial, upload, within } from "./transfer";
import { intl, t } from "../i18n/intl";

const KIND: Record<FileKind, string> = {
  [FileKind.UNSPECIFIED]: "",
  [FileKind.FILE]: t({
    defaultMessage: "file",
    id: "/GKa1+lg",
    description: "a listing entry's kind",
  }),
  [FileKind.DIRECTORY]: t({
    defaultMessage: "directory",
    id: "KU6egau4",
    description: "a listing entry's kind",
  }),
  [FileKind.SYMLINK]: t({
    defaultMessage: "link, not followed",
    id: "z03iN+NR",
    description: "a listing entry's kind",
  }),
  [FileKind.OTHER]: t({
    defaultMessage: "not a file",
    id: "aM06WstJ",
    description: "a listing entry's kind",
  }),
};

/** One entry of the listing; changes offered when `fs` is given. */
export function Row(props: {
  readonly entry: FileEntry;
  readonly fs: FileSession | undefined;
  readonly dir: string;
  readonly open: () => void;
  readonly refresh: () => void;
  readonly failed: (e: unknown) => void;
}) {
  const { entry: e, fs } = props;
  const [doing, setDoing] = useState<"rename" | "remove">();
  const path = within(props.dir, e.name);
  const opens = e.kind === FileKind.DIRECTORY || e.kind === FileKind.FILE;
  const act = (work: (s: FileSession) => Promise<unknown>) => {
    if (fs === undefined) return;
    work(fs).then(() => {
      setDoing(undefined);
      props.refresh();
    }, props.failed);
  };
  const modified = e.modified === undefined ? "" : new Date(Number(e.modified.seconds) * 1000);
  return (
    <tr>
      <td>
        {opens ? (
          <Button
            aria-label={
              e.kind === FileKind.DIRECTORY
                ? t({ defaultMessage: "Open {name}", id: "X1BDgPIM" }, { name: e.name })
                : t({ defaultMessage: "Download {name}", id: "R3ptDDpo" }, { name: e.name })
            }
            onPress={props.open}
          >
            {e.name}
            {e.kind === FileKind.DIRECTORY && SEPARATOR}
          </Button>
        ) : (
          <span>{e.name}</span>
        )}
        <small className="aside">{KIND[e.kind]}</small>
      </td>
      <td>{e.kind === FileKind.FILE ? bytes(Number(e.size)) : ""}</td>
      <td>
        {modified === ""
          ? ""
          : intl.formatDate(modified, { dateStyle: "medium", timeStyle: "short" })}
      </td>
      {fs !== undefined && (
        <td>
          {doing === undefined && (
            <>
              <Button
                aria-label={t(
                  { defaultMessage: "Rename {name}", id: "NZmMEEju" },
                  { name: e.name },
                )}
                onPress={() => {
                  setDoing("rename");
                }}
              >
                {t({
                  defaultMessage: "Rename",
                  id: "rxv4RRs4",
                  description: "button: rename a file",
                })}
              </Button>
              <Button
                aria-label={t(
                  { defaultMessage: "Remove {name}", id: "T0e6Lh8l" },
                  { name: e.name },
                )}
                onPress={() => {
                  setDoing("remove");
                }}
              >
                {t({
                  defaultMessage: "Remove",
                  id: "Nf2GrGr0",
                  description: "button: remove a file",
                })}
              </Button>
            </>
          )}
          {doing === "rename" && (
            <Named
              label={t({ defaultMessage: "New name for {name}", id: "zYuUqeRO" }, { name: e.name })}
              initial={e.name}
              done={(name) => {
                act((s) =>
                  s.ask({ case: "rename", value: { from: path, to: within(props.dir, name) } }),
                );
              }}
              cancel={() => {
                setDoing(undefined);
              }}
            />
          )}
          {doing === "remove" && (
            <span
              role="group"
              aria-label={t({ defaultMessage: "Remove {name}", id: "T0e6Lh8l" }, { name: e.name })}
            >
              {e.kind === FileKind.DIRECTORY
                ? t(
                    {
                      defaultMessage: "Remove {name} and everything in it? This cannot be undone.",
                      id: "miozhqcP",
                    },
                    { name: e.name },
                  )
                : t(
                    { defaultMessage: "Remove {name}? This cannot be undone.", id: "rGyY3Tcv" },
                    { name: e.name },
                  )}
              <Button
                onPress={() => {
                  const recursive = e.kind === FileKind.DIRECTORY;
                  act((s) => s.ask({ case: "remove", value: { path, recursive } }));
                }}
              >
                {t({
                  defaultMessage: "Remove",
                  id: "Nf2GrGr0",
                  description: "button: remove a file",
                })}
              </Button>
              <Button
                onPress={() => {
                  setDoing(undefined);
                }}
              >
                {t({ defaultMessage: "Cancel", id: "47FYwba+" })}
              </Button>
            </span>
          )}
        </td>
      )}
    </tr>
  );
}

/** A name asked for in a small form. */
function Named(props: {
  readonly label: string;
  readonly initial: string;
  readonly done: (name: string) => void;
  readonly cancel: () => void;
}) {
  const [name, setName] = useState(props.initial);
  const plain = name !== "" && name !== "." && name !== ".." && !name.includes("/");
  return (
    <Form
      onSubmit={(ev) => {
        ev.preventDefault();
        if (plain) props.done(name);
      }}
    >
      <TextField value={name} onChange={setName} isInvalid={!plain}>
        <Label>{props.label}</Label>
        <Input />
      </TextField>
      <Button type="submit" isDisabled={!plain}>
        {t({ defaultMessage: "Save", id: "jvo0vs3n" })}
      </Button>
      <Button onPress={props.cancel}>{t({ defaultMessage: "Cancel", id: "47FYwba+" })}</Button>
    </Form>
  );
}

/** An upload waiting on the person: the file is there with fewer bytes. */
interface Asking {
  readonly file: File;
  readonly there: number;
  readonly rest: File[];
}

/** The directory's own changes: a new directory, and uploads into it. */
export function Changes(props: {
  readonly fs: FileSession;
  readonly dir: string;
  readonly refresh: () => void;
  readonly failed: (e: unknown) => void;
  readonly tell: (status: string) => void;
}) {
  const { fs, dir, tell } = props;
  const [making, setMaking] = useState(false);
  const [asking, setAsking] = useState<Asking>();
  const send = async (file: File, from: number) => {
    await upload(fs, file, within(dir, file.name), from, (done, total) => {
      tell(
        t(
          { defaultMessage: "Uploading {name}: {done} of {total}", id: "qyvx6JZq" },
          { name: file.name, done: bytes(done), total: bytes(total) },
        ),
      );
    });
    tell(t({ defaultMessage: "Uploaded {name}.", id: "k/D6s5T/" }, { name: file.name }));
  };
  /** Uploads `files` in turn, stopping to ask about one already partly there. */
  const each = async (files: File[]) => {
    for (const [i, file] of files.entries()) {
      const there = await partial(fs, within(dir, file.name), file.size);
      if (there !== undefined) {
        setAsking({ file, there, rest: files.slice(i + 1) });
        return;
      }
      await send(file, 0);
    }
    props.refresh();
  };
  const run = (work: () => Promise<void>) => {
    work().catch(props.failed);
  };
  const decide = (from: number | undefined) => {
    if (asking === undefined) return;
    const { file, rest } = asking;
    setAsking(undefined);
    run(async () => {
      if (from !== undefined) await send(file, from);
      await each(rest);
    });
  };
  return (
    <div className="changes">
      <label className="upload">
        {t({ defaultMessage: "Upload files", id: "2jjdVzJX" })}
        <input
          type="file"
          multiple
          disabled={asking !== undefined}
          onChange={(ev) => {
            const files = Array.from(ev.target.files ?? []);
            ev.target.value = "";
            run(() => each(files));
          }}
        />
      </label>
      {making ? (
        <Named
          label={t({ defaultMessage: "New directory", id: "c6s3fip6" })}
          initial=""
          done={(name) => {
            run(async () => {
              await fs.ask({ case: "mkdir", value: { path: within(dir, name) } });
              setMaking(false);
              props.refresh();
            });
          }}
          cancel={() => {
            setMaking(false);
          }}
        />
      ) : (
        <Button
          onPress={() => {
            setMaking(true);
          }}
        >
          {t({ defaultMessage: "New directory", id: "c6s3fip6" })}
        </Button>
      )}
      {asking !== undefined && (
        <div
          role="group"
          aria-label={t(
            { defaultMessage: "Upload {name}", id: "H+C4LITW" },
            { name: asking.file.name },
          )}
        >
          {t(
            { defaultMessage: "{name} is there with {there} of {size}.", id: "Od6W77Tm" },
            { name: asking.file.name, there: bytes(asking.there), size: bytes(asking.file.size) },
          )}
          <Button
            onPress={() => {
              decide(asking.there);
            }}
          >
            {t({
              defaultMessage: "Resume",
              id: "RimURWP3",
              description: "button: resume an upload",
            })}
          </Button>
          <Button
            onPress={() => {
              decide(0);
            }}
          >
            {t({
              defaultMessage: "Replace",
              id: "SzKCJJnE",
              description: "button: replace a partly uploaded file",
            })}
          </Button>
          <Button
            onPress={() => {
              decide(undefined);
            }}
          >
            {t({ defaultMessage: "Skip", id: "sBDwIptW", description: "button: skip an upload" })}
          </Button>
        </div>
      )}
    </div>
  );
}
