// SPDX-License-Identifier: AGPL-3.0-or-later

//! A DNS server for tests: TXT records held in memory and answered over UDP and TCP on one
//! loopback port, so a domain is verified, and an ACME challenge validated, against records a test
//! publishes and withdraws.

use hickory_proto::op::{Message, MessageType, ResponseCode};
use hickory_proto::rr::rdata::TXT;
use hickory_proto::rr::{RData, Record, RecordType};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, UdpSocket};

/// TXT records by name, without the final dot.
#[derive(Debug, Clone, Default)]
pub struct Zone(Arc<Mutex<BTreeMap<String, Vec<String>>>>);

impl Zone {
    /// Serves the zone on a loopback port, over UDP and TCP, until the test ends.
    pub async fn serve() -> (Self, SocketAddr) {
        let zone = Self::default();
        for _ in 0..20 {
            let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let at = socket.local_addr().unwrap();
            let Ok(listener) = TcpListener::bind(at).await else { continue };
            tokio::spawn(answer_on(socket, zone.clone()));
            tokio::spawn(answer_streams(listener, zone.clone()));
            return (zone, at);
        }
        panic!("no loopback port free for both UDP and TCP");
    }

    /// Publishes a TXT record at `name`.
    pub fn publish(&self, name: &str, text: &str) {
        self.0.lock().unwrap().entry(name.to_owned()).or_default().push(text.to_owned());
    }

    /// Withdraws every record at `name`.
    pub fn withdraw(&self, name: &str) {
        self.0.lock().unwrap().remove(name);
    }

    fn answer(&self, query: &[u8]) -> Option<Vec<u8>> {
        let query = Message::from_vec(query).ok()?;
        let mut reply = Message::response(query.metadata.id, query.metadata.op_code);
        reply.metadata.recursion_desired = query.metadata.recursion_desired;
        reply.metadata.recursion_available = true;
        reply.metadata.authoritative = true;
        let q = query.queries.first()?.clone();
        let name = q.name().to_ascii().trim_end_matches('.').to_owned();
        let found = self.0.lock().unwrap().get(&name).cloned().unwrap_or_default();
        if found.is_empty() || q.query_type() != RecordType::TXT {
            reply.metadata.response_code = ResponseCode::NXDomain;
        }
        if q.query_type() == RecordType::TXT {
            for text in found {
                let rdata = RData::TXT(TXT::new(vec![text]));
                reply.answers.push(Record::from_rdata(q.name().clone(), 0, rdata));
            }
        }
        reply.add_query(q);
        debug_assert_eq!(reply.metadata.message_type, MessageType::Response);
        reply.to_vec().ok()
    }
}

/// Answers every query on every connection `listener` accepts, each framed by its length.
async fn answer_streams(listener: TcpListener, zone: Zone) {
    while let Ok((stream, _)) = listener.accept().await {
        tokio::spawn(answer_stream(stream, zone.clone()));
    }
}

/// Answers the queries of one connection until it closes.
async fn answer_stream(mut stream: tokio::net::TcpStream, zone: Zone) -> Option<()> {
    let mut length = [0u8; 2];
    while stream.read_exact(&mut length).await.is_ok() {
        let mut query = vec![0u8; usize::from(u16::from_be_bytes(length))];
        stream.read_exact(&mut query).await.ok()?;
        let answer = zone.answer(&query)?;
        let n = u16::try_from(answer.len()).ok()?;
        stream.write_all(&[n.to_be_bytes().as_slice(), &answer].concat()).await.ok()?;
    }
    Some(())
}

/// Answers every query that reaches `socket` from `zone`.
async fn answer_on(socket: UdpSocket, zone: Zone) {
    let mut buf = [0u8; 1500];
    while let Ok((n, from)) = socket.recv_from(&mut buf).await {
        let Some(answer) = zone.answer(&buf[..n]) else { continue };
        let _ = socket.send_to(&answer, from).await;
    }
}
