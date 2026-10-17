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
import { t } from "../i18n/intl";
import { refusal } from "../views/refusal";
import { ALLOWS } from "./allows";

/** What a link made here may allow. */
const OFFERED: readonly string[] = [
  "workload.read",
  "workload.console.read",
  "workload.console.write",
  "workload.restart",
  "workload.start",
  "workload.stop",
  "workload.file.read",
];

const hoursOf = (n: number) =>
  t({ defaultMessage: "{n, plural, one {# hour} other {# hours}}", id: "57WbSh0o" }, { n });

/** How long a link may last, in hours. */
const LASTS: readonly (readonly [string, number])[] = [
  [hoursOf(1), 1],
  [hoursOf(24), 24],
  [t({ defaultMessage: "{n, plural, one {# day} other {# days}}", id: "jYLOYRR/" }, { n: 7 }), 168],
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
      <h2 id="share">{t({ defaultMessage: "Share as a link", id: "7L9bcZ1z" })}</h2>
      <Form
        onSubmit={(event) => {
          event.preventDefault();
          issue();
        }}
      >
        <CheckboxGroup value={actions} onChange={setActions} isRequired>
          <Label>{t({ defaultMessage: "They can", id: "C0bpQ4YI" })}</Label>
          {OFFERED.map((action) => (
            <CheckboxField key={action} value={action}>
              <CheckboxButton>{ALLOWS[action]}</CheckboxButton>
            </CheckboxField>
          ))}
        </CheckboxGroup>
        <RadioGroup value={hours} onChange={setHours}>
          <Label>
            {t({
              defaultMessage: "For",
              id: "stvCF2Qv",
              description: "how long a share link lasts",
            })}
          </Label>
          {LASTS.map(([words, h]) => (
            <RadioField key={h} value={h.toString()}>
              <RadioButton>{words}</RadioButton>
            </RadioField>
          ))}
        </RadioGroup>
        <NumberField value={uses} onChange={setUses} minValue={1} maxValue={1000}>
          <Label>
            {t({
              defaultMessage: "Uses",
              id: "ZInTaFOM",
              description: "how many uses a share link has",
            })}
          </Label>
          <Input />
        </NumberField>
        <TextField value={label} onChange={setLabel}>
          <Label>
            {t({ defaultMessage: "What it is for (shown wherever it is listed)", id: "gFzZ9t+1" })}
          </Label>
          <Input />
        </TextField>
        <p>
          {t(
            {
              defaultMessage:
                "Whoever holds the link can do this, with no account. Opening its screen is one use, and so is each action; using it needs the control plane to be reachable. It lasts at most {uses, plural, one {# use} other {# uses}} and {lasts}, and you can revoke it at any time.",
              id: "rFlt7Wwo",
            },
            { uses, lasts: LASTS.find(([, h]) => h.toString() === hours)?.[0] ?? "" },
          )}
        </p>
        <Button type="submit" isDisabled={actions.length === 0}>
          {t({ defaultMessage: "Make the link", id: "FAkmp8n+" })}
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
      <h2 id="issued">{t({ defaultMessage: "The link", id: "1gwFNojj" })}</h2>
      <p>
        {t({
          defaultMessage:
            "It is shown this once. Send it only to the person it is for, not to a group.",
          id: "lfun+oaM",
        })}
      </p>
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
        {copied
          ? t({ defaultMessage: "Copied", id: "p556q3uv" })
          : t({ defaultMessage: "Copy", id: "4l6vz1/e" })}
      </Button>
    </section>
  );
}
