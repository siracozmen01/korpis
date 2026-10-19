// SPDX-License-Identifier: AGPL-3.0-or-later

// A quota dimension's name, as the panel says it (§5.1 of docs/05-scheduling.md). Read at
// render, so the name is in the language the panel is shown in.

import { t } from "../i18n/intl";
import type { Dimension } from "./quota";

/** What `d` is called on the screen. */
export function dimensionName(d: Dimension): string {
  switch (d) {
    case "cpuMillis":
      return t({ defaultMessage: "CPU (millicores)", id: "mSCpuTgp" });
    case "memoryBytes":
      return t({ defaultMessage: "Memory", id: "dVx3yznM" });
    case "diskBytes":
      return t({ defaultMessage: "Disk", id: "OuCSDnvE" });
    case "diskReadBytesPerSec":
      return t({ defaultMessage: "Disk reads (bytes/s)", id: "3i0BH2Sx" });
    case "diskWriteBytesPerSec":
      return t({ defaultMessage: "Disk writes (bytes/s)", id: "6vqEzPIc" });
    case "diskReadOpsPerSec":
      return t({ defaultMessage: "Disk reads (operations/s)", id: "mZ/c+AKf" });
    case "diskWriteOpsPerSec":
      return t({ defaultMessage: "Disk writes (operations/s)", id: "ZEgIPrqb" });
    case "bandwidthBitsPerSec":
      return t({ defaultMessage: "Egress (bits/s)", id: "O2ptnHRy" });
    case "workloads":
      return t({ defaultMessage: "Workloads", id: "BeBCMfk4" });
    case "volumes":
      return t({ defaultMessage: "Volumes", id: "cyDRxetx" });
    case "endpoints":
      return t({ defaultMessage: "Endpoints", id: "cQyKR1uH" });
    case "dedicatedIpv4":
      return t({ defaultMessage: "Dedicated IPv4 addresses", id: "QKVDcL4/" });
    case "nodePorts":
      return t({ defaultMessage: "Node ports", id: "x2GhgpDT" });
  }
}
