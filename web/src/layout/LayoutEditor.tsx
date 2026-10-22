// SPDX-License-Identifier: AGPL-3.0-or-later

// An organization's landing screen, composed (question 2 of §10 of docs/11-surface-web.md): a
// heading and the panel's own components in order, saved against the version it was read at, so
// two editors never silently overwrite each other, and removed, so the nearest ancestor's applies
// again. What the viewer's grants open is always placed and is moved but never removed. As for a
// theme, who may change it is the control plane's to say: the form is offered to whoever opens
// the organization, and a refusal says why.

import { create } from "@bufbuild/protobuf";
import { useEffect, useState } from "react";
import { Button, Form, Input, Label, TextArea, TextField } from "react-aria-components";
import { useReach } from "../clients";
import { type Layout, type LayoutItem, LayoutSchema } from "../gen/korpis/v1/tenancy_pb";
import { t } from "../i18n/intl";
import { refusal } from "../views/refusal";
import { keyOf } from "./landing";
import { described, hasQuota, link, moved, notice, places, quota, removed } from "./items";

/** What the editor holds: the draft, the version it was read at, and whose layout applies. */
interface Held {
  readonly draft: Layout;
  readonly version: bigint;
  readonly own: boolean;
}

/** A text field. */
function Text(props: {
  readonly label: string;
  readonly value: string;
  readonly set: (v: string) => void;
  readonly long?: boolean;
}) {
  return (
    <TextField value={props.value} onChange={props.set}>
      <Label>{props.label}</Label>
      {props.long === true ? <TextArea /> : <Input />}
    </TextField>
  );
}

/** A form adding a notice or a link to the draft. */
function Adding(props: {
  readonly kind: "notice" | "link";
  readonly add: (i: LayoutItem) => void;
}) {
  const [first, setFirst] = useState("");
  const [second, setSecond] = useState("");
  const isNotice = props.kind === "notice";
  return (
    <Form
      aria-label={
        isNotice
          ? t({ defaultMessage: "Add a notice", id: "3oi5Zjgo" })
          : t({ defaultMessage: "Add a link", id: "Bem7D1vo" })
      }
      onSubmit={(e) => {
        e.preventDefault();
        props.add(isNotice ? notice(first.trim(), second) : link(first.trim(), second.trim()));
        setFirst("");
        setSecond("");
      }}
    >
      <Text
        label={
          isNotice
            ? t({ defaultMessage: "Notice heading", id: "8/w/gS+c" })
            : t({ defaultMessage: "Link label", id: "g+VKkonE" })
        }
        value={first}
        set={setFirst}
      />
      <Text
        label={
          isNotice
            ? t({ defaultMessage: "Notice text", id: "C20BOa/N" })
            : t({ defaultMessage: "Link address (https://…)", id: "9B2qDwoy" })
        }
        value={second}
        set={setSecond}
        long={isNotice}
      />
      <Button type="submit">
        {isNotice
          ? t({ defaultMessage: "Add the notice", id: "dsXbmI+3" })
          : t({ defaultMessage: "Add the link", id: "CxqXCQvg" })}
      </Button>
    </Form>
  );
}

/** The draft's items, each moved or removed in place. */
function Items(props: {
  readonly items: readonly LayoutItem[];
  readonly set: (items: LayoutItem[]) => void;
}) {
  const { items, set } = props;
  return (
    <ol>
      {items.map((item, at) => {
        const what = described(item);
        return (
          <li key={keyOf(item, at)} className="layout-item">
            <span>{what}</span>
            <Button
              aria-label={t({ defaultMessage: "Move up: {what}", id: "OS0ZXSYI" }, { what })}
              isDisabled={at === 0}
              onPress={() => {
                set(moved(items, at, -1));
              }}
            >
              {t({ defaultMessage: "Up", id: "3H4xP3lY", description: "move an item up" })}
            </Button>
            <Button
              aria-label={t({ defaultMessage: "Move down: {what}", id: "2sxUZYvz" }, { what })}
              isDisabled={at === items.length - 1}
              onPress={() => {
                set(moved(items, at, 1));
              }}
            >
              {t({ defaultMessage: "Down", id: "wWOah4PS", description: "move an item down" })}
            </Button>
            {item.kind.case !== "places" && (
              <Button
                aria-label={t({ defaultMessage: "Remove: {what}", id: "ztRJ6N7u" }, { what })}
                onPress={() => {
                  set(removed(items, at));
                }}
              >
                {t({ defaultMessage: "Remove", id: "WLzsHoQ8", description: "remove an item" })}
              </Button>
            )}
          </li>
        );
      })}
    </ol>
  );
}

/** The landing screen editor of organization `id`. */
export function LayoutEditor({ id }: { readonly id: string }) {
  const { clients } = useReach();
  const [held, setHeld] = useState<Held>();
  const [said, setSaid] = useState<string>();
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let gone = false;
    clients.organizations.getLayout({ of: { kind: { case: "organization", value: id } } }).then(
      (r) => {
        if (gone) return;
        const g = r.governing;
        const own = g?.setOn === id && g.layout !== undefined;
        const draft = own ? g.layout : create(LayoutSchema, { items: [places()] });
        setHeld({ draft: draft ?? create(LayoutSchema, {}), version: g?.ownVersion ?? 0n, own });
      },
      (e: unknown) => {
        if (!gone) setSaid(refusal(e));
      },
    );
    return () => {
      gone = true;
    };
  }, [clients, id, attempt]);
  if (held === undefined) return said === undefined ? null : <p role="alert">{said}</p>;
  const edit = (change: Partial<Layout>) => {
    setHeld({ ...held, draft: create(LayoutSchema, { ...held.draft, ...change }) });
  };
  const save = (layout: Layout | undefined) => {
    const expectedVersion = held.version > 0n ? held.version : undefined;
    clients.organizations
      .setLayout({
        mutation: { idempotencyKey: crypto.randomUUID(), expectedVersion },
        organization: id,
        layout,
      })
      .then(
        () => {
          setSaid(
            layout === undefined
              ? t({
                  defaultMessage: "Removed; the nearest landing screen above applies.",
                  id: "IsMgH6JL",
                })
              : t({ defaultMessage: "Saved.", id: "GrnZDNX+" }),
          );
          setAttempt((n) => n + 1);
        },
        (e: unknown) => {
          setSaid(refusal(e));
        },
      );
  };
  const items = held.draft.items;
  return (
    <section aria-labelledby="layout">
      <h2 id="layout">{t({ defaultMessage: "Landing screen", id: "MAWWMW0y" })}</h2>
      <p>
        {held.own
          ? t({
              defaultMessage:
                "This organization's own landing screen, shown to those it serves unless an organization beneath sets its own. Every part of it shows each viewer only what they can reach.",
              id: "MVcbe/H0",
            })
          : t({
              defaultMessage:
                "This organization shows the landing screen of the nearest organization above that composes one, or the panel's own.",
              id: "lUejsUUp",
            })}
      </p>
      <Text
        label={t({ defaultMessage: "Heading of the landing screen", id: "1CuPMaoT" })}
        value={held.draft.name}
        set={(name) => {
          edit({ name });
        }}
      />
      <Items
        items={items}
        set={(next) => {
          edit({ items: next });
        }}
      />
      <Adding
        kind="notice"
        add={(i) => {
          edit({ items: [...items, i] });
        }}
      />
      <Adding
        kind="link"
        add={(i) => {
          edit({ items: [...items, i] });
        }}
      />
      {!hasQuota(items) && (
        <Button
          onPress={() => {
            edit({ items: [...items, quota()] });
          }}
        >
          {t({ defaultMessage: "Add the quota", id: "iOcrZf4w" })}
        </Button>
      )}
      <Button
        onPress={() => {
          save(held.draft);
        }}
      >
        {t({ defaultMessage: "Save the landing screen", id: "cJoJtXpi" })}
      </Button>
      {held.own && (
        <Button
          onPress={() => {
            save(undefined);
          }}
        >
          {t({ defaultMessage: "Remove this organization's landing screen", id: "Lcrvq8Xe" })}
        </Button>
      )}
      {said !== undefined && <p role="status">{said}</p>}
    </section>
  );
}
