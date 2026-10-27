-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A block class's devices (§2.1 of docs/06-storage.md): one per volume, made by the class's
-- provider, the `block_storage` provider of the class's name that the volume's organization sees,
-- and attached to one node for one lease epoch. A device is wanted before its volume exists, so
-- it is kept apart from the volume, by the store and the identifier its volume will have there,
-- and its identifier is the one its volume takes when its node reports it made; its name is the
-- volume's in its workload's declaration, which the workload's status shows it by. Each row keeps
-- the provider's name for the device, the node and epoch it was last attached for, what that
-- node needs to attach it (the credential sealed to the device by the control plane's device
-- key), and the state of asking: why the last call failed, how many have failed since one
-- answered, when to ask next, and until when a control plane holds it while it asks.

CREATE TABLE block_device (
    id                uuid PRIMARY KEY,
    organization_id   uuid NOT NULL REFERENCES organization (id),
    workload_id       uuid NOT NULL REFERENCES workload (id),
    store             text NOT NULL,
    helper_id         text NOT NULL,
    name              text NOT NULL,
    storage_class     text NOT NULL,
    size_bytes        bigint NOT NULL CHECK (size_bytes > 0),
    created_at        timestamptz NOT NULL,
    device            text,
    attached_node_id  uuid REFERENCES node (id),
    attached_epoch    bigint,
    protocol          text CHECK (protocol IN ('iscsi', 'rbd', 'loop')),
    target            text,
    credential        bytea,
    sealed_by         text,
    reason            text,
    attempts          integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    due               timestamptz,
    held_until        timestamptz,
    deleted_at        timestamptz,
    CONSTRAINT block_device_attached CHECK (
        (attached_node_id IS NULL) = (attached_epoch IS NULL)
        AND (protocol IS NULL) = (target IS NULL)
        AND (credential IS NULL) = (sealed_by IS NULL))
);

-- One live device per volume to be; a deleted one is kept, and the identifier freed.
CREATE UNIQUE INDEX block_device_volume ON block_device (store, helper_id)
    WHERE deleted_at IS NULL;
CREATE INDEX block_device_workload ON block_device (workload_id);

GRANT SELECT, INSERT, UPDATE ON block_device TO korpis_app;

-- A device attached to another node or for another epoch changes what its workload's assignment
-- carries; nothing else of it does.
CREATE TRIGGER block_device_notify AFTER UPDATE ON block_device
    FOR EACH ROW WHEN (NEW.attached_node_id IS DISTINCT FROM OLD.attached_node_id
                       OR NEW.attached_epoch IS DISTINCT FROM OLD.attached_epoch)
    EXECUTE FUNCTION korpis_notify_workload();
