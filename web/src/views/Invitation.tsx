// SPDX-License-Identifier: AGPL-3.0-or-later

// The invitation page: the invitation arrives in the fragment of `/invitation#korpisiv_…`, and is
// taken from the address bar at once (fragment.ts), or is pasted here when it was handed over as
// text, as `korpis-server install` prints the root account's.

import { useState } from "react";
import { Button, Form, Input, Label, TextField } from "react-aria-components";
import { Redeem, type RedeemProps } from "./Redeem";

/** The prefix every invitation starts with (credential.rs). */
export const INVITATION_PREFIX = "korpisiv_";

/** The invitation page; `held` is the invitation the fragment brought, if any. */
export function Invitation(
  props: Omit<RedeemProps, "invitation"> & { readonly held: string | undefined },
) {
  const { held, ...redeem } = props;
  const [invitation, setInvitation] = useState(held);
  const [typed, setTyped] = useState("");
  if (invitation !== undefined) return <Redeem {...redeem} invitation={invitation} />;
  return (
    <main>
      <h1>Redeem an invitation</h1>
      <Form
        onSubmit={(event) => {
          event.preventDefault();
          setInvitation(typed.trim());
        }}
      >
        <TextField
          name="invitation"
          isRequired
          value={typed}
          onChange={setTyped}
          autoComplete="off"
          validate={(v) =>
            v.trim().startsWith(INVITATION_PREFIX)
              ? null
              : `An invitation starts ${INVITATION_PREFIX}`
          }
        >
          <Label>The invitation</Label>
          <Input />
        </TextField>
        <Button type="submit">Continue</Button>
      </Form>
    </main>
  );
}
