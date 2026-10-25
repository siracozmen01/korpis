// SPDX-License-Identifier: AGPL-3.0-or-later

//! WireGuard's generic netlink family, spoken on a socket of the calling thread's network
//! namespace: its family's identifier asked of the controller, a device's configuration set, and
//! its public key and each peer's latest handshake read back.

use netlink_packet_core::{
    NLM_F_ACK, NLM_F_DUMP, NLM_F_REQUEST, NetlinkHeader, NetlinkMessage, NetlinkPayload,
    NetlinkSerializable,
};
use netlink_packet_generic::GenlMessage;
use netlink_packet_generic::ctrl::nlas::GenlCtrlAttrs;
use netlink_packet_generic::ctrl::{GenlCtrl, GenlCtrlCmd};
use netlink_packet_wireguard::{
    WireguardAttribute, WireguardCmd, WireguardMessage, WireguardPeerAttribute,
};
use netlink_sys::protocols::NETLINK_GENERIC;
use netlink_sys::{Socket, SocketAddr};
use std::io;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A WireGuard public key.
pub(crate) type Key = [u8; WireguardAttribute::WG_KEY_LEN];

/// A generic netlink socket, and WireGuard's family on it.
pub(crate) struct Wg {
    socket: Socket,
    seq: u32,
    family: u16,
}

/// What a device says of itself: its public key, and each peer's latest handshake.
#[derive(Debug, Default)]
pub(crate) struct Device {
    pub public_key: Option<Key>,
    pub handshakes: Vec<(Key, Option<SystemTime>)>,
}

fn invalid(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

/// Adds to `answers` the messages of `datagram` answering request `seq`; whether one ended it.
fn read_datagram<F>(
    datagram: &[u8],
    seq: u32,
    answers: &mut Vec<GenlMessage<F>>,
) -> io::Result<bool>
where
    GenlMessage<F>: netlink_packet_core::NetlinkDeserializable,
    F: std::fmt::Debug,
{
    let mut at = 0;
    while let Some(rest) = datagram.get(at..).filter(|r| !r.is_empty()) {
        let answer = NetlinkMessage::<GenlMessage<F>>::deserialize(rest).map_err(invalid)?;
        // Each message is padded to four bytes; a header is sixteen.
        at += (usize::try_from(answer.header.length).unwrap_or(0).max(16) + 3) & !3;
        if answer.header.sequence_number != seq {
            continue;
        }
        match answer.payload {
            NetlinkPayload::InnerMessage(m) => answers.push(m),
            NetlinkPayload::Error(e) if e.code.is_some() => return Err(e.to_io()),
            NetlinkPayload::Error(_) | NetlinkPayload::Done(_) => return Ok(true),
            _ => {}
        }
    }
    Ok(false)
}

impl Wg {
    /// A socket in the calling thread's network namespace, with WireGuard's family resolved.
    ///
    /// # Errors
    /// The socket could not be opened, or the kernel has no WireGuard.
    pub(crate) fn open() -> io::Result<Self> {
        let mut socket = Socket::new(NETLINK_GENERIC)?;
        socket.bind_auto()?;
        socket.connect(&SocketAddr::new(0, 0))?;
        let mut wg = Self { socket, seq: 0, family: 0 };
        let ask = GenlCtrl {
            cmd: GenlCtrlCmd::GetFamily,
            nlas: vec![GenlCtrlAttrs::FamilyName("wireguard".to_owned())],
        };
        let answers = wg.exchange(GenlMessage::from_payload(ask), NLM_F_REQUEST | NLM_F_ACK)?;
        wg.family = answers
            .into_iter()
            .flat_map(|m: GenlMessage<GenlCtrl>| m.payload.nlas)
            .find_map(|a| if let GenlCtrlAttrs::FamilyId(id) = a { Some(id) } else { None })
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "the kernel has no WireGuard")
            })?;
        Ok(wg)
    }

    /// Sends `message` with `flags` and reads every answer to it.
    fn exchange<F>(
        &mut self,
        mut message: GenlMessage<F>,
        flags: u16,
    ) -> io::Result<Vec<GenlMessage<F>>>
    where
        GenlMessage<F>: NetlinkSerializable + netlink_packet_core::NetlinkDeserializable,
        F: std::fmt::Debug,
    {
        self.seq = self.seq.wrapping_add(1);
        message.set_resolved_family_id(self.family);
        let mut header = NetlinkHeader::default();
        header.flags = flags;
        header.sequence_number = self.seq;
        let mut request = NetlinkMessage::new(header, NetlinkPayload::from(message));
        request.finalize();
        let mut bytes = vec![0; request.buffer_len()];
        request.serialize(&mut bytes);
        self.socket.send(&bytes, 0)?;
        let mut answers = Vec::new();
        loop {
            let (datagram, _) = self.socket.recv_from_full()?;
            if read_datagram(&datagram, self.seq, &mut answers)? {
                return Ok(answers);
            }
        }
    }

    /// Sets the device `attributes` name, which name it.
    pub(crate) fn set(&mut self, attributes: Vec<WireguardAttribute>) -> io::Result<()> {
        let set = WireguardMessage { cmd: WireguardCmd::SetDevice, attributes };
        self.exchange(GenlMessage::from_payload(set), NLM_F_REQUEST | NLM_F_ACK).map(drop)
    }

    /// What the device named `name` says of itself.
    pub(crate) fn get(&mut self, name: &str) -> io::Result<Device> {
        let get = WireguardMessage {
            cmd: WireguardCmd::GetDevice,
            attributes: vec![WireguardAttribute::IfName(name.to_owned())],
        };
        let mut device = Device::default();
        let dump = self.exchange(GenlMessage::from_payload(get), NLM_F_REQUEST | NLM_F_DUMP)?;
        for attribute in dump.into_iter().flat_map(|a| a.payload.attributes) {
            match attribute {
                WireguardAttribute::PublicKey(k) => device.public_key = Some(k),
                WireguardAttribute::Peers(peers) => {
                    device.handshakes.extend(peers.iter().filter_map(|p| handshake(&p.0)));
                }
                _ => {}
            }
        }
        Ok(device)
    }
}

/// A peer's key and the time of its latest handshake, none for never.
fn handshake(peer: &[WireguardPeerAttribute]) -> Option<(Key, Option<SystemTime>)> {
    let key = peer.iter().find_map(|a| match a {
        WireguardPeerAttribute::PublicKey(k) => Some(*k),
        _ => None,
    })?;
    let at = peer.iter().find_map(|a| match a {
        WireguardPeerAttribute::LastHandshake(t) if t.seconds > 0 => {
            let secs = u64::try_from(t.seconds).ok()?;
            let nanos = u32::try_from(t.nano_seconds).unwrap_or(0);
            Some(UNIX_EPOCH + Duration::new(secs, nanos))
        }
        _ => None,
    });
    Some((key, at))
}
