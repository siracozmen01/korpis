// SPDX-License-Identifier: AGPL-3.0-or-later

// The words a run is shown with (§4 of docs/05-scheduling.md): how it was made, where it is, and
// why it ended, each described by what it names.

import { RunCause, RunStatus, RunTrigger } from "../gen/korpis/v1/workload_pb";
import { t } from "../i18n/intl";

/** What made a run. */
export const TRIGGER: Record<RunTrigger, string> = {
  [RunTrigger.UNSPECIFIED]: t({
    defaultMessage: "unknown",
    id: "slJc0FW2",
    description: "what made a run",
  }),
  [RunTrigger.SCHEDULE]: t({
    defaultMessage: "schedule",
    id: "Sys9byfL",
    description: "what made a run",
  }),
  [RunTrigger.MANUAL]: t({
    defaultMessage: "requested",
    id: "5RQmDSnU",
    description: "what made a run",
  }),
  [RunTrigger.RETRY]: t({
    defaultMessage: "retry",
    id: "zm+/6FZh",
    description: "what made a run",
  }),
  [RunTrigger.EVENT]: t({
    defaultMessage: "event",
    id: "0gv3uCa8",
    description: "what made a run",
  }),
};

/** Where a run is. */
export const STATUS: Record<RunStatus, string> = {
  [RunStatus.UNSPECIFIED]: t({
    defaultMessage: "unknown",
    id: "zEu3E6rp",
    description: "a run's status",
  }),
  [RunStatus.PENDING]: t({
    defaultMessage: "pending",
    id: "du4X8Qzs",
    description: "a run's status",
  }),
  [RunStatus.RUNNING]: t({
    defaultMessage: "running",
    id: "KMVdwwFd",
    description: "a run's status",
  }),
  [RunStatus.SUCCEEDED]: t({
    defaultMessage: "succeeded",
    id: "Cc3ziaQW",
    description: "a run's status",
  }),
  [RunStatus.FAILED]: t({
    defaultMessage: "failed",
    id: "1eh18tF2",
    description: "a run's status",
  }),
  [RunStatus.CANCELLED]: t({
    defaultMessage: "cancelled",
    id: "2tn23Y+t",
    description: "a run's status",
  }),
  [RunStatus.MISSED]: t({
    defaultMessage: "missed",
    id: "vAYViqnE",
    description: "a run's status",
  }),
};

/** Why a run ended. */
export const CAUSE: Record<RunCause, string> = {
  [RunCause.UNSPECIFIED]: "",
  [RunCause.EXIT]: t({
    defaultMessage: "it exited",
    id: "CQMn8XYy",
    description: "why a run ended",
  }),
  [RunCause.TIMEOUT]: t({
    defaultMessage: "its timeout",
    id: "7gPpqPv6",
    description: "why a run ended",
  }),
  [RunCause.CANCELLED]: t({
    defaultMessage: "cancelled",
    id: "Hy1yFnf+",
    description: "why a run ended",
  }),
  [RunCause.STOPPED]: t({
    defaultMessage: "its workload was stopped",
    id: "L0/tjXb5",
    description: "why a run ended",
  }),
  [RunCause.REPLACED]: t({
    defaultMessage: "a newer run replaced it",
    id: "bfi2hPPn",
    description: "why a run ended",
  }),
  [RunCause.CONCURRENCY]: t({
    defaultMessage: "another run was active",
    id: "Tj+uaRpH",
    description: "why a run ended",
  }),
  [RunCause.NOT_STARTED]: t({
    defaultMessage: "its node never started it",
    id: "2RtZOa79",
    description: "why a run ended",
  }),
  [RunCause.SKIPPED]: t({
    defaultMessage: "missed while the control plane was down",
    id: "/SF8inMU",
    description: "why a run ended",
  }),
  [RunCause.NODE_LOST]: t({
    defaultMessage: "its node was lost",
    id: "Ck0eQc3Q",
    description: "why a run ended",
  }),
};
