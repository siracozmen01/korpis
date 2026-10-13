// SPDX-License-Identifier: AGPL-3.0-or-later

//! End-to-end tests of a Korpis node, in `tests/`: a control plane installed into a throwaway
//! database, an agent joined to it, and the agent's real `oci` driver and privileged helper,
//! against this machine's containerd. The crate holds no code of its own; it exists so the
//! programs can meet in one test without any of them depending on the others.
