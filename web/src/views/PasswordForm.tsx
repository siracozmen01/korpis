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
          <Label>Sign-in name</Label>
          <Input />
        </TextField>
      )}
      <TextField name="password" type="password" isRequired autoComplete="current-password">
        <Label>Password</Label>
        <Input />
      </TextField>
      <RadioGroup
        value={kind}
        onChange={(v) => {
          setKind(v === "recoveryCode" ? "recoveryCode" : "totpCode");
        }}
      >
        <Label>Beside it</Label>
        <RadioField value="totpCode">
          <RadioButton>A code from your authenticator app</RadioButton>
        </RadioField>
        <RadioField value="recoveryCode">
          <RadioButton>A recovery code</RadioButton>
        </RadioField>
      </RadioGroup>
      <TextField name="code" isRequired autoComplete="one-time-code">
        <Label>{kind === "totpCode" ? "Code" : "Recovery code"}</Label>
        <Input />
      </TextField>
      <Button type="submit" isDisabled={props.busy}>
        {props.action}
      </Button>
    </Form>
  );
}
