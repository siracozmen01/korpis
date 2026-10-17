// SPDX-License-Identifier: AGPL-3.0-or-later

// One field of the configuration form, as its type asks to be written: a checkbox for a boolean,
// a choice for an enum, text for the rest, which the control plane reads as its type. A secret is
// typed where it cannot be read over a shoulder, and one withheld from the viewer starts empty and
// says that empty keeps it.

import {
  CheckboxButton,
  CheckboxField,
  FieldError,
  Input,
  Label,
  RadioButton,
  RadioField,
  RadioGroup,
  Text,
  TextField,
} from "react-aria-components";
import { ConfigType, type ConfigField } from "../gen/korpis/v1/recipe_pb";
import { t } from "../i18n/intl";
import { read, shown } from "./values";

/** Why `text` is not a value of `field`'s type, if it is not. */
function wrong(field: ConfigField, text: string): string | undefined {
  if (read(field, text) !== "invalid") return undefined;
  return field.type === ConfigType.INTEGER
    ? t({ defaultMessage: "A whole number", id: "esOevyqJ" })
    : t({ defaultMessage: "A number", id: "8jocOM4G" });
}

/** A field the viewer may try to change. */
export function Field(props: {
  readonly field: ConfigField;
  readonly label: string;
  readonly text: string;
  readonly withheld: boolean;
  readonly refused: boolean;
  readonly change: (text: string) => void;
}) {
  const { field, label, text, change } = props;
  if (field.type === ConfigType.BOOLEAN) {
    return (
      <CheckboxField
        isSelected={text === "true"}
        isInvalid={props.refused}
        onChange={(on) => {
          change(on ? "true" : "false");
        }}
      >
        <CheckboxButton>{label}</CheckboxButton>
      </CheckboxField>
    );
  }
  if (field.type === ConfigType.ENUM) {
    return (
      <RadioGroup value={text} onChange={change} isInvalid={props.refused}>
        <Label>{label}</Label>
        {field.choices.map((c) => (
          <RadioField key={shown(c)} value={shown(c)}>
            <RadioButton>{shown(c)}</RadioButton>
          </RadioField>
        ))}
      </RadioGroup>
    );
  }
  const why = wrong(field, text);
  return (
    <TextField
      value={text}
      onChange={change}
      type={field.secret ? "password" : "text"}
      autoComplete="off"
      isInvalid={props.refused || why !== undefined}
    >
      <Label>{label}</Label>
      <Input {...(field.default === undefined ? {} : { placeholder: shown(field.default) })} />
      {props.withheld && (
        <Text slot="description">
          {t({
            defaultMessage: "Set, and not shown to you. Left empty, it is kept as it is.",
            id: "9r6qVdFD",
          })}
        </Text>
      )}
      <FieldError>{why}</FieldError>
    </TextField>
  );
}

/** A field set by Korpis or fixed by its recipe, shown as it is. */
export function ReadOnly(props: { readonly label: string; readonly value: string }) {
  return (
    <p>
      {t(
        {
          defaultMessage: "{label}: {value}",
          id: "YEehhHKX",
          description: "a configuration field set by Korpis",
        },
        {
          label: props.label,
          value:
            props.value === "" ? t({ defaultMessage: "not set yet", id: "ZwP0zyqP" }) : props.value,
        },
      )}
    </p>
  );
}
