// SPDX-License-Identifier: AGPL-3.0-or-later

// The file manager's parts that change things: a row with its rename and its remove, which asks
// first and says when a directory goes with all it holds; and the directory's own changes, a new
// directory and uploads, an upload onto a smaller file of the same name offered to resume from
// what is there or to replace it.

import { useState } from "react";
import { Button, Form, Input, Label, TextField } from "react-aria-components";
import { FileKind, type FileEntry } from "../gen/korpis/v1/files_pb";
import type { FileSession } from "./session";
import { bytes, partial, upload, within } from "./transfer";

const KIND: Record<FileKind, string> = {
  [FileKind.UNSPECIFIED]: "",
  [FileKind.FILE]: "file",
  [FileKind.DIRECTORY]: "directory",
  [FileKind.SYMLINK]: "link, not followed",
  [FileKind.OTHER]: "not a file",
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
            aria-label={e.kind === FileKind.DIRECTORY ? `Open ${e.name}` : `Download ${e.name}`}
            onPress={props.open}
          >
            {e.kind === FileKind.DIRECTORY ? `${e.name}/` : e.name}
          </Button>
        ) : (
          <span>{e.name}</span>
        )}{" "}
        <small>{KIND[e.kind]}</small>
      </td>
      <td>{e.kind === FileKind.FILE ? bytes(Number(e.size)) : ""}</td>
      <td>{modified === "" ? "" : modified.toLocaleString()}</td>
      {fs !== undefined && (
        <td>
          {doing === undefined && (
            <>
              <Button
                aria-label={`Rename ${e.name}`}
                onPress={() => {
                  setDoing("rename");
                }}
              >
                Rename
              </Button>
              <Button
                aria-label={`Remove ${e.name}`}
                onPress={() => {
                  setDoing("remove");
                }}
              >
                Remove
              </Button>
            </>
          )}
          {doing === "rename" && (
            <Named
              label={`New name for ${e.name}`}
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
            <span role="group" aria-label={`Remove ${e.name}`}>
              {e.kind === FileKind.DIRECTORY
                ? `Remove ${e.name} and everything in it? This cannot be undone.`
                : `Remove ${e.name}? This cannot be undone.`}
              <Button
                onPress={() => {
                  const recursive = e.kind === FileKind.DIRECTORY;
                  act((s) => s.ask({ case: "remove", value: { path, recursive } }));
                }}
              >
                Remove
              </Button>
              <Button
                onPress={() => {
                  setDoing(undefined);
                }}
              >
                Cancel
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
        Save
      </Button>
      <Button onPress={props.cancel}>Cancel</Button>
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
      tell(`Uploading ${file.name}: ${bytes(done)} of ${bytes(total)}`);
    });
    tell(`Uploaded ${file.name}.`);
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
        Upload files
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
          label="New directory"
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
          New directory
        </Button>
      )}
      {asking !== undefined && (
        <div role="group" aria-label={`Upload ${asking.file.name}`}>
          {`${asking.file.name} is there with ${bytes(asking.there)} of ${bytes(asking.file.size)}.`}
          <Button
            onPress={() => {
              decide(asking.there);
            }}
          >
            Resume
          </Button>
          <Button
            onPress={() => {
              decide(0);
            }}
          >
            Replace
          </Button>
          <Button
            onPress={() => {
              decide(undefined);
            }}
          >
            Skip
          </Button>
        </div>
      )}
    </div>
  );
}
