// SPDX-License-Identifier: AGPL-3.0-or-later

// Sharing a workload as a link (§6 of docs/11-surface-web.md, §6.1 of docs/08-identity.md): what
// the holder may do, for how long, and how many uses, with what a use is and that using it needs
// the control plane said before the link exists. The link is a link-form grant, answered once as
// `/g/#korpislk_…`: shown here once, never stored by the panel, and revoked like any grant. A
// link is never offered to a share link's holder, since a link can never make further links.

import { timestampFromMs } from "@bufbuild/protobuf/wkt";
import { useState } from "react";
import {
  Button,
  CheckboxButton,
  CheckboxField,
  CheckboxGroup,
  Form,
  Input,
  Label,
  NumberField,
  RadioButton,
  RadioField,
  RadioGroup,
  TextField,
} from "react-aria-components";
import { useReach } from "../clients";
import { refusal } from "../views/refusal";

/** What a link may allow, in the words the share screen uses. */
const OFFERED: readonly (readonly [string, string])[] = [
  ["workload.read", "see its status"],
  ["workload.console.read", "read its console"],
  ["workload.console.write", "send commands to its console"],
  ["workload.restart", "restart it"],
  ["workload.start", "start it"],
  ["workload.stop", "stop it"],
  ["workload.file.read", "read its files"],
];

/** How long a link may last, in hours. */
const LASTS: readonly (readonly [string, number])[] = [
  ["1 hour", 1],
  ["24 hours", 24],
  ["7 days", 168],
];

const HOUR_MS = 3_600_000;

/** The share screen for workload `id`. */
export function Share({ id }: { readonly id: string }) {
  const { clients } = useReach();
  const [actions, setActions] = useState<string[]>(["workload.read", "workload.console.read"]);
  const [hours, setHours] = useState("24");
  const [uses, setUses] = useState(3);
  const [label, setLabel] = useState("");
  const [link, setLink] = useState<string>();
  const [error, setError] = useState<string>();

  const issue = () => {
    setError(undefined);
    const expires = Date.now() + Number(hours) * HOUR_MS;
    clients.grants
      .issueGrant({
        mutation: { idempotencyKey: crypto.randomUUID() },
        actions,
        scope: { kind: { case: "workload", value: id } },
        conditions: { expiresAt: timestampFromMs(expires), maxUses: BigInt(uses) },
        linkForm: true,
        label,
      })
      .then(
        (r) => {
          setLink(`${window.location.origin}/g/#${r.linkToken}`);
        },
        (e: unknown) => {
          setError(refusal(e));
        },
      );
  };

  if (link !== undefined) return <Issued link={link} />;
  return (
    <section aria-labelledby="share">
      <h2 id="share">Share as a link</h2>
      <Form
        onSubmit={(event) => {
          event.preventDefault();
          issue();
        }}
      >
        <CheckboxGroup value={actions} onChange={setActions} isRequired>
          <Label>They can</Label>
          {OFFERED.map(([action, words]) => (
            <CheckboxField key={action} value={action}>
              <CheckboxButton>{words}</CheckboxButton>
            </CheckboxField>
          ))}
        </CheckboxGroup>
        <RadioGroup value={hours} onChange={setHours}>
          <Label>For</Label>
          {LASTS.map(([words, h]) => (
            <RadioField key={h} value={h.toString()}>
              <RadioButton>{words}</RadioButton>
            </RadioField>
          ))}
        </RadioGroup>
        <NumberField value={uses} onChange={setUses} minValue={1} maxValue={1000}>
          <Label>Uses</Label>
          <Input />
        </NumberField>
        <TextField value={label} onChange={setLabel}>
          <Label>What it is for (shown wherever it is listed)</Label>
          <Input />
        </TextField>
        <p>
          Whoever holds the link can do this, with no account. Opening its screen is one use, and so
          is each action; using it needs the control plane to be reachable. It lasts at most{" "}
          {uses.toString()} uses and {LASTS.find(([, h]) => h.toString() === hours)?.[0]}, and you
          can revoke it at any time.
        </p>
        <Button type="submit" isDisabled={actions.length === 0}>
          Make the link
        </Button>
      </Form>
      {error !== undefined && <p role="alert">{error}</p>}
    </section>
  );
}

/** The link, shown once. */
function Issued({ link }: { readonly link: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <section aria-labelledby="issued">
      <h2 id="issued">The link</h2>
      <p>It is shown this once. Send it only to the person it is for, not to a group.</p>
      <p>
        <code>{link}</code>
      </p>
      <Button
        onPress={() => {
          void navigator.clipboard.writeText(link).then(() => {
            setCopied(true);
          });
        }}
      >
        {copied ? "Copied" : "Copy"}
      </Button>
    </section>
  );
}
