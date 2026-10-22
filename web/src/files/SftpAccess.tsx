// SPDX-License-Identifier: AGPL-3.0-or-later

// Where an SFTP client reaches a workload's files (§8 of docs/11-surface-web.md, §3.1 of
// docs/02-architecture.md): for each server `ServerInfo` names, the line a client takes, the host
// key's fingerprint a client shows on its first connection, and a `known_hosts` line, so that
// connection is checked rather than trusted. Nothing where no server serves SFTP, or the
// control plane could not be asked: the file manager above is the panel's own way in.

import { useCallback } from "react";
import { useReach } from "../clients";
import type { SftpServer } from "../gen/korpis/v1/api_pb";
import { t } from "../i18n/intl";
import { useLoad } from "../views/load";

/** What a client is given to reach workload `id` at `s`. */
export function command(id: string, s: SftpServer): string {
  const at = s.address.lastIndexOf(":");
  const host = at < 0 ? s.address : s.address.slice(0, at);
  const port = at < 0 ? "22" : s.address.slice(at + 1);
  return port === "22" ? `sftp ${id}@${host}` : `sftp -P ${port} ${id}@${host}`;
}

/** The line a `known_hosts` file takes for `s`. */
export function knownHostsLine(s: SftpServer): string {
  return `${s.knownHostsName} ${s.hostKey}`;
}

/** The SFTP servers that reach workload `id`, with a key `korpis ssh-key add` registered. */
export function SftpAccess(props: { readonly id: string }) {
  const { clients } = useReach();
  const info = useLoad(useCallback(async () => clients.serverInfo.getServerInfo({}), [clients]));
  const servers = info.state === "done" ? info.value.sftp : [];
  if (servers.length === 0) return null;
  return (
    <section aria-labelledby="sftp">
      <h3 id="sftp">{t({ defaultMessage: "Over SFTP", id: "z9pGS9Fq" })}</h3>
      <p>
        {t({
          defaultMessage:
            "An SFTP client reaches these files with an SSH key added to your profile. Compare the key it shows on its first connection with the one here.",
          id: "BwQ8jN2w",
        })}
      </p>
      <ul>
        {servers.map((s) => (
          <li key={s.address} className="kept-lines">
            <code>{command(props.id, s)}</code>
            <span>
              {t(
                { defaultMessage: "Host key {fingerprint}", id: "A2OYdBg8" },
                { fingerprint: s.fingerprint },
              )}
            </span>
            <code>{knownHostsLine(s)}</code>
          </li>
        ))}
      </ul>
    </section>
  );
}
