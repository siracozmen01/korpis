// SPDX-License-Identifier: AGPL-3.0-or-later

// The account's sessions (§9.4 of docs/08-identity.md): each browser and command-line client
// signed in, where and when it began and was last used, and ending one, or every one but this.
// Ending the sessions a thief holds is how a holder takes an account back.

import { timestampDate } from "@bufbuild/protobuf/wkt";
import { useCallback, useState } from "react";
import { Button } from "react-aria-components";
import type { Session } from "../gen/korpis/v1/session_pb";
import { useLoad } from "../views/load";
import { refusal } from "../views/refusal";
import { Shown } from "../views/Shown";
import type { ProfileServices } from "./services";
import { intl, t } from "../i18n/intl";

/** The sessions screen, read again after each change. */
export function Sessions({ services }: { readonly services: ProfileServices }) {
  const { sessions } = services;
  const [round, setRound] = useState(0);
  const [said, setSaid] = useState<string>();
  const again = () => {
    setRound((r) => r + 1);
  };
  const fail = (e: unknown) => {
    setSaid(refusal(e));
  };
  const end = (s: Session) => {
    sessions.endSession({ session: s.id }).then(again, fail);
  };
  return (
    <section aria-labelledby="sessions">
      <h2 id="sessions">{t({ defaultMessage: "Where you are signed in", id: "qmFAWUlu" })}</h2>
      <Listed key={round} services={services} end={end} />
      <Button
        onPress={() => {
          sessions.endOtherSessions({}).then((r) => {
            setSaid(
              t(
                {
                  defaultMessage:
                    "{n, plural, =0 {No other session was open.} one {# session ended.} other {# sessions ended.}}",
                  id: "dXVcO4GV",
                },
                { n: Number(r.ended) },
              ),
            );
            again();
          }, fail);
        }}
      >
        {t({ defaultMessage: "End every other session", id: "C3v4CI1c" })}
      </Button>
      {said !== undefined && <p role="status">{said}</p>}
    </section>
  );
}

function Listed(props: { readonly services: ProfileServices; readonly end: (s: Session) => void }) {
  const { sessions } = props.services;
  const listed = useLoad(
    useCallback(async () => (await sessions.listSessions({})).sessions, [sessions]),
  );
  return (
    <Shown
      loaded={listed}
      reading={t({ defaultMessage: "Reading your sessions…", id: "JyQxWYW1" })}
      done={(list) => (
        <ul>
          {list.map((s) => (
            <li key={s.id}>
              {t(
                {
                  defaultMessage:
                    "{kind, select, cli {Command line} other {Browser}}: {agent}, from {source}{used, select, yes {, last used {lastUsed}} other {}}",
                  id: "kBgKnjmv",
                  description: "one session: its kind, client, address, and last use",
                },
                {
                  kind: s.kind,
                  agent:
                    s.userAgent === ""
                      ? t({
                          defaultMessage: "unnamed",
                          id: "PRlKOXbY",
                          description: "a session with no client name",
                        })
                      : s.userAgent,
                  source: s.source,
                  used: s.lastUsedAt === undefined ? "no" : "yes",
                  lastUsed:
                    s.lastUsedAt === undefined
                      ? ""
                      : intl.formatDate(timestampDate(s.lastUsedAt), {
                          dateStyle: "medium",
                          timeStyle: "short",
                        }),
                },
              )}
              {s.current ? (
                <strong className="aside">
                  {t({
                    defaultMessage: "(this one)",
                    id: "1G5SDr8l",
                    description: "the session in use now",
                  })}
                </strong>
              ) : (
                <Button
                  onPress={() => {
                    props.end(s);
                  }}
                >
                  {t({
                    defaultMessage: "End it",
                    id: "3jqpN1eS",
                    description: "button: end a session",
                  })}
                </Button>
              )}
            </li>
          ))}
        </ul>
      )}
    />
  );
}
