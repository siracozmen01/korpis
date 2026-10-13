-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A lease issued, moved, or released changes what a node's assignment carries (its lease epoch,
-- §4.8 of docs/02-architecture.md), so it announces the workload like every other change an
-- assignment is made of. A renewal only moves when the grantor heard the holder, which changes
-- nothing a node is told, so it announces nothing: a renewal per workload per term would
-- otherwise recompute every set on every renewal.

CREATE TRIGGER lease_notify AFTER INSERT OR UPDATE OF epoch, node_id, released_at ON lease
    FOR EACH ROW EXECUTE FUNCTION korpis_notify_workload();
