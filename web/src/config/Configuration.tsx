// SPDX-License-Identifier: AGPL-3.0-or-later

// A workload's configuration (§5 of docs/09-recipes.md): the form Korpis generates from its
// recipe's schema, every field not hidden with its label in the recipe's words for the viewer's
// language (§5.3 there), its value, and saving it as a new intent whose plan interrupts as much
// as its risk calls for (planned.tsx). Which fields the viewer may change is the control plane's
// to say, field by field. A secret the viewer may not read is never shown and never needed: left
// empty it is kept as it is, which the API's `keep` says without knowing it.

import { clone, create } from "@bufbuild/protobuf";
import { ConnectError } from "@connectrpc/connect";
import { useCallback, useState } from "react";
import { Button, Form } from "react-aria-components";
import { useReach } from "../clients";
import type { GetWorkloadRecipeResponse } from "../gen/korpis/v1/api_pb";
import { ErrorSchema, type Value } from "../gen/korpis/v1/common_pb";
import { FieldPermission, type ConfigField } from "../gen/korpis/v1/recipe_pb";
import { type Intent, IntentBodySchema } from "../gen/korpis/v1/workload_pb";
import { t } from "../i18n/intl";
import { useLoad } from "../views/load";
import { Shown } from "../views/Shown";
import { usePlanned } from "../workload/planned";
import { Field, ReadOnly } from "./Field";
import { plain, read, shown } from "./values";

/** Whether the form offers `f` for changing: shown, and set by no one but a person. */
function editable(f: ConfigField): boolean {
  return f.derived === "" && f.const === undefined && f.permission !== FieldPermission.SYSTEM;
}

/** The configuration of workload `id`, whose newest intent is `head`. */
export function Configuration({ id, head }: { readonly id: string; readonly head: Intent }) {
  const { clients } = useReach();
  const load = useCallback(
    () => clients.workloads.getWorkloadRecipe({ workload: id, locales: [...navigator.languages] }),
    [clients, id],
  );
  const loaded = useLoad(load);
  return (
    <Shown
      loaded={loaded}
      reading={t({ defaultMessage: "Reading its configuration…", id: "lWYMAj0Y" })}
      done={(r) => <Form_ key={head.version.toString()} id={id} head={head} recipe={r} />}
    />
  );
}

/** The field a refusal names, when it names one of the configuration's. */
function refusedField(e: unknown): string | undefined {
  const field = ConnectError.from(e).findDetails(ErrorSchema)[0]?.field ?? "";
  return field.startsWith("body.config.") ? field.slice("body.config.".length) : undefined;
}

function Form_(props: {
  readonly id: string;
  readonly head: Intent;
  readonly recipe: GetWorkloadRecipeResponse;
}) {
  const { id, head, recipe } = props;
  const { clients } = useReach();
  const flow = usePlanned();
  const config = head.body?.config ?? {};
  const fields = (recipe.config?.fields ?? []).filter((f) => !f.hidden);
  const [texts, setTexts] = useState<Record<string, string>>(() =>
    Object.fromEntries(fields.map((f) => [f.key, shown(config[f.key])])),
  );
  const [refused, setRefused] = useState<string>();
  const label = (f: ConfigField) =>
    plain(recipe.messages[`config.fields.${f.key}.label`] ?? f.label) || f.key;
  const invalid = fields.filter((f) => editable(f) && read(f, texts[f.key] ?? "") === "invalid");
  const save = () => {
    // What the form does not show is declared as it stands, but for what the control plane
    // computes itself; a secret withheld from the viewer and left empty is kept as it is.
    const next = new Map<string, Value>(Object.entries(config));
    for (const f of recipe.config?.fields ?? []) {
      if (f.derived !== "" || f.const !== undefined) {
        next.delete(f.key);
      } else if (!f.hidden && editable(f)) {
        const v = read(f, texts[f.key] ?? "");
        if (v === undefined || v === "invalid") next.delete(f.key);
        else next.set(f.key, v);
      }
    }
    const keep = head.withheld.filter((k) => !next.has(k));
    const body = clone(IntentBodySchema, head.body ?? create(IntentBodySchema));
    body.config = Object.fromEntries(next);
    setRefused(undefined);
    flow.begin(t({ defaultMessage: "Configuration change", id: "LW9qzGlz" }), async () => {
      try {
        const r = await clients.workloads.declareIntent({
          mutation: { idempotencyKey: crypto.randomUUID(), expectedVersion: head.version },
          target: { case: "workload", value: id },
          body,
          keep,
          propose: true,
        });
        return r.plan;
      } catch (e) {
        setRefused(refusedField(e));
        throw e;
      }
    });
  };
  if (flow.review) return flow.review;
  const description = recipe.messages["metadata.description"];
  return (
    <section aria-labelledby="configuration">
      <h2 id="configuration">{t({ defaultMessage: "Configuration", id: "7OW8BTDz" })}</h2>
      {description !== undefined && <p>{plain(description)}</p>}
      <Form
        onSubmit={(e) => {
          e.preventDefault();
          if (invalid.length === 0) save();
        }}
      >
        {fields.map((f) =>
          editable(f) ? (
            <Field
              key={f.key}
              field={f}
              label={label(f)}
              text={texts[f.key] ?? ""}
              withheld={head.withheld.includes(f.key)}
              refused={refused === f.key}
              change={(text) => {
                setTexts({ ...texts, [f.key]: text });
              }}
            />
          ) : (
            <ReadOnly key={f.key} label={label(f)} value={shown(config[f.key])} />
          ),
        )}
        <Button type="submit" isDisabled={!flow.ready || invalid.length > 0}>
          {t({ defaultMessage: "Save the configuration", id: "ICZqeoPt" })}
        </Button>
      </Form>
      {flow.shown}
    </section>
  );
}
