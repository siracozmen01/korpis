-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- When a relay's feed was last served (§3.1 of docs/02-architecture.md): written by the replica
-- feeding it, at most every thirty seconds, so ListRelays can say whether it is connected.

ALTER TABLE relay ADD COLUMN last_seen_at timestamptz;
