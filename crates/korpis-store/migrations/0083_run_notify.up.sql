-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A run made, started, or ended announces its workload (§4.3 of docs/02-architecture.md), as
-- every other change to what a workload's status and assignment are read from does: its node is
-- told which runs to start and stop, and a watcher of the workload hears that its runs changed.

CREATE TRIGGER run_notify AFTER INSERT OR UPDATE OR DELETE ON run
    FOR EACH ROW EXECUTE FUNCTION korpis_notify_workload();
