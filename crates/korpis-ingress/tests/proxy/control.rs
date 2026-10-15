// SPDX-License-Identifier: AGPL-3.0-or-later

//! The agent's connection: the sockets arrive attached to the first message, and every
//! configuration after it replaces the last unless it is refused, when the last stays.

use buffa::Message as _;
use korpis_ingress::{Table, control};
use korpis_proto::korpis::ingress::v1 as pb;
use std::io::Write as _;
use std::os::fd::{AsFd as _, OwnedFd};
use std::os::unix::net::UnixListener;
use std::sync::Arc;
use tokio::sync::watch;

fn framed(body: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(body.len()).unwrap().to_be_bytes().to_vec();
    out.extend_from_slice(body);
    out
}

#[test]
fn the_sockets_arrive_with_the_first_message_in_the_order_it_says() {
    use rustix::net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags, sendmsg};
    let dir = std::env::temp_dir().join(format!("korpis-ingress-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("control.sock");
    let _ = std::fs::remove_file(&path);
    let agent = UnixListener::bind(&path).unwrap();
    let sockets: Vec<std::net::TcpListener> =
        (0..3).map(|_| std::net::TcpListener::bind("127.0.0.1:0").unwrap()).collect();
    let ports: Vec<u16> = sockets.iter().map(|s| s.local_addr().unwrap().port()).collect();
    let feeding = std::thread::spawn(move || {
        let (s, _) = agent.accept().unwrap();
        let first =
            framed(&pb::Listeners { http: 1, https: 2, ..Default::default() }.encode_to_vec());
        let fds: Vec<OwnedFd> = sockets.into_iter().map(OwnedFd::from).collect();
        let borrowed: Vec<_> = fds.iter().map(|f| f.as_fd()).collect();
        let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(3))];
        let mut cmsg = SendAncillaryBuffer::new(&mut space);
        assert!(cmsg.push(SendAncillaryMessage::ScmRights(&borrowed)));
        let sent = sendmsg(&s, &[std::io::IoSlice::new(&first)], &mut cmsg, SendFlags::empty());
        let mut s = s;
        s.write_all(&first[sent.unwrap()..]).unwrap();
        s
    });
    let (_stream, listeners) = control::connect(&path).unwrap();
    let _agent_side = feeding.join().unwrap();
    let port = |l: &std::net::TcpListener| l.local_addr().unwrap().port();
    assert_eq!(listeners.http.iter().map(port).collect::<Vec<_>>(), ports[..1]);
    assert_eq!(listeners.https.iter().map(port).collect::<Vec<_>>(), ports[1..]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_refused_configuration_leaves_the_last_one_served() {
    let route = |backend: &str| pb::Route {
        names: vec!["shop.example.test".into()],
        backend: backend.into(),
        mode: pb::RouteMode::ROUTE_MODE_HTTP.into(),
        ..Default::default()
    };
    let config =
        |backend: &str| pb::IngressConfig { routes: vec![route(backend)], ..Default::default() };
    let mut wire = framed(&config("10.0.0.5:80").encode_to_vec());
    wire.extend(framed(&config("not an address").encode_to_vec()));
    let (publish, table) = watch::channel(Arc::new(Table::default()));
    control::configs(wire.as_slice(), &publish).await.unwrap();
    let served = table.borrow().route("shop.example.test").map(|r| r.backend.to_string());
    assert_eq!(served.as_deref(), Some("10.0.0.5:80"), "the refused one changed nothing");
    let oversized = (u32::try_from(control::MAX).unwrap() + 1).to_be_bytes();
    assert!(control::configs(oversized.as_slice(), &publish).await.is_err());
}
