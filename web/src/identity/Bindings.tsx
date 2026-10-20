// SPDX-License-Identifier: AGPL-3.0-or-later

// The identity bindings declared at an organization (§5 and §5.1 of docs/08-identity.md, §5 of
// docs/11-surface-web.md): each person or set an identity provider installed there vouches for,
// with its state, when its provider last confirmed it, and its interval and grace together. A
// signed-in caller holding `identity.binding.write` there binds a person or a set, edits a
// binding's interval and grace against the version shown, asks its provider now, and retires it.
// Nothing here is offered through a share link.

import { useCallback, useState } from "react";
import { Button } from "react-aria-components";
import { useReach } from "../clients";
import { BindingState, type IdentityBinding } from "../gen/korpis/v1/authority_pb";
import { t } from "../i18n/intl";
import { everyGrant, holds, upward } from "../views/held";
import { useLoad } from "../views/load";
import { PAGE_SIZE } from "../views/Pages";
import { refusal } from "../views/refusal";
import { Shown } from "../views/Shown";
import { Editor } from "./BindingEditor";
import { longestMinutes, minutes, subjectText } from "./identity";

/** A binding's state as a person reads it. */
function stateOf(b: IdentityBinding): string {
  if (b.retiredAt !== undefined) return t({ defaultMessage: "retired", id: "pQA40tdT" });
  return b.state === BindingState.VERIFIED
    ? t({ defaultMessage: "verified", id: "RTHCUGR4" })
    : t({ defaultMessage: "unverified", id: "mVCuSoEt" });
}

/** When it was last confirmed, its interval and grace, and the longest they let it stand. */
function described(b: IdentityBinding): string {
  const confirmed =
    b.lastVerifiedAt === undefined
      ? t({ defaultMessage: "Never confirmed yet.", id: "oC5qt97I" })
      : t(
          { defaultMessage: "Last confirmed {at}.", id: "YbussuOe" },
          { at: new Date(Number(b.lastVerifiedAt.seconds) * 1000).toISOString() },
        );
  const asked = t(
    {
      defaultMessage:
        "Asked every {interval} minutes with {grace} minutes of grace: an unreachable provider extends it at most {longest} minutes past its last confirmation.",
      id: "L0CqJYP/",
    },
    {
      interval: String(minutes(b.reverifyInterval) ?? "-"),
      grace: String(minutes(b.constraints?.grace) ?? "-"),
      longest: String(longestMinutes(b) ?? "-"),
    },
  );
  return `${confirmed} ${asked}`;
}

function Binding(props: {
  readonly binding: IdentityBinding;
  readonly organization: string;
  readonly writes: boolean;
  readonly changed: () => void;
}) {
  const { clients } = useReach();
  const { binding: b, organization, writes, changed } = props;
  const [editing, setEditing] = useState(false);
  const [said, setSaid] = useState<string>();
  const act = async (ask: () => Promise<string | undefined>) => {
    setSaid(undefined);
    try {
      const unanswered = await ask();
      if (unanswered !== undefined && unanswered !== "") {
        setSaid(
          t(
            { defaultMessage: "Its provider did not answer: {why}", id: "Qgzl1qJN" },
            { why: unanswered },
          ),
        );
        return;
      }
      changed();
    } catch (e) {
      setSaid(refusal(e));
    }
  };
  const live = b.retiredAt === undefined;
  return (
    <li>
      <strong>{subjectText(b.subject)}</strong>
      <small className="aside">
        {t(
          { defaultMessage: "at {provider}, {state}", id: "RuANMV6H" },
          { provider: b.provider, state: stateOf(b) },
        )}
      </small>
      <p>{described(b)}</p>
      {writes && live && !editing && (
        <>
          <Button
            onPress={() =>
              void act(
                async () => (await clients.identity.verifyIdentityBinding({ id: b.id })).unanswered,
              )
            }
          >
            {t({ defaultMessage: "Ask its provider now", id: "NeP9qItg" })}
          </Button>
          <Button
            onPress={() => {
              setEditing(true);
            }}
          >
            {t({ defaultMessage: "Edit", id: "wEQDC6Wv" })}
          </Button>
          <Button
            onPress={() =>
              void act(async () => {
                await clients.identity.retireIdentityBinding({
                  mutation: { idempotencyKey: crypto.randomUUID() },
                  id: b.id,
                });
                return undefined;
              })
            }
          >
            {t({ defaultMessage: "Retire it", id: "ttY/jSVX" })}
          </Button>
        </>
      )}
      {editing && (
        <Editor
          organization={organization}
          binding={b}
          done={() => {
            setEditing(false);
            changed();
          }}
        />
      )}
      {said !== undefined && <p role="alert">{said}</p>}
    </li>
  );
}

/** Every binding declared at `organization`, every page of them. */
async function declared(
  clients: ReturnType<typeof useReach>["clients"],
  organization: string,
): Promise<IdentityBinding[]> {
  const found: IdentityBinding[] = [];
  let pageToken = "";
  do {
    const r = await clients.identity.listIdentityBindings({
      organization,
      page: { pageSize: PAGE_SIZE, pageToken },
    });
    found.push(...r.bindings);
    pageToken = r.page?.nextPageToken ?? "";
  } while (pageToken !== "");
  return found;
}

/** The bindings declared at `organization`, read again whenever one changes, by remounting. */
export function Bindings({ organization }: { readonly organization: string }) {
  const [round, setRound] = useState(0);
  return (
    <BindingsRead
      key={round}
      organization={organization}
      changed={() => {
        setRound((n) => n + 1);
      }}
    />
  );
}

function BindingsRead(props: { readonly organization: string; readonly changed: () => void }) {
  const { clients, allowed } = useReach();
  const { organization, changed } = props;
  const bindings = useLoad(
    useCallback(async () => declared(clients, organization), [clients, organization]),
  );
  const writes = useLoad(
    useCallback(async () => {
      if (allowed !== undefined) return false;
      const [grants, above] = await Promise.all([
        everyGrant(clients),
        upward(clients, organization),
      ]);
      return holds(grants, "identity.binding.write", above);
    }, [clients, allowed, organization]),
  );
  const writer = writes.state === "done" && writes.value;
  const [making, setMaking] = useState(false);
  return (
    <section aria-labelledby="bindings">
      <h2 id="bindings">{t({ defaultMessage: "Identity bindings", id: "H+ArmwnP" })}</h2>
      <Shown
        loaded={bindings}
        reading={t({
          defaultMessage: "Reading the identity bindings declared here…",
          id: "rLD07YnN",
        })}
        done={(list) =>
          list.length === 0 ? (
            <p>{t({ defaultMessage: "No person or set is bound here.", id: "CcFwK8UF" })}</p>
          ) : (
            <ul>
              {list.map((b) => (
                <Binding
                  key={b.id}
                  binding={b}
                  organization={organization}
                  writes={writer}
                  changed={changed}
                />
              ))}
            </ul>
          )
        }
      />
      {writer && !making && (
        <Button
          onPress={() => {
            setMaking(true);
          }}
        >
          {t({ defaultMessage: "Bind a person or a set", id: "L+NetjuJ" })}
        </Button>
      )}
      {making && (
        <Editor
          organization={organization}
          done={() => {
            setMaking(false);
            changed();
          }}
        />
      )}
    </section>
  );
}
