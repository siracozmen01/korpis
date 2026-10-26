-- SPDX-License-Identifier: AGPL-3.0-or-later

DROP TABLE stable_port;
DELETE FROM address_pool WHERE scope = 'edge';
ALTER TABLE address_pool DROP CONSTRAINT address_pool_routed;
ALTER TABLE address_pool DROP CONSTRAINT address_pool_group;
ALTER TABLE address_pool DROP COLUMN group_id;
ALTER TABLE address_pool ADD CONSTRAINT address_pool_check1
    CHECK ((scope = 'cluster') = (router IS NOT NULL));
ALTER TABLE address_pool ADD CONSTRAINT address_pool_check2
    CHECK ((scope = 'node') = (routing = 'static'));
DROP TABLE edge_member;
DROP TABLE edge_group;
