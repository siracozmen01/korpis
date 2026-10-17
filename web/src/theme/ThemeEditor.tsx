// SPDX-License-Identifier: AGPL-3.0-or-later

// An organization's theme, edited (§7 of docs/11-surface-web.md): its name, logo, colours,
// sign-in text, and stylesheet, saved against the version it was read at, so two editors never
// silently overwrite each other; previewed on the page before it is saved; and removed, so the
// nearest ancestor's applies again. Who may change it is the control plane's to say: the form is
// offered to whoever opens the organization, and a refusal says why.

import { create } from "@bufbuild/protobuf";
import { useEffect, useState } from "react";
import { Button, Form, Input, Label, TextArea, TextField } from "react-aria-components";
import { useReach } from "../clients";
import {
  type Theme,
  ThemeColorsSchema,
  ThemeSchema,
  type ThemeColors,
} from "../gen/korpis/v1/tenancy_pb";
import { refusal } from "../views/refusal";
import { wear } from "./theme";

type Token = keyof Omit<ThemeColors, "$typeName" | "$unknown">;

const TOKENS: readonly [Token, string][] = [
  ["background", "Background"],
  ["text", "Text"],
  ["muted", "Muted text"],
  ["line", "Lines"],
  ["accent", "Accent"],
  ["accentText", "Text on the accent"],
  ["danger", "Danger"],
  ["surface", "Surfaces"],
  ["focus", "Focus ring"],
];

/** What the editor holds: the draft, the version it was read at, and whose theme it shows. */
interface Held {
  readonly draft: Theme;
  readonly version: bigint;
  readonly own: boolean;
}

/** The theme editor of organization `id`. */
export function ThemeEditor({ id }: { readonly id: string }) {
  const { clients } = useReach();
  const [held, setHeld] = useState<Held>();
  const [said, setSaid] = useState<string>();
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let gone = false;
    clients.organizations.getTheme({ of: { kind: { case: "organization", value: id } } }).then(
      (r) => {
        if (gone) return;
        const g = r.governing;
        const own = g?.setOn === id;
        const draft = own && g.theme !== undefined ? g.theme : create(ThemeSchema, {});
        setHeld({ draft, version: g?.ownVersion ?? 0n, own });
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
  const edit = (change: Partial<Theme>) => {
    setHeld({ ...held, draft: create(ThemeSchema, { ...held.draft, ...change }) });
  };
  const colour = (scheme: "light" | "dark", token: Token, value: string) => {
    const now = held.draft[scheme] ?? create(ThemeColorsSchema, {});
    edit({ [scheme]: create(ThemeColorsSchema, { ...now, [token]: value }) });
  };
  const save = (theme: Theme | undefined) => {
    const expectedVersion = held.version > 0n ? held.version : undefined;
    clients.organizations
      .setTheme({
        mutation: { idempotencyKey: crypto.randomUUID(), expectedVersion },
        organization: id,
        theme,
      })
      .then(
        () => {
          setSaid(theme === undefined ? "Removed; the nearest theme above applies." : "Saved.");
          setAttempt((n) => n + 1);
        },
        (e: unknown) => {
          setSaid(refusal(e));
        },
      );
  };
  return (
    <section aria-labelledby="theme">
      <h2 id="theme">Theme</h2>
      <p>
        {held.own
          ? "This organization's own theme, worn here and beneath unless an organization sets its own."
          : "This organization wears the theme of the nearest organization above that sets one."}
      </p>
      <Form
        onSubmit={(e) => {
          e.preventDefault();
          save(held.draft);
        }}
      >
        <TextField
          value={held.draft.productName}
          onChange={(v) => {
            edit({ productName: v });
          }}
        >
          <Label>Product name</Label>
          <Input />
        </TextField>
        <TextField
          value={held.draft.signInText}
          onChange={(v) => {
            edit({ signInText: v });
          }}
        >
          <Label>Sign-in text</Label>
          <TextArea />
        </TextField>
        <Logo held={held.draft} edit={edit} />
        {(["light", "dark"] as const).map((scheme) => (
          <fieldset key={scheme}>
            <legend>{scheme === "light" ? "Light colours" : "Dark colours"}</legend>
            {TOKENS.map(([token, label]) => (
              <TextField
                key={token}
                value={held.draft[scheme]?.[token] ?? ""}
                onChange={(v) => {
                  colour(scheme, token, v);
                }}
              >
                <Label>{`${label} (#rrggbb)`}</Label>
                <Input />
              </TextField>
            ))}
          </fieldset>
        ))}
        <TextField
          value={held.draft.stylesheet}
          onChange={(v) => {
            edit({ stylesheet: v });
          }}
        >
          <Label>Stylesheet</Label>
          <TextArea />
        </TextField>
        <Button
          onPress={() => {
            wear(document, held.draft, { stylesheet: true });
          }}
        >
          Preview
        </Button>
        <Button type="submit">Save</Button>
        {held.own && (
          <Button
            onPress={() => {
              save(undefined);
            }}
          >
            Remove this organization's theme
          </Button>
        )}
      </Form>
      {said !== undefined && <p role="status">{said}</p>}
    </section>
  );
}

/** The logo: a PNG, WebP, or SVG file read into the theme, or removed from it. */
function Logo(props: { readonly held: Theme; readonly edit: (c: Partial<Theme>) => void }) {
  const has = props.held.logo !== undefined && props.held.logo.data.length > 0;
  return (
    <div>
      <label>
        Logo (PNG, WebP, or SVG, at most 256 KiB)
        <input
          type="file"
          accept="image/png,image/webp,image/svg+xml"
          onChange={(e) => {
            const file = e.target.files?.[0];
            if (file === undefined) return;
            void file.arrayBuffer().then((b) => {
              props.edit({
                logo: {
                  $typeName: "korpis.v1.Logo",
                  mediaType: file.type,
                  data: new Uint8Array(b),
                },
              });
            });
          }}
        />
      </label>
      {has && (
        <Button
          onPress={() => {
            props.edit({ logo: undefined });
          }}
        >
          Remove the logo
        </Button>
      )}
    </div>
  );
}
