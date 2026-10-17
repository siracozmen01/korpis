// SPDX-License-Identifier: AGPL-3.0-or-later

// The signed-in user's own screen: who they are, how they sign in, and where they are signed in.

import { useCallback, useState } from "react";
import { useLoad } from "../views/load";
import { Shown } from "../views/Shown";
import { Factors } from "./Factors";
import type { ProfileServices } from "./services";
import { Sessions } from "./Sessions";
import { t } from "../i18n/intl";

/** The profile screen, its factors read again after each change. */
export function Profile({ services }: { readonly services: ProfileServices }) {
  const [round, setRound] = useState(0);
  return (
    <main>
      <Read
        key={round}
        services={services}
        changed={() => {
          setRound((r) => r + 1);
        }}
      />
      <Sessions services={services} />
    </main>
  );
}

function Read(props: { readonly services: ProfileServices; readonly changed: () => void }) {
  const { services, changed } = props;
  const { profiles } = services;
  const profile = useLoad(useCallback(() => profiles.getProfile({}), [profiles]));
  return (
    <Shown
      loaded={profile}
      reading={t({ defaultMessage: "Reading your profile…", id: "3AVtR6Oy" })}
      done={(p) => (
        <>
          <h1>{p.displayName}</h1>
          <p>{t({ defaultMessage: "Signed in as {name}.", id: "TDUCXqaO" }, { name: p.name })}</p>
          <Factors profile={p} services={services} changed={changed} />
        </>
      )}
    />
  );
}
