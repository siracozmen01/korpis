// SPDX-License-Identifier: AGPL-3.0-or-later

// The console screen (§8 and §9 of docs/11-surface-web.md): the stream, gaps drawn as gaps, a
// marker for what left the window, older records on request, and a reading mode that holds the
// view still, without a live region, for a screen reader, telling how much arrived meanwhile.

import { useEffect, useMemo, useState } from "react";
import { Button, Form, Input, Label, TextField } from "react-aria-components";
import { useReach } from "../clients";
import { refusal } from "../views/refusal";
import "./console.css";
import { Writer } from "./input";
import type { Relay } from "./relay";
import { useConsole } from "./useConsole";
import type { Line, View } from "./view";

/** The console of workload `id`; typing is offered when `typing`. */
export function Console(props: { readonly id: string; readonly typing: boolean }) {
  const { clients, metered, relay } = useReach();
  const { id } = props;
  const c = useConsole({ clients, relay, workload: id, automatic: !metered });
  const [held, setHeld] = useState<View>();
  const [error, setError] = useState<string>();
  const shown = held ?? c.view;
  const waiting =
    held === undefined
      ? 0
      : c.view.lines.length - held.lines.length + (c.view.dropped - held.dropped);
  return (
    <section aria-labelledby="console">
      <h2 id="console">Console</h2>
      <p role="status">
        {c.live ? "Live." : c.ended === undefined ? "Opening…" : `Not live: ${c.ended}.`}
        {c.ended !== undefined && metered && (
          <Button onPress={c.reopen}>Open it again (uses one of the link's uses)</Button>
        )}
      </p>
      <Button
        onPress={() => {
          setHeld(held === undefined ? c.view : undefined);
        }}
      >
        {held === undefined
          ? "Reading mode"
          : `Back to live${waiting > 0 ? ` (${waiting.toString()} new lines)` : ""}`}
      </Button>
      <Button
        isDisabled={shown.first === undefined || shown.first === 0n}
        onPress={() => {
          c.earlier().catch((e: unknown) => {
            setError(refusal(e));
          });
        }}
      >
        Earlier
      </Button>
      {shown.dropped > 0 && (
        <p>{shown.dropped.toString()} older lines left the view; use Earlier to read them.</p>
      )}
      <pre
        className="console"
        role="log"
        aria-live={held === undefined ? "polite" : "off"}
        tabIndex={0}
      >
        {shown.lines.map((l) => (
          <LineView key={`${l.kind}:${l.offset.toString()}`} line={l} />
        ))}
      </pre>
      {props.typing && <Typing id={id} relay={relay} />}
      {error !== undefined && <p role="alert">{error}</p>}
    </section>
  );
}

function LineView({ line }: { readonly line: Line }) {
  if (line.kind === "gap") return <div className="console-gap">[{line.says}]</div>;
  return (
    <div className={line.source === "stderr" ? "console-stderr" : undefined}>
      {line.runs.map((r, i) => (
        <span key={i} className={r.classes === "" ? undefined : r.classes}>
          {r.text}
        </span>
      ))}
    </div>
  );
}

/** A line typed into the console. */
function Typing({ id, relay }: { readonly id: string; readonly relay: () => Promise<Relay> }) {
  const { clients } = useReach();
  const writer = useMemo(
    () => new Writer(async () => ({ clients, relay: await relay(), workload: id })),
    [clients, relay, id],
  );
  useEffect(
    () => () => {
      writer.close();
    },
    [writer],
  );
  const [line, setLine] = useState("");
  const [error, setError] = useState<string>();
  return (
    <Form
      onSubmit={(e) => {
        e.preventDefault();
        setError(undefined);
        writer.type(line).then(
          () => {
            setLine("");
          },
          (err: unknown) => {
            setError(refusal(err));
          },
        );
      }}
    >
      <TextField value={line} onChange={setLine} autoComplete="off">
        <Label>Send a command</Label>
        <Input />
      </TextField>
      <Button type="submit">Send</Button>
      {error !== undefined && <p role="alert">{error}</p>}
    </Form>
  );
}
