// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use std::net::Ipv4Addr;

/// A query for `example.org`, type A, with identifier `id`.
fn query(id: u16) -> Vec<u8> {
    let mut q = id.to_be_bytes().to_vec();
    q.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    q.extend_from_slice(b"\x07example\x03org\x00\x00\x01\x00\x01");
    q
}

#[test]
fn a_refusal_keeps_the_identifier_and_the_question() {
    let q = query(0x1234);
    let r = message::refused(&q).unwrap();
    assert_eq!(&r[..2], &[0x12, 0x34]);
    assert_eq!(
        (r[2] & 0x80, r[2] & 0x01, r[3] & 0x0f),
        (0x80, 0x01, 5),
        "an answer, RD kept, REFUSED"
    );
    assert_eq!(&r[4..6], &[0, 1]);
    assert_eq!(&r[12..], &q[12..], "its question");
    assert!(message::refused(&r).is_none(), "an answer is not refused");
    assert!(message::refused(&q[..5]).is_none(), "nor what is not a message");
    let truncated = message::refused(&q[..20]).unwrap();
    assert_eq!((truncated.len(), truncated[5]), (12, 0), "a question it cannot read is left out");
    assert_eq!(failed(&q, SERVFAIL).unwrap()[3] & 0x0f, 2);
}

#[test]
fn the_host_s_resolvers_are_read_in_order() {
    let conf = "# stub\nnameserver 127.0.0.53\noptions edns0\nnameserver 2001:db8::1\nnameserver fe80::1%eth0\n";
    let got = message::upstreams(conf, 53);
    assert_eq!(got, ["127.0.0.53:53".parse().unwrap(), "[2001:db8::1]:53".parse().unwrap()]);
}

#[test]
fn a_workload_has_its_burst_and_then_its_rate() {
    let mut b = bucket::Bucket::default();
    let now = Instant::now();
    let taken = (0..300).filter(|_| b.take(now)).count();
    assert_eq!(taken, 200, "the burst, and no more at once");
    assert!(!b.take(now), "past it, refused");
    assert!(b.take(now + Duration::from_millis(100)), "and the rate refills");
}

/// A resolver forwarding to an upstream on loopback at `upstream_port`, answering on loopback
/// sockets as it would on those the helper opens in a workload's namespace.
fn resolver(
    dir: &std::path::Path,
    upstream_port: u16,
) -> (Resolver, Bound, SocketAddr, SocketAddr) {
    let conf = dir.join("resolv.conf");
    std::fs::write(&conf, "nameserver 127.0.0.1\n").unwrap();
    let mut r = Resolver::new(conf);
    r.upstream_port = upstream_port;
    let udp = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let tcp = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let at = (udp.local_addr().unwrap(), tcp.local_addr().unwrap());
    (r, Bound::new(udp, tcp).unwrap(), at.0, at.1)
}

/// An upstream that answers each UDP query with the query, marked as an answer.
async fn echo_upstream() -> u16 {
    let s = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = s.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut buf = vec![0; 512];
        while let Ok((n, from)) = s.recv_from(&mut buf).await {
            let mut a = buf[..n].to_vec();
            a[2] |= 0x80;
            let _ = s.send_to(&a, from).await;
        }
    });
    port
}

#[tokio::test]
async fn a_workload_s_query_is_forwarded_and_past_its_rate_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (r, bound, at, _) = resolver(dir.path(), echo_upstream().await);
    tokio::spawn(r.serve(bound));
    let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    client.connect(at).await.unwrap();
    let mut buf = vec![0; 512];
    client.send(&query(7)).await.unwrap();
    let n =
        tokio::time::timeout(Duration::from_secs(2), client.recv(&mut buf)).await.unwrap().unwrap();
    assert_eq!(
        (&buf[..2], buf[3] & 0x0f),
        (&7u16.to_be_bytes()[..], 0),
        "forwarded: {:?}",
        &buf[..n]
    );

    for id in 0..300 {
        client.send(&query(id)).await.unwrap();
    }
    let mut refused = 0;
    while let Ok(Ok(_)) =
        tokio::time::timeout(Duration::from_millis(500), client.recv(&mut buf)).await
    {
        refused += usize::from(buf[3] & 0x0f == 5);
    }
    // Of the hundred past the burst, the rate refills some while they are sent, and a datagram
    // can be lost: at least half are refused.
    assert!(refused >= 50, "past the burst, refused: {refused}");
}

#[test]
fn a_resolver_is_two_sockets() {
    assert!(Bound::from_attached(Vec::new()).is_err());
}

/// Answers one TCP query on `s` with the query, marked as an answer.
async fn echo_once(mut s: TcpStream) {
    if let Some(mut q) = read(&mut s).await {
        q[2] |= 0x80;
        let _ = write(&mut s, &q).await;
    }
}

#[tokio::test]
async fn a_query_over_tcp_is_forwarded_over_tcp() {
    let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = upstream.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((s, _)) = upstream.accept().await {
            tokio::spawn(echo_once(s));
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let (r, bound, _, at) = resolver(dir.path(), port);
    tokio::spawn(r.serve(bound));
    let mut s = TcpStream::connect(at).await.unwrap();
    let answer = tokio::time::timeout(Duration::from_secs(2), exchange(&mut s, &query(9)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((&answer[..2], answer[2] & 0x80), (&9u16.to_be_bytes()[..], 0x80));
}
