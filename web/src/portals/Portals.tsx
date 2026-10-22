// SPDX-License-Identifier: AGPL-3.0-or-later

// The portals of an organization (§3.6 of docs/07-networking.md, §9.8 of docs/08-identity.md):
// names it holds under which the control plane answers for it, each shown with the certificate it
// is served with and how that certificate stands. A caller holding `portal.write` there makes
// one, served with its own `acme` certificate or a provided one it names, serves one with another
// certificate, and deletes one once told how many passkeys deleting it removes and how many
// accounts would then sign in nowhere. Nothing here is offered through a share link.

import { useCallback, useState } from "react";
import { Button, Form, Input, Label, TextField } from "react-aria-components";
import { useReach } from "../clients";
import { CertificateSource, CertificateState, type Portal } from "../gen/korpis/v1/attached_pb";
import { t } from "../i18n/intl";
import { everyGrant, holds, upward } from "../views/held";
import { useLoad } from "../views/load";
import { PAGE_SIZE } from "../views/Pages";
import { refusal } from "../views/refusal";
import { Shown } from "../views/Shown";

/** How a portal's certificate stands, as a person reads it. */
export function served(p: Portal): string {
  const own = p.certificateSource !== CertificateSource.PROVIDED;
  switch (p.certificateState) {
    case CertificateState.ISSUED:
      return own
        ? t({ defaultMessage: "Served with its own certificate.", id: "ZGrG3DQ2" })
        : t({ defaultMessage: "Served with a provided certificate.", id: "QyrO/nr6" });
    case CertificateState.FAILED:
      return t(
        { defaultMessage: "Not served: its certificate was not issued. {why}", id: "9OvYhnkX" },
        { why: p.certificateReason },
      );
    default:
      return t({
        defaultMessage: "Not served yet: its certificate is being issued.",
        id: "FSapitOw",
      });
  }
}

/** A text field. */
function Text(props: {
  readonly label: string;
  readonly value: string;
  readonly set: (v: string) => void;
}) {
  return (
    <TextField value={props.value} onChange={props.set}>
      <Label>{props.label}</Label>
      <Input />
    </TextField>
  );
}

/** What deleting `portal` removes, said before it is confirmed, and the confirmation. */
function Deleting(props: { readonly portal: Portal; readonly done: (gone: boolean) => void }) {
  const { clients } = useReach();
  const { portal, done } = props;
  const reach = useLoad(
    useCallback(async () => clients.portals.getPortal({ id: portal.id }), [clients, portal.id]),
  );
  const [said, setSaid] = useState<string>();
  const remove = async () => {
    setSaid(undefined);
    try {
      await clients.portals.deletePortal({
        mutation: { idempotencyKey: crypto.randomUUID() },
        id: portal.id,
      });
      done(true);
    } catch (e) {
      setSaid(refusal(e));
    }
  };
  return (
    <Shown
      loaded={reach}
      reading={t({ defaultMessage: "Reading what deleting it removes…", id: "se3fX2nT" })}
      done={(r) => (
        <div role="group" aria-label={t({ defaultMessage: "Delete the portal", id: "8d/q0B9s" })}>
          <p>
            {t(
              {
                defaultMessage:
                  "Deleting it ends every session begun at it and removes {passkeys, plural, one {the # passkey} other {the # passkeys}} registered there. {accounts, plural, =0 {Every account with one signs in elsewhere too.} one {# account signs in nowhere else, and is recovered by being invited again.} other {# accounts sign in nowhere else, and are recovered by being invited again.}}",
                id: "AdYv7VFi",
              },
              { passkeys: Number(r.passkeys), accounts: Number(r.accountsOnlyHere) },
            )}
          </p>
          <Button onPress={() => void remove()}>
            {t({ defaultMessage: "Delete the portal", id: "8d/q0B9s" })}
          </Button>
          <Button
            onPress={() => {
              done(false);
            }}
          >
            {t({ defaultMessage: "Cancel", id: "47FYwba+" })}
          </Button>
          {said !== undefined && <p role="alert">{said}</p>}
        </div>
      )}
    />
  );
}

/** A form serving `portal` with another certificate: a provided one it names, or its own. */
function Recertify(props: { readonly portal: Portal; readonly done: (changed: boolean) => void }) {
  const { clients } = useReach();
  const { portal, done } = props;
  const [certificate, setCertificate] = useState("");
  const [said, setSaid] = useState<string>();
  const save = async () => {
    setSaid(undefined);
    try {
      await clients.portals.setPortalCertificate({
        mutation: { idempotencyKey: crypto.randomUUID() },
        id: portal.id,
        certificate: certificate.trim(),
      });
      done(true);
    } catch (e) {
      setSaid(refusal(e));
    }
  };
  return (
    <Form
      aria-label={t({ defaultMessage: "Serve it with another certificate", id: "hJSRZ6Nh" })}
      onSubmit={(event) => {
        event.preventDefault();
        void save();
      }}
    >
      <Text
        label={t({
          defaultMessage: "A provided certificate, crt_…, or nothing for its own",
          id: "HOzQ6Y9l",
        })}
        value={certificate}
        set={setCertificate}
      />
      <Button type="submit">{t({ defaultMessage: "Serve it with this", id: "iE8ogyPn" })}</Button>
      <Button
        onPress={() => {
          done(false);
        }}
      >
        {t({ defaultMessage: "Cancel", id: "47FYwba+" })}
      </Button>
      {said !== undefined && <p role="alert">{said}</p>}
    </Form>
  );
}

function PortalItem(props: {
  readonly portal: Portal;
  readonly writes: boolean;
  readonly changed: () => void;
}) {
  const { portal: p, writes, changed } = props;
  const [doing, setDoing] = useState<"certificate" | "delete">();
  const finished = (changes: boolean) => {
    setDoing(undefined);
    if (changes) changed();
  };
  return (
    <li>
      <strong>{p.name}</strong>
      <p>{served(p)}</p>
      {writes && doing === undefined && (
        <>
          <Button
            onPress={() => {
              setDoing("certificate");
            }}
          >
            {t({ defaultMessage: "Serve it with another certificate", id: "hJSRZ6Nh" })}
          </Button>
          <Button
            onPress={() => {
              setDoing("delete");
            }}
          >
            {t({ defaultMessage: "Delete…", id: "lNbJX12X" })}
          </Button>
        </>
      )}
      {doing === "certificate" && <Recertify portal={p} done={finished} />}
      {doing === "delete" && <Deleting portal={p} done={finished} />}
    </li>
  );
}

/** A form making a portal of `organization`. */
function Making(props: { readonly organization: string; readonly done: (made: boolean) => void }) {
  const { clients } = useReach();
  const { organization, done } = props;
  const [name, setName] = useState("");
  const [certificate, setCertificate] = useState("");
  const [said, setSaid] = useState<string>();
  const save = async () => {
    setSaid(undefined);
    try {
      await clients.portals.createPortal({
        mutation: { idempotencyKey: crypto.randomUUID() },
        organization,
        name: name.trim().toLowerCase(),
        certificate: certificate.trim(),
      });
      done(true);
    } catch (e) {
      setSaid(refusal(e));
    }
  };
  return (
    <Form
      aria-label={t({ defaultMessage: "Make a portal", id: "Sjp5VKdA" })}
      onSubmit={(event) => {
        event.preventDefault();
        void save();
      }}
    >
      <Text
        label={t({ defaultMessage: "Name, one this organization holds", id: "yRLGlA/I" })}
        value={name}
        set={setName}
      />
      <Text
        label={t({
          defaultMessage: "A provided certificate, crt_…, or nothing for its own",
          id: "HOzQ6Y9l",
        })}
        value={certificate}
        set={setCertificate}
      />
      <Button type="submit">{t({ defaultMessage: "Make the portal", id: "DeXb3C7q" })}</Button>
      <Button
        onPress={() => {
          done(false);
        }}
      >
        {t({ defaultMessage: "Cancel", id: "47FYwba+" })}
      </Button>
      {said !== undefined && <p role="alert">{said}</p>}
    </Form>
  );
}

/** Every portal of `organization` the caller may read, every page of them. */
async function portalsOf(
  clients: ReturnType<typeof useReach>["clients"],
  organization: string,
): Promise<Portal[]> {
  const found: Portal[] = [];
  let pageToken = "";
  do {
    const r = await clients.portals.listPortals({
      organization,
      page: { pageSize: PAGE_SIZE, pageToken },
    });
    found.push(...r.portals);
    pageToken = r.page?.nextPageToken ?? "";
  } while (pageToken !== "");
  return found;
}

/** The portals of `organization`, read again whenever one changes, by remounting. */
export function Portals({ organization }: { readonly organization: string }) {
  const [round, setRound] = useState(0);
  return (
    <PortalsRead
      key={round}
      organization={organization}
      changed={() => {
        setRound((n) => n + 1);
      }}
    />
  );
}

function PortalsRead(props: { readonly organization: string; readonly changed: () => void }) {
  const { clients, allowed } = useReach();
  const { organization, changed } = props;
  const portals = useLoad(
    useCallback(async () => portalsOf(clients, organization), [clients, organization]),
  );
  const writes = useLoad(
    useCallback(async () => {
      if (allowed !== undefined) return false;
      const [grants, above] = await Promise.all([
        everyGrant(clients),
        upward(clients, organization),
      ]);
      return holds(grants, "portal.write", above);
    }, [clients, allowed, organization]),
  );
  const writer = writes.state === "done" && writes.value;
  const [making, setMaking] = useState(false);
  return (
    <section aria-labelledby="portals">
      <h2 id="portals">{t({ defaultMessage: "Portals", id: "Q/y8dTor" })}</h2>
      <Shown
        loaded={portals}
        reading={t({ defaultMessage: "Reading this organization's portals…", id: "0naACmRu" })}
        done={(list) =>
          list.length === 0 ? (
            <p>
              {t({
                defaultMessage: "The control plane answers at no name of this organization's.",
                id: "ZgHI/owG",
              })}
            </p>
          ) : (
            <ul>
              {list.map((p) => (
                <PortalItem key={p.id} portal={p} writes={writer} changed={changed} />
              ))}
            </ul>
          )
        }
      />
      {writer && !making && (
        <Button
          onPress={() => {
            setMaking(true);
          }}
        >
          {t({ defaultMessage: "Make a portal", id: "Sjp5VKdA" })}
        </Button>
      )}
      {making && (
        <Making
          organization={organization}
          done={(made) => {
            setMaking(false);
            if (made) changed();
          }}
        />
      )}
    </section>
  );
}
