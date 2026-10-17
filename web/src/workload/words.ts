// SPDX-License-Identifier: AGPL-3.0-or-later

// The words the workload screens use for the API's states, each described by the state it names,
// so a language that words "running" differently for a declaration and an observation can.

import { Convergence } from "../gen/korpis/v1/api_pb";
import {
  HealthState,
  ObservedState,
  PowerState,
  UnsatisfiableReason,
} from "../gen/korpis/v1/workload_pb";
import { t } from "../i18n/intl";

/** What its owner declared it should be doing. */
export const DECLARED: Record<PowerState, string> = {
  [PowerState.UNSPECIFIED]: t({
    defaultMessage: "nothing declared",
    id: "9G511qz9",
    description: "a workload's declared power state",
  }),
  [PowerState.RUNNING]: t({
    defaultMessage: "running",
    id: "nVDoOwm8",
    description: "a workload's declared power state",
  }),
  [PowerState.STOPPED]: t({
    defaultMessage: "stopped",
    id: "05J8UAPw",
    description: "a workload's declared power state",
  }),
};

/** Where a workload stands against what was declared. */
export const CONVERGENCE: Record<Convergence, string> = {
  [Convergence.UNSPECIFIED]: t({
    defaultMessage: "unknown",
    id: "WlKwuX88",
    description: "where a workload stands against what was declared",
  }),
  [Convergence.CONVERGED]: t({
    defaultMessage: "converged",
    id: "yS/twS6R",
    description: "where a workload stands against what was declared",
  }),
  [Convergence.APPLYING]: t({
    defaultMessage: "applying",
    id: "WQNWGK22",
    description: "where a workload stands against what was declared",
  }),
  [Convergence.AWAITING_APPROVAL]: t({
    defaultMessage: "awaiting approval",
    id: "2ku0j3Lw",
    description: "where a workload stands against what was declared",
  }),
  [Convergence.SCHEDULED]: t({
    defaultMessage: "scheduled",
    id: "xIzLahez",
    description: "where a workload stands against what was declared",
  }),
  [Convergence.FAILING]: t({
    defaultMessage: "failing",
    id: "AX61Yquk",
    description: "where a workload stands against what was declared",
  }),
  [Convergence.UNKNOWN]: t({
    defaultMessage: "unknown",
    id: "WlKwuX88",
    description: "where a workload stands against what was declared",
  }),
  [Convergence.UNPLACED]: t({
    defaultMessage: "not placed",
    id: "9+4JL8Ec",
    description: "where a workload stands against what was declared",
  }),
};

/** What the node saw it doing. */
export const OBSERVED: Record<ObservedState, string> = {
  [ObservedState.UNSPECIFIED]: t({
    defaultMessage: "unknown",
    id: "BIIyPmVF",
    description: "a workload's state as its node observed it",
  }),
  [ObservedState.RUNNING]: t({
    defaultMessage: "running",
    id: "iitdQxcB",
    description: "a workload's state as its node observed it",
  }),
  [ObservedState.STOPPED]: t({
    defaultMessage: "stopped",
    id: "kn6QOTAC",
    description: "a workload's state as its node observed it",
  }),
  [ObservedState.STARTING]: t({
    defaultMessage: "starting",
    id: "Z12PiHVA",
    description: "a workload's state as its node observed it",
  }),
  [ObservedState.STOPPING]: t({
    defaultMessage: "stopping",
    id: "3K8BEHus",
    description: "a workload's state as its node observed it",
  }),
  [ObservedState.CRASHED]: t({
    defaultMessage: "crashed",
    id: "sCY1Nc/A",
    description: "a workload's state as its node observed it",
  }),
  [ObservedState.UNSATISFIABLE]: t({
    defaultMessage: "cannot run",
    id: "07sHvW3R",
    description: "a workload's state as its node observed it",
  }),
  [ObservedState.UNKNOWN]: t({
    defaultMessage: "unknown",
    id: "BIIyPmVF",
    description: "a workload's state as its node observed it",
  }),
};

/** Its health check's answer. */
export const HEALTH: Record<HealthState, string> = {
  [HealthState.UNSPECIFIED]: t({
    defaultMessage: "unknown",
    id: "ka5UfM/i",
    description: "a workload's health check answer",
  }),
  [HealthState.HEALTHY]: t({
    defaultMessage: "healthy",
    id: "fcokJlVA",
    description: "a workload's health check answer",
  }),
  [HealthState.UNHEALTHY]: t({
    defaultMessage: "unhealthy",
    id: "xV3WEmSo",
    description: "a workload's health check answer",
  }),
  [HealthState.UNKNOWN]: t({
    defaultMessage: "unknown",
    id: "ka5UfM/i",
    description: "a workload's health check answer",
  }),
};

/** Why a node cannot run it (§7 of docs/05-scheduling.md). */
export const UNSATISFIABLE: Record<UnsatisfiableReason, string> = {
  [UnsatisfiableReason.UNSPECIFIED]: t({
    defaultMessage: "for a reason it did not give",
    id: "jbhLQnPx",
    description: "why a node cannot run a workload, completing 'it cannot run there because'",
  }),
  [UnsatisfiableReason.MISSING_DRIVER]: t({
    defaultMessage: "its runtime driver is missing",
    id: "NuSEztoC",
    description: "why a node cannot run a workload, completing 'it cannot run there because'",
  }),
  [UnsatisfiableReason.INSUFFICIENT_RESOURCES]: t({
    defaultMessage: "it has too few resources left",
    id: "YxsNQ0kQ",
    description: "why a node cannot run a workload, completing 'it cannot run there because'",
  }),
  [UnsatisfiableReason.VOLUME_UNAVAILABLE]: t({
    defaultMessage: "a volume is unavailable",
    id: "9YypKw6B",
    description: "why a node cannot run a workload, completing 'it cannot run there because'",
  }),
  [UnsatisfiableReason.DEVICE_UNAVAILABLE]: t({
    defaultMessage: "a device is unavailable",
    id: "mJqqsxJJ",
    description: "why a node cannot run a workload, completing 'it cannot run there because'",
  }),
  [UnsatisfiableReason.IMAGE_UNSUPPORTED]: t({
    defaultMessage: "its image is not supported there",
    id: "+e3kx38m",
    description: "why a node cannot run a workload, completing 'it cannot run there because'",
  }),
  [UnsatisfiableReason.PREPARE_REJECTED]: t({
    defaultMessage: "preparing it was refused",
    id: "KZ0aUUdu",
    description: "why a node cannot run a workload, completing 'it cannot run there because'",
  }),
};
