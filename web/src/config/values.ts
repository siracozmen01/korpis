// SPDX-License-Identifier: AGPL-3.0-or-later

// A recipe's words and values as the configuration form shows them (§5 and §5.3 of
// docs/09-recipes.md). A recipe's message holds no placeholder, so showing it is reading its
// quoting, as the control plane checked it; a value is shown as text and read back as the type
// its field declares, the control plane deciding whether it is valid.

import { create } from "@bufbuild/protobuf";
import { ConfigType, type ConfigField } from "../gen/korpis/v1/recipe_pb";
import { type Value, ValueSchema } from "../gen/korpis/v1/common_pb";

/** What `text` says once its quoting is read: `''` is an apostrophe, and an apostrophe before a
 * brace quotes up to the next lone one. */
export function plain(text: string): string {
  let out = "";
  let quoted = false;
  for (let i = 0; i < text.length; i += 1) {
    const c = text.charAt(i);
    const next = text.charAt(i + 1);
    if (c === "'" && next === "'") {
      out += "'";
      i += 1;
    } else if (c === "'" && quoted) {
      quoted = false;
    } else if (c === "'" && (next === "{" || next === "}")) {
      quoted = true;
    } else {
      out += c;
    }
  }
  return out;
}

/** A value as text. */
export function shown(v: Value | undefined): string {
  const k = v?.kind;
  if (k === undefined) return "";
  switch (k.case) {
    case "stringValue":
      return k.value;
    case "intValue":
      return k.value.toString();
    case "doubleValue":
      return k.value.toString();
    case "boolValue":
      return k.value ? "true" : "false";
    case undefined:
      return "";
    default:
      return JSON.stringify(k.value, (_, x: unknown) => (typeof x === "bigint" ? x.toString() : x));
  }
}

/** `text` as a value of `field`'s type; none when it is empty, `"invalid"` when it is not one. */
export function read(field: ConfigField, text: string): Value | undefined | "invalid" {
  const trimmed = text.trim();
  if (trimmed === "") return undefined;
  switch (field.type) {
    case ConfigType.INTEGER:
      return /^-?\d+$/.test(trimmed)
        ? create(ValueSchema, { kind: { case: "intValue", value: BigInt(trimmed) } })
        : "invalid";
    case ConfigType.NUMBER: {
      const n = Number(trimmed);
      return Number.isFinite(n)
        ? create(ValueSchema, { kind: { case: "doubleValue", value: n } })
        : "invalid";
    }
    default:
      return create(ValueSchema, { kind: { case: "stringValue", value: text } });
  }
}
