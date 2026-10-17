// SPDX-License-Identifier: AGPL-3.0-or-later

// The device page (§9.4 of docs/08-identity.md, RFC 8628): a command-line client asked to sign
// in as its user and showed them a code; here the signed-in user reads what is asking and from
// where, checks that the code is the one their terminal shows, and approves or denies it.
// Approving makes a session as strong as this one, so it needs a recent sign-in, and a session
// older than ten minutes re-authenticates first. Denying needs nothing more.

import type { Client } from "@connectrpc/connect";
import { timestampDate } from "@bufbuild/protobuf/wkt";
import { useEffect, useState } from "react";
import { Button, Form, Input, Label, TextField } from "react-aria-components";
import type { AccountService } from "../gen/korpis/v1/account_pb";
import type { ProfileService } from "../gen/korpis/v1/profile_pb";
import type { GetDeviceAuthorizationResponse, SessionService } from "../gen/korpis/v1/session_pb";
import type { Assertion } from "../session/webauthn";
import { Reauthenticate } from "./Reauthenticate";
import { reasonOf, refusal } from "./refusal";

/** What the device page needs. */
export interface DeviceProps {
  readonly code: string | undefined;
  readonly sessions: Client<typeof SessionService>;
  readonly accounts: Client<typeof AccountService>;
  readonly profiles: Client<typeof ProfileService>;
  readonly assert: (challenge: Uint8Array, rpId: string) => Promise<Assertion>;
}

type Step =
  | { readonly kind: "ask" }
  | { readonly kind: "read"; readonly code: string }
  | { readonly kind: "decide"; readonly asked: GetDeviceAuthorizationResponse }
  | { readonly kind: "recent"; readonly asked: GetDeviceAuthorizationResponse }
  | { readonly kind: "decided"; readonly approved: boolean };

/** A time as the user's own clock and language write it. */
function when(t: GetDeviceAuthorizationResponse["createdAt"]): string {
  return t === undefined ? "an unknown time" : timestampDate(t).toLocaleString();
}

/** The device page. */
export function Device(props: DeviceProps) {
  const [step, setStep] = useState<Step>(
    props.code === undefined ? { kind: "ask" } : { kind: "read", code: props.code },
  );
  const [error, setError] = useState<string>();
  const { sessions } = props;

  const reading = step.kind === "read" ? step.code : undefined;
  useEffect(() => {
    if (reading === undefined) return;
    let current = true;
    sessions.getDeviceAuthorization({ userCode: reading }).then(
      (asked) => {
        if (current) setStep({ kind: "decide", asked });
      },
      (e: unknown) => {
        if (!current) return;
        setError(refusal(e));
        setStep({ kind: "ask" });
      },
    );
    return () => {
      current = false;
    };
  }, [sessions, reading]);

  const decide = (asked: GetDeviceAuthorizationResponse, approved: boolean) => {
    setError(undefined);
    const userCode = asked.userCode;
    const call = approved
      ? sessions.approveDevice({ userCode })
      : sessions.denyDevice({ userCode });
    call.then(
      () => {
        setStep({ kind: "decided", approved });
      },
      (e: unknown) => {
        if (reasonOf(e) === "session.not_recent") setStep({ kind: "recent", asked });
        else setError(refusal(e));
      },
    );
  };

  const alert = error !== undefined && <p role="alert">{error}</p>;
  switch (step.kind) {
    case "ask":
    case "read":
      return (
        <Ask
          initial={props.code ?? ""}
          reading={step.kind === "read"}
          alert={alert}
          read={(code) => {
            setError(undefined);
            setStep({ kind: "read", code });
          }}
        />
      );
    case "decided":
      return (
        <main>
          <h1>{step.approved ? "Signed in" : "Refused"}</h1>
          <p>
            {step.approved
              ? "The command-line client is signed in as you. You can return to your terminal."
              : "The command-line client was refused, and its code no longer works."}
          </p>
        </main>
      );
    case "recent":
      return (
        <main>
          <h1>Sign in a command-line client</h1>
          <Reauthenticate
            accounts={props.accounts}
            profiles={props.profiles}
            assert={props.assert}
            done={() => {
              decide(step.asked, true);
            }}
          />
        </main>
      );
    case "decide":
      return <Decide asked={step.asked} alert={alert} decide={decide} />;
  }
}

/** The code, typed by hand when the page was opened without one, and while it is read. */
function Ask(props: {
  readonly initial: string;
  readonly reading: boolean;
  readonly alert: React.ReactNode;
  readonly read: (code: string) => void;
}) {
  const [code, setCode] = useState(props.initial);
  return (
    <main>
      <h1>Sign in a command-line client</h1>
      <Form
        onSubmit={(event) => {
          event.preventDefault();
          props.read(code);
        }}
      >
        <TextField name="code" isRequired value={code} onChange={setCode} autoComplete="off">
          <Label>The code your terminal shows</Label>
          <Input />
        </TextField>
        <Button type="submit" isDisabled={props.reading}>
          Continue
        </Button>
      </Form>
      {props.reading && <p role="status">Reading the request…</p>}
      {props.alert}
    </main>
  );
}

/** What is asking, and the two answers. */
function Decide(props: {
  readonly asked: GetDeviceAuthorizationResponse;
  readonly alert: React.ReactNode;
  readonly decide: (asked: GetDeviceAuthorizationResponse, approved: boolean) => void;
}) {
  const { asked } = props;
  return (
    <main>
      <h1>Sign in a command-line client</h1>
      <p>
        A command-line client asks to sign in as you. Approve it only if you started it yourself and
        your terminal shows this code:
      </p>
      <p>
        <strong>{asked.userCode}</strong>
      </p>
      <dl>
        <dt>It says it is</dt>
        <dd>{asked.userAgent === "" ? "nothing" : asked.userAgent}</dd>
        <dt>It asked from</dt>
        <dd>{asked.source}</dd>
        <dt>It asked at</dt>
        <dd>{when(asked.createdAt)}</dd>
        <dt>The code expires at</dt>
        <dd>{when(asked.expiresAt)}</dd>
      </dl>
      <Button
        onPress={() => {
          props.decide(asked, true);
        }}
      >
        Approve
      </Button>
      <Button
        onPress={() => {
          props.decide(asked, false);
        }}
      >
        Deny
      </Button>
      {props.alert}
    </main>
  );
}
