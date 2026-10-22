use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tether_core::{
    channel::{Role, SecureChannel},
    identity::Identity,
    media::{MediaAnswer, MediaFuture, MediaProvider},
    pairing::PairingOffer,
    permissions::PermissionSet,
    session::{accept, connect, ClientMode, HostConfig, Outcome, PeerInfo, SessionError},
    trust::TrustStore,
};
use tether_proto::{Message, Permission};
use tokio::io::duplex;

struct Device {
    identity: Arc<Identity>,
    trust: Arc<Mutex<TrustStore>>,
    dir: tempfile::TempDir,
}

fn device() -> Device {
    let dir = tempfile::tempdir().unwrap();
    let trust = TrustStore::load(dir.path().join("trusted.json")).unwrap();
    Device {
        identity: Arc::new(Identity::generate().unwrap()),
        trust: Arc::new(Mutex::new(trust)),
        dir,
    }
}

fn host_config(d: &Device, offer: Option<PairingOffer>) -> HostConfig {
    HostConfig {
        identity: d.identity.clone(),
        trust: d.trust.clone(),
        pairing: Arc::new(Mutex::new(offer)),
        device_name: "host".into(),
        media: None,
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
    let host = Arc::new(host_config(&host_dev, Some(offer)));

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
    let serve_host = host.clone();
    let serve = tokio::spawn(async move { host_session.serve(&serve_host).await });
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
            PermissionSet::none(),
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

/// Pairs `client` with `host` using an offer that grants `grant` (or the default).
async fn pair_with_grant(
    host_dev: &Device,
    client: &Device,
    grant: Option<PermissionSet>,
) -> Arc<HostConfig> {
    let mut offer = PairingOffer::new(Duration::from_secs(60));
    if let Some(g) = grant {
        offer = offer.with_grant(g);
    }
    let code = offer.display_code();
    let host = Arc::new(host_config(host_dev, Some(offer)));
    let (h, c) = run(&host, client, ClientMode::Pair { code }).await;
    assert!(matches!(h.unwrap(), Outcome::Paired(_)));
    assert!(matches!(c.unwrap(), Outcome::Paired(_)));
    host
}

async fn open_session(
    host: &HostConfig,
    client: &Device,
) -> (
    tether_core::session::Session<tokio::io::DuplexStream>,
    tether_core::session::Session<tokio::io::DuplexStream>,
) {
    let (h, c) = run(host, client, ClientMode::Connect).await;
    match (h.unwrap(), c.unwrap()) {
        (Outcome::Authenticated(h), Outcome::Authenticated(c)) => (h, c),
        _ => panic!("expected authenticated sessions"),
    }
}

fn external_store(d: &Device) -> TrustStore {
    TrustStore::load(d.dir.path().join("trusted.json")).unwrap()
}

#[tokio::test]
async fn pairing_grant_reaches_both_sides() {
    let host_dev = device();
    let client_dev = device();
    let grant = PermissionSet::none()
        .with(Permission::View)
        .with(Permission::Input);
    let host = pair_with_grant(&host_dev, &client_dev, Some(grant)).await;

    let stored = host_dev
        .trust
        .lock()
        .unwrap()
        .get(&client_dev.identity.public)
        .unwrap()
        .permissions;
    assert_eq!(stored, grant);

    let (host_session, client_session) = open_session(&host, &client_dev).await;
    assert_eq!(host_session.permissions, grant);
    assert_eq!(client_session.permissions, grant);
}

#[tokio::test]
async fn default_grant_is_view_only() {
    let host_dev = device();
    let client_dev = device();
    let host = pair_with_grant(&host_dev, &client_dev, None).await;
    let (host_session, client_session) = open_session(&host, &client_dev).await;
    assert_eq!(host_session.permissions, PermissionSet::default_grant());
    assert_eq!(client_session.permissions.to_vec(), vec![Permission::View]);
}

#[tokio::test]
async fn grant_changes_and_revocation_reach_a_live_session() {
    let host_dev = device();
    let client_dev = device();
    let host = pair_with_grant(&host_dev, &client_dev, None).await;
    let (host_session, mut client_session) = open_session(&host, &client_dev).await;

    let serve_host = host.clone();
    let serve = tokio::spawn(async move { host_session.serve(&serve_host).await });

    client_session.ping(1).await.unwrap();
    assert!(!client_session.permissions.contains(Permission::Input));

    // Another process (the CLI) widens the grant while the session is open.
    external_store(&host_dev)
        .change_permissions("client", |p| {
            p.with(Permission::Input).with(Permission::Power)
        })
        .unwrap();
    client_session.ping(2).await.unwrap();
    assert!(client_session.permissions.contains(Permission::Input));
    assert!(client_session.permissions.contains(Permission::Power));

    // ...and then narrows it again.
    external_store(&host_dev)
        .change_permissions("client", |p| p.without(Permission::Power))
        .unwrap();
    client_session.ping(3).await.unwrap();
    assert!(!client_session.permissions.contains(Permission::Power));

    // Unpairing ends the session.
    assert_eq!(external_store(&host_dev).remove("client").unwrap(), 1);
    let err = client_session.ping(4).await.unwrap_err();
    assert!(matches!(err, SessionError::Rejected(r) if r == "access revoked"));
    assert!(matches!(serve.await.unwrap(), Err(SessionError::Revoked)));
}

#[tokio::test]
async fn authorize_is_the_gate_for_actions() {
    let host_dev = device();
    let client_dev = device();
    let host = pair_with_grant(&host_dev, &client_dev, None).await;
    let (mut host_session, _client_session) = open_session(&host, &client_dev).await;

    host_session
        .authorize(&host, Permission::View)
        .await
        .unwrap();
    let err = host_session
        .authorize(&host, Permission::Input)
        .await
        .unwrap_err();
    assert!(matches!(err, SessionError::PermissionDenied(d) if d.0 == Permission::Input));

    external_store(&host_dev)
        .change_permissions("client", |p| p.with(Permission::Input))
        .unwrap();
    host_session
        .authorize(&host, Permission::Input)
        .await
        .unwrap();
}

// ----- video signaling -----

/// Stands in for the real WebRTC host: counts starts and stops.
struct FakeMedia {
    started: Arc<AtomicUsize>,
    stopped: Arc<AtomicUsize>,
}

struct StopFlag(Arc<AtomicUsize>);

impl Drop for StopFlag {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl MediaProvider for FakeMedia {
    fn answer<'a>(&'a self, _peer: &'a PeerInfo, offer: String) -> MediaFuture<'a> {
        Box::pin(async move {
            self.started.fetch_add(1, Ordering::SeqCst);
            Ok(MediaAnswer {
                sdp: format!("answer-for:{offer}"),
                guard: Box::new(StopFlag(self.stopped.clone())),
            })
        })
    }
}

fn with_media(host: &HostConfig) -> (Arc<HostConfig>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let started = Arc::new(AtomicUsize::new(0));
    let stopped = Arc::new(AtomicUsize::new(0));
    let cfg = HostConfig {
        identity: host.identity.clone(),
        trust: host.trust.clone(),
        pairing: host.pairing.clone(),
        device_name: host.device_name.clone(),
        media: Some(Arc::new(FakeMedia {
            started: started.clone(),
            stopped: stopped.clone(),
        })),
    };
    (Arc::new(cfg), started, stopped)
}

async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    for _ in 0..60 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn video_needs_a_provider_and_the_view_permission() {
    let host_dev = device();
    let client_dev = device();

    // A host that shares nothing says so.
    let plain = pair_with_grant(&host_dev, &client_dev, None).await;
    let (host_session, mut client_session) = open_session(&plain, &client_dev).await;
    let h = plain.clone();
    let serve = tokio::spawn(async move { host_session.serve(&h).await });
    let err = client_session
        .request_video("offer".into())
        .await
        .unwrap_err();
    assert!(matches!(err, SessionError::Signal(m) if m.contains("not sharing")));
    client_session.bye("done").await.unwrap();
    serve.await.unwrap().unwrap();

    // With a provider, a device that holds only `input` is refused.
    let (shared, started, _stopped) = with_media(&plain);
    external_store(&host_dev)
        .change_permissions("client", |p| {
            p.without(Permission::View).with(Permission::Input)
        })
        .unwrap();
    let (host_session, mut client_session) = open_session(&shared, &client_dev).await;
    let h = shared.clone();
    let serve = tokio::spawn(async move { host_session.serve(&h).await });
    let err = client_session
        .request_video("offer".into())
        .await
        .unwrap_err();
    assert!(matches!(err, SessionError::Signal(m) if m.contains("view")));
    assert_eq!(started.load(Ordering::SeqCst), 0);

    // Granting view makes the same session work, with no reconnect.
    external_store(&host_dev)
        .change_permissions("client", |p| p.with(Permission::View))
        .unwrap();
    let answer = client_session
        .request_video("offer-2".into())
        .await
        .unwrap();
    assert_eq!(answer, "answer-for:offer-2");
    assert_eq!(started.load(Ordering::SeqCst), 1);
    client_session.bye("done").await.unwrap();
    serve.await.unwrap().unwrap();
}

#[tokio::test]
async fn video_stops_when_the_session_ends_or_a_new_offer_arrives() {
    let host_dev = device();
    let client_dev = device();
    let plain = pair_with_grant(&host_dev, &client_dev, None).await;
    let (shared, started, stopped) = with_media(&plain);
    let (host_session, mut client_session) = open_session(&shared, &client_dev).await;
    let h = shared.clone();
    let serve = tokio::spawn(async move { host_session.serve(&h).await });

    client_session.request_video("one".into()).await.unwrap();
    assert_eq!(
        (
            started.load(Ordering::SeqCst),
            stopped.load(Ordering::SeqCst)
        ),
        (1, 0)
    );
    client_session.request_video("two".into()).await.unwrap();
    assert_eq!(
        (
            started.load(Ordering::SeqCst),
            stopped.load(Ordering::SeqCst)
        ),
        (2, 1)
    );

    client_session.bye("done").await.unwrap();
    serve.await.unwrap().unwrap();
    assert_eq!(stopped.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn revoking_view_stops_video_even_if_the_client_stays_silent() {
    let host_dev = device();
    let client_dev = device();
    let plain = pair_with_grant(&host_dev, &client_dev, None).await;
    let (shared, _started, stopped) = with_media(&plain);
    let (host_session, mut client_session) = open_session(&shared, &client_dev).await;
    let h = shared.clone();
    let serve = tokio::spawn(async move { host_session.serve(&h).await });

    client_session.request_video("offer".into()).await.unwrap();
    assert_eq!(stopped.load(Ordering::SeqCst), 0);

    // The client sends nothing from here on. The host's own timer must notice.
    external_store(&host_dev)
        .change_permissions("client", |p| p.without(Permission::View))
        .unwrap();
    wait_until("video to stop", || stopped.load(Ordering::SeqCst) == 1).await;

    // The client is told, and a fresh request is refused.
    client_session.ping(1).await.unwrap();
    assert!(!client_session.permissions.contains(Permission::View));
    let err = client_session
        .request_video("again".into())
        .await
        .unwrap_err();
    assert!(matches!(err, SessionError::Signal(m) if m.contains("view")));

    // Unpairing while idle also ends the session by itself.
    external_store(&host_dev).remove("client").unwrap();
    assert!(matches!(serve.await.unwrap(), Err(SessionError::Revoked)));
}
