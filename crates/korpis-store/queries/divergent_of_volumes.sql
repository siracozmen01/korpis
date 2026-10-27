-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The copies left on nodes the volumes of $1 failed over from, live ones, and of those volumes the
-- ones that are such copies themselves (§2.2 of docs/06-storage.md), oldest first.
SELECT id, divergent_of AS "divergent_of!", node_id, divergent_since AS "since!",
       held_until AS "held_until!", deleted_at
  FROM volume
 WHERE divergent_of IS NOT NULL AND (divergent_of = ANY($1) OR id = ANY($1))
 ORDER BY divergent_since, id
