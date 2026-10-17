// SPDX-License-Identifier: AGPL-3.0-or-later

// The words the workload screens use for the API's states.

import { Convergence } from "../gen/korpis/v1/api_pb";
import {
  HealthState,
  ObservedState,
  PowerState,
  UnsatisfiableReason,
} from "../gen/korpis/v1/workload_pb";

/** What its owner declared it should be doing. */
export const DECLARED: Record<PowerState, string> = {
  [PowerState.UNSPECIFIED]: "nothing declared",
  [PowerState.RUNNING]: "running",
  [PowerState.STOPPED]: "stopped",
};

/** Where a workload stands against what was declared. */
export const CONVERGENCE: Record<Convergence, string> = {
  [Convergence.UNSPECIFIED]: "unknown",
  [Convergence.CONVERGED]: "converged",
  [Convergence.APPLYING]: "applying",
  [Convergence.AWAITING_APPROVAL]: "awaiting approval",
  [Convergence.SCHEDULED]: "scheduled",
  [Convergence.FAILING]: "failing",
  [Convergence.UNKNOWN]: "unknown",
  [Convergence.UNPLACED]: "not placed",
};

/** What the node saw it doing. */
export const OBSERVED: Record<ObservedState, string> = {
  [ObservedState.UNSPECIFIED]: "unknown",
  [ObservedState.RUNNING]: "running",
  [ObservedState.STOPPED]: "stopped",
  [ObservedState.STARTING]: "starting",
  [ObservedState.STOPPING]: "stopping",
  [ObservedState.CRASHED]: "crashed",
  [ObservedState.UNSATISFIABLE]: "cannot run",
  [ObservedState.UNKNOWN]: "unknown",
};

/** Its health check's answer. */
export const HEALTH: Record<HealthState, string> = {
  [HealthState.UNSPECIFIED]: "unknown",
  [HealthState.HEALTHY]: "healthy",
  [HealthState.UNHEALTHY]: "unhealthy",
  [HealthState.UNKNOWN]: "unknown",
};

/** Why a node cannot run it (§7 of docs/05-scheduling.md). */
export const UNSATISFIABLE: Record<UnsatisfiableReason, string> = {
  [UnsatisfiableReason.UNSPECIFIED]: "for a reason it did not give",
  [UnsatisfiableReason.MISSING_DRIVER]: "its runtime driver is missing",
  [UnsatisfiableReason.INSUFFICIENT_RESOURCES]: "it has too few resources left",
  [UnsatisfiableReason.VOLUME_UNAVAILABLE]: "a volume is unavailable",
  [UnsatisfiableReason.DEVICE_UNAVAILABLE]: "a device is unavailable",
  [UnsatisfiableReason.IMAGE_UNSUPPORTED]: "its image is not supported there",
  [UnsatisfiableReason.PREPARE_REJECTED]: "preparing it was refused",
};
