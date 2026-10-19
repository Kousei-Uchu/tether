use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tether_core::{
    channel::{Role, SecureChannel},
    identity::Identity,
    pairing::PairingOffer,
    session::{accept, connect, ClientMode, HostConfig, Outcome, SessionError},
    trust::TrustStore,
};
use tether_proto::Message;
use tokio::io::duplex;

struct Device {
    identity: Arc<Identity>,
    trust: Arc<Mutex<TrustStore>>,
    _dir: tempfile::TempDir,
}

fn device() -> Device {
    let dir = tempfile::tempdir().unwrap();
    let trust = TrustStore::load(dir.path().join("trusted.json")).unwrap();
    Device {
        identity: Arc::new(Identity::generate().unwrap()),
        trust: Arc::new(Mutex::new(trust)),
        _dir: dir,
    }
}

fn host_config(d: &Device, offer: Option<PairingOffer>) -> HostConfig {
    HostConfig {
        identity: d.identity.clone(),
        trust: d.trust.clone(),
        pairing: Arc::new(Mutex::new(offer)),
        device_name: "host".into(),
    }
}

/// Runs one host accept and one client connect against each other.
#[allow(clippy::await_holding_lock)]
async fn run(
    host: &HostConfig,
    client: &Device,
    mode: ClientMode,
) -> (
    Result<Outcome<tokio::io::DuplexStream>, SessionError>,
    Result<Outcome<tokio::io::DuplexStream>, SessionError>,
) {
    let (a, b) = duplex(1 << 16);
    let mut client_trust = client.trust.lock().unwrap();
    // The client trust store is only touched from this one future, so holding the lock
    // across the join is fine for a test.
    let client_fut = connect(b, &client.identity, &mut client_trust, "client", mode);
    tokio::join!(accept(a, host), client_fut)
}

#[tokio::test]
async fn pair_then_connect_then_ping() {
    let host_dev = device();
    let client_dev = device();
    let offer = PairingOffer::new(Duration::from_secs(60));
    let code = offer.display_code();
    let host = host_config(&host_dev, Some(offer));

    let (h, c) = run(&host, &client_dev, ClientMode::Pair { code }).await;
    assert!(matches!(h.unwrap(), Outcome::Paired(_)));
    assert!(matches!(c.unwrap(), Outcome::Paired(_)));
    assert!(host_dev
        .trust
        .lock()
        .unwrap()
        .contains(&client_dev.identity.public));
    assert!(client_dev
        .trust
        .lock()
        .unwrap()
        .contains(&host_dev.identity.public));
    assert!(host.pairing.lock().unwrap().is_none(), "code is single use");

    let (h, c) = run(&host, &client_dev, ClientMode::Connect).await;
    let (Outcome::Authenticated(host_session), Outcome::Authenticated(mut client_session)) =
        (h.unwrap(), c.unwrap())
    else {
        panic!("expected authenticated sessions");
    };
    let serve = tokio::spawn(host_session.serve());
    let rtt = client_session.ping(42).await.unwrap();
    assert!(rtt < Duration::from_secs(1));
    client_session.bye("done").await.unwrap();
    serve.await.unwrap().unwrap();
}

#[tokio::test]
async fn wrong_code_is_rejected_and_nothing_is_stored() {
    let host_dev = device();
    let client_dev = device();
    let offer = PairingOffer::new(Duration::from_secs(60));
    let host = host_config(&host_dev, Some(offer));

    let (h, c) = run(
        &host,
        &client_dev,
        ClientMode::Pair {
            code: "0000-0000-0000-0000".into(),
        },
    )
    .await;
    assert!(matches!(h, Err(SessionError::PairingFailed(_))));
    assert!(matches!(c, Err(SessionError::PairingFailed(_))));
    assert!(host_dev.trust.lock().unwrap().peers().is_empty());
    assert!(client_dev.trust.lock().unwrap().peers().is_empty());
    let left = host
        .pairing
        .lock()
        .unwrap()
        .as_ref()
        .map(|o| o.attempts_left);
    assert_eq!(
        left,
        Some(4),
        "a bad guess costs an attempt but keeps the offer"
    );
}

#[tokio::test]
async fn offer_dies_after_too_many_bad_guesses() {
    let host_dev = device();
    let client_dev = device();
    let offer = PairingOffer::new(Duration::from_secs(60));
    let real = offer.display_code();
    let host = host_config(&host_dev, Some(offer));

    for _ in 0..5 {
        let _ = run(
            &host,
            &client_dev,
            ClientMode::Pair {
                code: "0000-0000-0000-0000".into(),
            },
        )
        .await;
    }
    assert!(host.pairing.lock().unwrap().is_none());
    let (h, _) = run(&host, &client_dev, ClientMode::Pair { code: real }).await;
    assert!(matches!(h, Err(SessionError::PairingFailed(_))));
}

#[tokio::test]
async fn pairing_disabled_or_expired_is_rejected() {
    let host_dev = device();
    let client_dev = device();
    let host = host_config(&host_dev, None);
    let (h, c) = run(
        &host,
        &client_dev,
        ClientMode::Pair {
            code: "0000-0000-0000-0000".into(),
        },
    )
    .await;
    assert!(h.is_err() && c.is_err());

    let offer = PairingOffer::new(Duration::from_millis(0));
    let code = offer.display_code();
    let host = host_config(&host_dev, Some(offer));
    let (h, c) = run(&host, &client_dev, ClientMode::Pair { code }).await;
    assert!(h.is_err() && c.is_err());
    assert!(host_dev.trust.lock().unwrap().peers().is_empty());
}

#[tokio::test]
async fn unpaired_connect_is_refused_on_both_sides() {
    let host_dev = device();
    let client_dev = device();
    let host = host_config(&host_dev, None);
    let (h, c) = run(&host, &client_dev, ClientMode::Connect).await;
    // The client notices the host is unknown and hangs up before the host decides.
    assert!(matches!(c, Err(SessionError::HostNotTrusted { .. })));
    assert!(h.is_err());
}

#[tokio::test]
async fn paired_host_refuses_unknown_client() {
    let host_dev = device();
    let stranger = device();
    // The stranger trusts the host (e.g. scraped its key) but the host never paired with it.
    stranger
        .trust
        .lock()
        .unwrap()
        .add(tether_core::trust::TrustedPeer::new(
            "host".into(),
            &host_dev.identity.public,
            tether_proto::Platform::Linux,
        ))
        .unwrap();
    let host = host_config(&host_dev, None);
    let (h, c) = run(&host, &stranger, ClientMode::Connect).await;
    assert!(matches!(h, Err(SessionError::PeerNotTrusted { .. })));
    assert!(matches!(c, Err(SessionError::Rejected(_))));
}

#[tokio::test]
async fn large_messages_are_chunked_and_reassembled() {
    let a = Identity::generate().unwrap();
    let b = Identity::generate().unwrap();
    let (x, y) = duplex(1 << 16);
    let (mut ca, mut cb) = tokio::try_join!(
        SecureChannel::handshake(x, &a, Role::Initiator),
        SecureChannel::handshake(y, &b, Role::Responder)
    )
    .unwrap();
    assert_eq!(ca.remote_static(), &b.public);
    assert_eq!(cb.remote_static(), &a.public);
    assert_eq!(ca.handshake_hash(), cb.handshake_hash());

    let big = Message::Bye {
        reason: "x".repeat(300_000),
    };
    let sender = async {
        ca.send(&big).await.unwrap();
        ca.send(&Message::Ready).await.unwrap();
    };
    let receiver = async {
        assert_eq!(cb.recv().await.unwrap(), big);
        assert_eq!(cb.recv().await.unwrap(), Message::Ready);
    };
    tokio::join!(sender, receiver);
}
