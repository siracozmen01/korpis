// SPDX-License-Identifier: AGPL-3.0-or-later

// A password with its second factor beside it, a code from an authenticator app or a recovery
// code (§9.3 of docs/08-identity.md): signing in asks for the sign-in name too, re-authenticating
// does not, since the session already names the account.

import { useState } from "react";
import {
  Button,
  Form,
  Input,
  Label,
  RadioButton,
  RadioField,
  RadioGroup,
  TextField,
} from "react-aria-components";
import { t } from "../i18n/intl";

/** The second factor beside a password, as the API's `oneof second` carries it. */
export type Second =
  | { readonly case: "totpCode"; readonly value: string }
  | { readonly case: "recoveryCode"; readonly value: string };

/** What the form was submitted with. */
export interface Credentials {
  readonly name: string;
  readonly password: string;
  readonly second: Second;
}

/** The text `form` holds under `name`, or nothing. */
export function field(form: FormData, name: string): string {
  const value = form.get(name);
  return typeof value === "string" ? value : "";
}

/** The form; `withName` asks for the sign-in name as well. */
export function PasswordForm(props: {
  readonly withName: boolean;
  readonly busy: boolean;
  readonly action: string;
  readonly submit: (c: Credentials) => void;
}) {
  const [kind, setKind] = useState<Second["case"]>("totpCode");
  return (
    <Form
      onSubmit={(event) => {
        event.preventDefault();
        const form = new FormData(event.currentTarget);
        const value = field(form, "code");
        props.submit({
          name: field(form, "name"),
          password: field(form, "password"),
          second: { case: kind, value },
        });
      }}
    >
      {props.withName && (
        <TextField name="name" isRequired autoComplete="username">
          <Label>{t({ defaultMessage: "Sign-in name", id: "zRVBYlyI" })}</Label>
          <Input />
        </TextField>
      )}
      <TextField name="password" type="password" isRequired autoComplete="current-password">
        <Label>{t({ defaultMessage: "Password", id: "5sg7KCrr" })}</Label>
        <Input />
      </TextField>
      <RadioGroup
        value={kind}
        onChange={(v) => {
          setKind(v === "recoveryCode" ? "recoveryCode" : "totpCode");
        }}
      >
        <Label>
          {t({
            defaultMessage: "Beside it",
            id: "6/tAYJsQ",
            description: "the second factor given beside a password",
          })}
        </Label>
        <RadioField value="totpCode">
          <RadioButton>
            {t({ defaultMessage: "A code from your authenticator app", id: "yuk2Sbg3" })}
          </RadioButton>
        </RadioField>
        <RadioField value="recoveryCode">
          <RadioButton>{t({ defaultMessage: "A recovery code", id: "+PRD45NT" })}</RadioButton>
        </RadioField>
      </RadioGroup>
      <TextField name="code" isRequired autoComplete="one-time-code">
        <Label>
          {kind === "totpCode"
            ? t({
                defaultMessage: "Code",
                id: "N+x6wMk/",
                description: "an authenticator app's code",
              })
            : t({ defaultMessage: "Recovery code", id: "e4C1p0sc" })}
        </Label>
        <Input />
      </TextField>
      <Button type="submit" isDisabled={props.busy}>
        {props.action}
      </Button>
    </Form>
  );
}
