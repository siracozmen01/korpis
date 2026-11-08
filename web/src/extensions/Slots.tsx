// SPDX-License-Identifier: AGPL-3.0-or-later

// What installed extensions contribute to a slot of the panel: a card showing what one of an
// extension's actions answers, or a form invoking one. Each is drawn by the panel's own components
// from what the extension declared, never by the extension's code, so it cannot read the viewer's
// credential or imitate the panel; and the control plane lists only the contributions whose action
// the viewer may invoke here, authorizing each invocation again as it is made.

import { useCallback, useEffect, useState } from "react";
import {
  Button,
  CheckboxButton,
  CheckboxField,
  Form,
  Input,
  Label,
  TextField,
} from "react-aria-components";
import { useReach } from "../clients";
import type {
  ActionArgument,
  InvokeActionResponse,
  SlotContribution,
} from "../gen/korpis/v1/extension_pb";
import type { ActionValue } from "../gen/korpis/v1/provider_pb";
import { t } from "../i18n/intl";
import { refusal } from "../views/refusal";

/** The slot `slot` shown for `object`, a workload's identifier for its page and an organization's
 * for its settings. Nothing is drawn when nothing is contributed. */
export function Slots(props: { readonly slot: string; readonly object: string }) {
  const { clients } = useReach();
  const [contributions, setContributions] = useState<readonly SlotContribution[]>([]);
  useEffect(() => {
    let live = true;
    clients.extensions.listSlots({ slot: props.slot, object: props.object }).then(
      (r) => {
        if (live) setContributions(r.contributions);
      },
      () => {
        // A slot nobody may see is not there; the page says nothing of it.
        if (live) setContributions([]);
      },
    );
    return () => {
      live = false;
    };
  }, [clients, props.slot, props.object]);
  if (contributions.length === 0) return null;
  return (
    <>
      {contributions.map((c) => (
        <section key={`${c.extension} ${c.action?.name ?? ""}`} aria-label={c.title}>
          <h2>{c.title}</h2>
          {c.kind === "card" ? (
            <Card contribution={c} object={props.object} />
          ) : (
            <ActionForm contribution={c} object={props.object} />
          )}
        </section>
      ))}
    </>
  );
}

/** The argument the page's object is given as: the first workload argument, on a workload. */
function context(c: SlotContribution, object: string): ActionArgument | undefined {
  return object.startsWith("wl_")
    ? c.action?.arguments.find((a) => a.type === "workload")
    : undefined;
}

/** What one invocation answered: its line and its data, or why it did not. */
function Answer({ answer }: { readonly answer: Answered | undefined }) {
  if (answer === undefined) return null;
  if ("error" in answer) return <p role="alert">{answer.error}</p>;
  return (
    <>
      <p role="status">{answer.message}</p>
      {answer.data !== "" && <pre>{answer.data}</pre>}
    </>
  );
}

type Answered = { readonly message: string; readonly data: string } | { readonly error: string };

function answered(r: InvokeActionResponse): Answered {
  if (r.error !== undefined) return { error: `${r.error.reason}: ${r.error.message}` };
  return { message: r.result?.message ?? "", data: r.result?.data ?? "" };
}

/** Invokes `c`'s action with `values`, the page's object given as its context argument. */
function useInvoke(c: SlotContribution, object: string) {
  const { clients } = useReach();
  return useCallback(
    async (values: Record<string, ActionValue>): Promise<Answered> => {
      const arguments_: Record<string, ActionValue> = { ...values };
      const ctx = context(c, object);
      if (ctx !== undefined) {
        arguments_[ctx.name] = { value: { case: "workload", value: object } } as ActionValue;
      }
      try {
        const r = await clients.extensions.invokeAction({
          extension: c.extension,
          action: c.action?.name ?? "",
          arguments: arguments_,
        });
        return answered(r);
      } catch (e: unknown) {
        return { error: refusal(e) };
      }
    },
    [clients, c, object],
  );
}

/** A card: what the action answers, asked as it is shown and again when asked. */
function Card(props: { readonly contribution: SlotContribution; readonly object: string }) {
  const invoke = useInvoke(props.contribution, props.object);
  const [answer, setAnswer] = useState<Answered>();
  const ask = useCallback(() => {
    void invoke({}).then(setAnswer);
  }, [invoke]);
  useEffect(ask, [ask]);
  return (
    <>
      <Answer answer={answer} />
      <Button onPress={ask}>{t({ defaultMessage: "Ask again", id: "5P4VhhT2" })}</Button>
    </>
  );
}

/** A form of the action's arguments, but the page's own, invoking it when submitted. */
function ActionForm(props: { readonly contribution: SlotContribution; readonly object: string }) {
  const c = props.contribution;
  const invoke = useInvoke(c, props.object);
  const ctx = context(c, props.object);
  const shown = (c.action?.arguments ?? []).filter((a) => a.name !== ctx?.name);
  const [values, setValues] = useState<Record<string, string>>({});
  const [answer, setAnswer] = useState<Answered>();
  const submit = () => {
    const typed: Record<string, ActionValue> = {};
    for (const a of shown) {
      const v = values[a.name];
      if (v === undefined || v === "") continue;
      typed[a.name] = valueOf(a, v);
    }
    void invoke(typed).then(setAnswer);
  };
  return (
    <Form
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      {shown.map((a) =>
        a.type === "bool" ? (
          <CheckboxField
            key={a.name}
            isSelected={values[a.name] === "true"}
            onChange={(on) => {
              setValues({ ...values, [a.name]: on ? "true" : "false" });
            }}
          >
            <CheckboxButton>{a.name}</CheckboxButton>
          </CheckboxField>
        ) : (
          <TextField
            key={a.name}
            isRequired={a.required}
            value={values[a.name] ?? ""}
            onChange={(v) => {
              setValues({ ...values, [a.name]: v });
            }}
          >
            <Label>{a.name}</Label>
            <Input inputMode={a.type === "int" ? "numeric" : "text"} />
          </TextField>
        ),
      )}
      <Button type="submit">{c.action?.description ?? c.title}</Button>
      <Answer answer={answer} />
    </Form>
  );
}

/** `v` as the value of an argument of `a`'s type. */
function valueOf(a: ActionArgument, v: string): ActionValue {
  switch (a.type) {
    case "int":
      return { value: { case: "int", value: BigInt(v) } } as ActionValue;
    case "bool":
      return { value: { case: "flag", value: v === "true" } } as ActionValue;
    case "workload":
      return { value: { case: "workload", value: v } } as ActionValue;
    case "project":
      return { value: { case: "project", value: v } } as ActionValue;
    default:
      return { value: { case: "text", value: v } } as ActionValue;
  }
}
