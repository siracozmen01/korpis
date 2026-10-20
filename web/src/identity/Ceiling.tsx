// SPDX-License-Identifier: AGPL-3.0-or-later

// The delegation ceiling at an organization (§5.1 of docs/08-identity.md, §5 of
// docs/11-surface-web.md): the longest a grant beneath a set subject lasts there, the one in
// force beside the organization's own. A signed-in caller holding `org.write` there sets or
// removes its own; below the root the control plane refuses one above what is in force above,
// in its words.

import { useCallback, useState } from "react";
import { Button, Form, Input, Label, TextField } from "react-aria-components";
import { useReach } from "../clients";
import { t } from "../i18n/intl";
import { everyGrant, holds, upward } from "../views/held";
import { useLoad } from "../views/load";
import { refusal } from "../views/refusal";
import { Shown } from "../views/Shown";
import { days, ofDays } from "./identity";

/** The ceiling at `organization`, read again whenever it changes, by remounting. */
export function Ceiling({ organization }: { readonly organization: string }) {
  const [round, setRound] = useState(0);
  return (
    <CeilingRead
      key={round}
      organization={organization}
      changed={() => {
        setRound((n) => n + 1);
      }}
    />
  );
}

function CeilingRead(props: { readonly organization: string; readonly changed: () => void }) {
  const { clients, allowed } = useReach();
  const { organization, changed } = props;
  const ceiling = useLoad(
    useCallback(async () => {
      const r = await clients.organizations.getDelegationCeiling({ organization });
      return r.ceiling;
    }, [clients, organization]),
  );
  const writes = useLoad(
    useCallback(async () => {
      if (allowed !== undefined) return false;
      const [grants, above] = await Promise.all([
        everyGrant(clients),
        upward(clients, organization),
      ]);
      return holds(grants, "org.write", above);
    }, [clients, allowed, organization]),
  );
  const [text, setText] = useState("");
  const [said, setSaid] = useState<string>();
  const set = async (to: bigint | undefined) => {
    setSaid(undefined);
    try {
      await clients.organizations.setDelegationCeiling({
        mutation: { idempotencyKey: crypto.randomUUID() },
        organization,
        ...(to === undefined ? {} : { ceiling: ofDays(to) }),
      });
      changed();
    } catch (e) {
      setSaid(refusal(e));
    }
  };
  return (
    <section aria-labelledby="ceiling">
      <h2 id="ceiling">{t({ defaultMessage: "Delegation ceiling", id: "6Mby31OS" })}</h2>
      <Shown
        loaded={ceiling}
        reading={t({ defaultMessage: "Reading the delegation ceiling…", id: "VrQtwEUx" })}
        done={(c) => (
          <p>
            {t(
              {
                defaultMessage:
                  "A grant passed on beneath a set lasts at most {effective, plural, one {# day} other {# days}} from its issue here.",
                id: "gN4aX+eV",
              },
              { effective: Number(days(c?.effective) ?? 7n) },
            )}
          </p>
        )}
      />
      <Shown
        loaded={ceiling}
        reading=""
        done={(c) => (
          <p>
            {c?.own === undefined
              ? t({ defaultMessage: "This organization sets none of its own.", id: "ntlnaUQN" })
              : t(
                  {
                    defaultMessage:
                      "This organization sets {own, plural, one {# day} other {# days}} of its own.",
                    id: "4WUq97/2",
                  },
                  { own: Number(days(c.own)) },
                )}
          </p>
        )}
      />
      {writes.state === "done" && writes.value && (
        <Form
          aria-label={t({ defaultMessage: "Set the delegation ceiling", id: "tH3NtNAa" })}
          onSubmit={(event) => {
            event.preventDefault();
            const n = text.trim();
            if (!/^\d+$/.test(n) || BigInt(n) < 1n) {
              setSaid(
                t({
                  defaultMessage: "A ceiling is a whole number of days from 1.",
                  id: "nH30CoiV",
                }),
              );
              return;
            }
            void set(BigInt(n));
          }}
        >
          <TextField value={text} onChange={setText}>
            <Label>
              {t({ defaultMessage: "Days a grant passed on beneath a set lasts", id: "UK0SIM1f" })}
            </Label>
            <Input />
          </TextField>
          <Button type="submit">{t({ defaultMessage: "Set it", id: "WNmQ7tuE" })}</Button>
          <Button onPress={() => void set(undefined)}>
            {t({ defaultMessage: "Remove this organization's own", id: "YjqQlIWk" })}
          </Button>
          {said !== undefined && <p role="alert">{said}</p>}
        </Form>
      )}
    </section>
  );
}
