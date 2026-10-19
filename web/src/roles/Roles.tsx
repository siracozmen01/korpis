// SPDX-License-Identifier: AGPL-3.0-or-later

// The roles offered at a place (§4 of docs/08-identity.md, §5 of docs/11-surface-web.md): the
// grant templates its organization and every one above it own, each with what it allows, what it
// passes on, and how long it lasts. A signed-in caller is offered to give a role here; the control
// plane decides, by attenuating the caller's own grant, and says why when it refuses. On an
// organization, whoever holds `template.write` there makes, edits, and retires its own roles.
// Nothing here is offered through a share link, which can give nothing.

import { useCallback, useState } from "react";
import { Button } from "react-aria-components";
import { useReach } from "../clients";
import type { GrantTemplate } from "../gen/korpis/v1/authority_pb";
import { ObjectKind } from "../gen/korpis/v1/common_pb";
import { t } from "../i18n/intl";
import { everyGrant, holds, upward } from "../views/held";
import { useLoad } from "../views/load";
import { PAGE_SIZE } from "../views/Pages";
import { Shown } from "../views/Shown";
import { GiveRole, type Place } from "./GiveRole";
import { RoleEditor } from "./RoleEditor";
import { appliesAt, lifetimeDays } from "./roles";

const KIND: Record<Place["case"], ObjectKind> = {
  organization: ObjectKind.ORGANIZATION,
  project: ObjectKind.PROJECT,
  workload: ObjectKind.WORKLOAD,
};

/** What a role allows and passes on, and how long it lasts, as a person reads it. */
function described(r: GrantTemplate): string {
  const allows = t(
    { defaultMessage: "Allows {actions}.", id: "JQj3BcAV" },
    { actions: r.actions.join(", ") },
  );
  const d = r.delegation;
  const passes =
    d?.allowed === true
      ? t(
          { defaultMessage: "Passes on {actions}, {depth} levels deep.", id: "rUC1kIHy" },
          { actions: d.actions.join(", "), depth: d.maxDepth ?? 1 },
        )
      : t({ defaultMessage: "Passes nothing on.", id: "sWEBtqIv" });
  const days = lifetimeDays(r);
  const lasts =
    days === undefined
      ? ""
      : t(
          { defaultMessage: "Lasts {days, plural, one {# day} other {# days}}.", id: "BhZi+dzN" },
          { days: Number(days) },
        );
  return [allows, passes, lasts].filter((s) => s !== "").join(" ");
}

/** Every role offered at `place`, every page of them. */
async function offered(
  clients: ReturnType<typeof useReach>["clients"],
  place: Place,
): Promise<GrantTemplate[]> {
  const at = { kind: KIND[place.case], id: place.id };
  const found: GrantTemplate[] = [];
  let pageToken = "";
  do {
    const r = await clients.grants.listGrantTemplates({
      of: { case: "at", value: at },
      page: { pageSize: PAGE_SIZE, pageToken },
    });
    found.push(...r.templates);
    pageToken = r.page?.nextPageToken ?? "";
  } while (pageToken !== "");
  return found;
}

function Role(props: {
  readonly role: GrantTemplate;
  readonly place: Place;
  readonly editable: boolean;
  readonly changed: () => void;
}) {
  const { allowed } = useReach();
  const { role, place, editable, changed } = props;
  const [open, setOpen] = useState<"give" | "edit">();
  const givable = allowed === undefined && appliesAt(role, KIND[place.case]);
  return (
    <li>
      <strong>{role.name}</strong>
      <small className="aside">
        {role.organization === place.id
          ? t({ defaultMessage: "version {v}", id: "myUPH/nd" }, { v: String(role.version) })
          : t(
              { defaultMessage: "version {v}, from above", id: "cv76pwSz" },
              { v: String(role.version) },
            )}
      </small>
      <p>{described(role)}</p>
      {givable && open === undefined && (
        <Button
          onPress={() => {
            setOpen("give");
          }}
        >
          {t({ defaultMessage: "Give this role", id: "c7tiahWJ" })}
        </Button>
      )}
      {editable && open === undefined && (
        <Button
          onPress={() => {
            setOpen("edit");
          }}
        >
          {t({ defaultMessage: "Edit", id: "wEQDC6Wv" })}
        </Button>
      )}
      {open === "give" && <GiveRole template={role} place={place} />}
      {open === "edit" && (
        <RoleEditor
          organization={place.id}
          role={role}
          done={() => {
            setOpen(undefined);
            changed();
          }}
        />
      )}
    </li>
  );
}

/** The roles offered at `place`, read again whenever `changed` is called, by remounting. */
export function Roles({ place }: { readonly place: Place }) {
  const [round, setRound] = useState(0);
  return (
    <RolesRead
      key={round}
      place={place}
      changed={() => {
        setRound((n) => n + 1);
      }}
    />
  );
}

function RolesRead(props: { readonly place: Place; readonly changed: () => void }) {
  const { clients, allowed } = useReach();
  const { place, changed } = props;
  const { case: kind, id } = place;
  const roles = useLoad(
    useCallback(async () => offered(clients, { case: kind, id }), [clients, kind, id]),
  );
  // On an organization, whoever holds template.write there may make and edit its own roles.
  const writes = useLoad(
    useCallback(async () => {
      if (allowed !== undefined || kind !== "organization") return false;
      const [grants, above] = await Promise.all([everyGrant(clients), upward(clients, id)]);
      return holds(grants, "template.write", above);
    }, [clients, allowed, kind, id]),
  );
  const editor = writes.state === "done" && writes.value;
  const [making, setMaking] = useState(false);
  return (
    <section aria-labelledby="roles">
      <h2 id="roles">{t({ defaultMessage: "Roles", id: "c35gM5T7" })}</h2>
      <Shown
        loaded={roles}
        reading={t({ defaultMessage: "Reading the roles offered here…", id: "RAkt0S4r" })}
        done={(list) =>
          list.length === 0 ? (
            <p>{t({ defaultMessage: "No role is offered here.", id: "LEqZXL7u" })}</p>
          ) : (
            <ul>
              {list.map((r) => (
                <Role
                  key={r.id}
                  role={r}
                  place={place}
                  editable={editor && r.organization === place.id}
                  changed={changed}
                />
              ))}
            </ul>
          )
        }
      />
      {editor && !making && (
        <Button
          onPress={() => {
            setMaking(true);
          }}
        >
          {t({ defaultMessage: "A new role", id: "ramQHEyT" })}
        </Button>
      )}
      {making && (
        <RoleEditor
          organization={place.id}
          done={() => {
            setMaking(false);
            changed();
          }}
        />
      )}
    </section>
  );
}
