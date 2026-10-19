// SPDX-License-Identifier: AGPL-3.0-or-later

// An organization's suspension (§2.1 of docs/08-identity.md): shown to everyone who reads the
// organization, with why, since when, what became of its workloads, and what its tenant still
// reads; and pulled or lifted from above by someone whose own grants hold that over an
// organization above it. Whether they do is read from their grants as a courtesy (§5 of
// docs/11-surface-web.md); the control plane decides, and says why when it refuses.

import { timestampDate } from "@bufbuild/protobuf/wkt";
import { useCallback, useState } from "react";
import {
  Button,
  CheckboxButton,
  CheckboxField,
  Form,
  Input,
  Label,
  TextField,
} from "react-aria-components";
import { useReach } from "../clients";
import { OnSuspend, type Organization, OrganizationStatus } from "../gen/korpis/v1/tenancy_pb";
import { intl, t } from "../i18n/intl";
import { everyGrant, holds, upward } from "./held";
import { useLoad } from "./load";
import { field } from "./PasswordForm";
import { refusal } from "./refusal";

/** Where `organization` stands, and the lever above it for whoever holds it. */
export function Suspension({ organization }: { readonly organization: Organization }) {
  const [shown, setShown] = useState(organization);
  return (
    <>
      {shown.status === OrganizationStatus.SUSPENDED && <Notice organization={shown} />}
      <Lever organization={shown} changed={setShown} />
    </>
  );
}

/** What a suspended organization's readers are told. */
function Notice({ organization }: { readonly organization: Organization }) {
  const s = organization.suspension;
  const since =
    s?.at === undefined ? "" : intl.formatDate(timestampDate(s.at), { dateStyle: "long" });
  return (
    <section role="status" aria-labelledby="suspended">
      <h2 id="suspended">{t({ defaultMessage: "Suspended", id: "oJsHQWAy" })}</h2>
      <p>
        {t(
          { defaultMessage: "Since {since}: {reason}", id: "NCZyas1o" },
          { since, reason: s?.reason ?? "" },
        )}
      </p>
      <p>
        {s?.onSuspend === OnSuspend.STOP
          ? t({
              defaultMessage: "Its workloads are held stopped until it is resumed.",
              id: "GfsxhpW3",
            })
          : t({ defaultMessage: "Its workloads keep running.", id: "NNOUFZd9" })}
      </p>
      <p>
        {t({
          defaultMessage:
            "Its grants, organizations, projects, plans, nodes, and the status of its workloads stay readable. Every change, a console, and a file are refused until it is resumed.",
          id: "BfEXcPXB",
        })}
      </p>
    </section>
  );
}

/** Suspending or resuming `organization`, offered to a signed-in caller holding it above. */
function Lever(props: {
  readonly organization: Organization;
  readonly changed: (o: Organization) => void;
}) {
  const { clients, allowed } = useReach();
  const { organization, changed } = props;
  const suspended = organization.status === OrganizationStatus.SUSPENDED;
  const action = suspended ? "org.resume" : "org.suspend";
  const parent = organization.parent;
  const may = useLoad(
    useCallback(async () => {
      // A share link holds no lever over an organization, and a root has nothing above it.
      if (allowed !== undefined || parent === undefined) return false;
      const [grants, above] = await Promise.all([everyGrant(clients), upward(clients, parent)]);
      return holds(grants, action, above);
    }, [clients, allowed, parent, action]),
  );
  const [busy, setBusy] = useState(false);
  const [stop, setStop] = useState(false);
  const [said, setSaid] = useState<string>();
  if (may.state !== "done" || !may.value) return null;
  const call = async (ask: () => Promise<{ organization?: Organization | undefined }>) => {
    setBusy(true);
    setSaid(undefined);
    try {
      const r = await ask();
      if (r.organization !== undefined) changed(r.organization);
    } catch (e) {
      setSaid(refusal(e));
    } finally {
      setBusy(false);
    }
  };
  const mutation = () => ({ idempotencyKey: crypto.randomUUID() });
  const id = organization.id;
  return (
    <section aria-label={t({ defaultMessage: "Suspension", id: "/gco5BAB" })}>
      {suspended ? (
        <Button
          isDisabled={busy}
          onPress={() => {
            void call(() =>
              clients.organizations.resumeOrganization({ mutation: mutation(), organization: id }),
            );
          }}
        >
          {t({ defaultMessage: "Resume", id: "3y9DGgur" })}
        </Button>
      ) : (
        <Form
          onSubmit={(event) => {
            event.preventDefault();
            const reason = field(new FormData(event.currentTarget), "reason");
            const onSuspend = stop ? OnSuspend.STOP : OnSuspend.KEEP_RUNNING;
            void call(() =>
              clients.organizations.suspendOrganization({
                mutation: mutation(),
                organization: id,
                reason,
                onSuspend,
              }),
            );
          }}
        >
          <TextField name="reason" isRequired maxLength={500}>
            <Label>
              {t({ defaultMessage: "Why, as its tenant will be shown it", id: "edpuFUqD" })}
            </Label>
            <Input />
          </TextField>
          <CheckboxField isSelected={stop} onChange={setStop}>
            <CheckboxButton>
              {t({ defaultMessage: "Hold its workloads stopped as well", id: "VOPMMp1+" })}
            </CheckboxButton>
          </CheckboxField>
          <Button type="submit" isDisabled={busy}>
            {t({ defaultMessage: "Suspend", id: "elnaZeq9" })}
          </Button>
        </Form>
      )}
      {said !== undefined && <p role="alert">{said}</p>}
    </section>
  );
}
