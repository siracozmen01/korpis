// SPDX-License-Identifier: AGPL-3.0-or-later

// What each action a share link may hold lets its holder do, in the words both the share screen
// and the link's own screen use (§6 of docs/11-surface-web.md), so the two never say it apart.

import { t } from "../i18n/intl";

/** What each action lets a link's holder do, completing "they can". */
export const ALLOWS: Readonly<Record<string, string>> = {
  "workload.read": t({ defaultMessage: "see its status", id: "9YURJKmQ" }),
  "workload.console.read": t({ defaultMessage: "read its console", id: "JFlcSpSl" }),
  "workload.console.write": t({ defaultMessage: "send commands to its console", id: "Y0g70Mpa" }),
  "workload.start": t({ defaultMessage: "start it", id: "sYQQIZsv" }),
  "workload.stop": t({ defaultMessage: "stop it", id: "qYynOpiu" }),
  "workload.restart": t({ defaultMessage: "restart it", id: "Uw77NfUG" }),
  "workload.file.read": t({ defaultMessage: "read its files", id: "9HUO3CrK" }),
  "workload.file.write": t({ defaultMessage: "change its files", id: "EildnEYR" }),
  "workload.exec": t({ defaultMessage: "run commands in it", id: "3Py1cc+q" }),
};
