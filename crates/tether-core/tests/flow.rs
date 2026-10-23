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
    input::{InputProvider, InputSink},
    media::{MediaAnswer, MediaFuture, MediaProvider},
    pairing::PairingOffer,
    permissions::PermissionSet,
    session::{accept, connect, ClientMode, HostConfig, Outcome, PeerInfo, SessionError},
    trust::TrustStore,
};
use tether_proto::{InputEvent, Key, Message, MouseButton, Permission};
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
        input: None,
        wake: None,
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
        input: None,
        wake: None,
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

// ----- mouse and keyboard -----

#[derive(Clone, Default)]
struct FakeInput {
    applied: Arc<Mutex<Vec<InputEvent>>>,
    released: Arc<AtomicUsize>,
    fail_open: Option<String>,
}

struct FakeSink(FakeInput);

impl InputSink for FakeSink {
    fn apply(&mut self, event: InputEvent) -> Result<(), String> {
        self.0.applied.lock().unwrap().push(event);
        Ok(())
    }
}

impl Drop for FakeSink {
    fn drop(&mut self) {
        self.0.released.fetch_add(1, Ordering::SeqCst);
    }
}

impl InputProvider for FakeInput {
    fn open(&self, _peer: &PeerInfo) -> Result<Box<dyn InputSink>, String> {
        match &self.fail_open {
            Some(reason) => Err(reason.clone()),
            None => Ok(Box::new(FakeSink(self.clone()))),
        }
    }
}

fn with_input(host: &HostConfig, fake: Option<FakeInput>) -> Arc<HostConfig> {
    Arc::new(HostConfig {
        identity: host.identity.clone(),
        trust: host.trust.clone(),
        pairing: host.pairing.clone(),
        device_name: host.device_name.clone(),
        media: None,
        input: fake.map(|f| Arc::new(f) as Arc<dyn InputProvider>),
        wake: None,
    })
}

fn key(k: Key, down: bool) -> InputEvent {
    InputEvent::Key { key: k, down }
}

#[tokio::test]
async fn input_is_gated_by_permission_and_by_the_host_switch() {
    let host_dev = device();
    let client_dev = device();
    let plain = pair_with_grant(&host_dev, &client_dev, None).await; // view only
    let fake = FakeInput::default();
    let host = with_input(&plain, Some(fake.clone()));
    let (host_session, mut client) = open_session(&host, &client_dev).await;
    let h = host.clone();
    let serve = tokio::spawn(async move { host_session.serve(&h).await });

    // Without `input` the events are dropped, and the person is told exactly once.
    for _ in 0..5 {
        client.send_input(key(Key::A, true)).await.unwrap();
    }
    client.ping(1).await.unwrap();
    let notices = client.take_notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("input"));
    assert!(fake.applied.lock().unwrap().is_empty());

    // Granting `input` takes effect on the open session.
    external_store(&host_dev)
        .change_permissions("client", |p| p.with(Permission::Input))
        .unwrap();
    client.ping(2).await.unwrap();
    client
        .send_input(InputEvent::MouseMove { x: 100, y: 200 })
        .await
        .unwrap();
    client.send_input(key(Key::A, true)).await.unwrap();
    client.send_input(key(Key::A, false)).await.unwrap();
    client.ping(3).await.unwrap();
    assert_eq!(
        *fake.applied.lock().unwrap(),
        vec![
            InputEvent::MouseMove { x: 100, y: 200 },
            key(Key::A, true),
            key(Key::A, false)
        ]
    );

    // Media keys need `media`, not `input`.
    client.send_input(key(Key::VolumeUp, true)).await.unwrap();
    client.ping(4).await.unwrap();
    assert_eq!(fake.applied.lock().unwrap().len(), 3);
    external_store(&host_dev)
        .change_permissions("client", |p| p.with(Permission::Media))
        .unwrap();
    client.ping(5).await.unwrap();
    client.send_input(key(Key::VolumeUp, true)).await.unwrap();
    client.ping(6).await.unwrap();
    assert_eq!(
        fake.applied.lock().unwrap().last(),
        Some(&key(Key::VolumeUp, true))
    );

    client.bye("done").await.unwrap();
    serve.await.unwrap().unwrap();
    assert_eq!(
        fake.released.load(Ordering::SeqCst),
        1,
        "injector released on session end"
    );
}

#[tokio::test]
async fn a_host_with_remote_control_off_says_so() {
    let host_dev = device();
    let client_dev = device();
    let plain = pair_with_grant(&host_dev, &client_dev, Some(PermissionSet::all())).await;
    let host = with_input(&plain, None);
    let (host_session, mut client) = open_session(&host, &client_dev).await;
    let h = host.clone();
    let serve = tokio::spawn(async move { host_session.serve(&h).await });

    client.send_input(key(Key::A, true)).await.unwrap();
    client.ping(1).await.unwrap();
    let notices = client.take_notices();
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("turned off"));
    client.bye("done").await.unwrap();
    serve.await.unwrap().unwrap();
}

#[tokio::test]
async fn an_injector_that_cannot_open_explains_why() {
    let host_dev = device();
    let client_dev = device();
    let plain = pair_with_grant(&host_dev, &client_dev, Some(PermissionSet::all())).await;
    let fake = FakeInput {
        fail_open: Some("Allow Tether under Accessibility, then try again.".into()),
        ..Default::default()
    };
    let host = with_input(&plain, Some(fake.clone()));
    let (host_session, mut client) = open_session(&host, &client_dev).await;
    let h = host.clone();
    let serve = tokio::spawn(async move { host_session.serve(&h).await });

    client
        .send_input(InputEvent::MouseMove { x: 1, y: 1 })
        .await
        .unwrap();
    client
        .send_input(InputEvent::MouseMove { x: 2, y: 2 })
        .await
        .unwrap();
    client.ping(1).await.unwrap();
    assert_eq!(
        client.take_notices(),
        vec!["Allow Tether under Accessibility, then try again.".to_string()]
    );
    assert!(fake.applied.lock().unwrap().is_empty());
    client.bye("done").await.unwrap();
    serve.await.unwrap().unwrap();
}

#[tokio::test]
async fn floods_of_moves_are_dropped_but_presses_never_are() {
    let host_dev = device();
    let client_dev = device();
    let plain = pair_with_grant(&host_dev, &client_dev, Some(PermissionSet::all())).await;
    let fake = FakeInput::default();
    let host = with_input(&plain, Some(fake.clone()));
    let (host_session, mut client) = open_session(&host, &client_dev).await;
    let h = host.clone();
    let serve = tokio::spawn(async move { host_session.serve(&h).await });

    for i in 0..4000u16 {
        client
            .send_input(InputEvent::MouseMove { x: i, y: i })
            .await
            .unwrap();
    }
    client.send_input(key(Key::Enter, true)).await.unwrap();
    client
        .send_input(InputEvent::MouseButton {
            button: MouseButton::Left,
            down: false,
        })
        .await
        .unwrap();
    client.ping(1).await.unwrap();

    let applied = fake.applied.lock().unwrap().clone();
    let moves = applied
        .iter()
        .filter(|e| matches!(e, InputEvent::MouseMove { .. }))
        .count();
    assert!(moves <= 1500 * 2, "{moves} moves got through"); // at most two one second windows
    assert!(moves < 4000, "nothing was dropped");
    assert!(applied.contains(&key(Key::Enter, true)));
    assert!(applied.contains(&InputEvent::MouseButton {
        button: MouseButton::Left,
        down: false
    }));
    client.bye("done").await.unwrap();
    serve.await.unwrap().unwrap();
}

#[tokio::test]
async fn revoking_input_releases_the_injector_even_if_the_client_is_silent() {
    let host_dev = device();
    let client_dev = device();
    let plain = pair_with_grant(&host_dev, &client_dev, Some(PermissionSet::all())).await;
    let fake = FakeInput::default();
    let host = with_input(&plain, Some(fake.clone()));
    let (host_session, mut client) = open_session(&host, &client_dev).await;
    let h = host.clone();
    let serve = tokio::spawn(async move { host_session.serve(&h).await });

    client.send_input(key(Key::ShiftLeft, true)).await.unwrap(); // held down
    client.ping(1).await.unwrap();
    assert_eq!(fake.released.load(Ordering::SeqCst), 0);

    external_store(&host_dev)
        .change_permissions("client", |p| {
            p.without(Permission::Input).without(Permission::Media)
        })
        .unwrap();
    wait_until("the injector to be released", || {
        fake.released.load(Ordering::SeqCst) == 1
    })
    .await;
    client.ping(2).await.unwrap();
    client.bye("done").await.unwrap();
    serve.await.unwrap().unwrap();
}

struct FakeWake {
    asked: Mutex<Vec<String>>,
}

impl tether_core::wake::WakeProvider for FakeWake {
    fn wake(&self, target: &str) -> Result<String, String> {
        self.asked.lock().unwrap().push(target.to_owned());
        if target == "desk" {
            Ok("Sent a wake signal to desk.".into())
        } else {
            Err(format!("no computer called {target}"))
        }
    }
}

async fn wake_answer(cfg: &Arc<HostConfig>, client_dev: &Device, targets: &[&str]) -> Vec<String> {
    let (host_session, mut client_session) = open_session(cfg, client_dev).await;
    let h = cfg.clone();
    let serve = tokio::spawn(async move { host_session.serve(&h).await });
    let mut notices = Vec::new();
    for t in targets {
        client_session.wake(t).await.unwrap();
        client_session.ping(1).await.unwrap();
        notices.extend(client_session.take_notices());
    }
    client_session.bye("done").await.unwrap();
    serve.await.unwrap().unwrap();
    notices
}

#[tokio::test]
async fn waking_needs_a_relay_and_the_power_permission() {
    let host_dev = device();
    let client_dev = device();
    let plain = pair_with_grant(&host_dev, &client_dev, None).await;

    // View only: refused, and the provider is never asked.
    let fake = Arc::new(FakeWake {
        asked: Mutex::new(Vec::new()),
    });
    let relay = Arc::new(HostConfig {
        identity: plain.identity.clone(),
        trust: plain.trust.clone(),
        pairing: plain.pairing.clone(),
        device_name: plain.device_name.clone(),
        media: None,
        input: None,
        wake: Some(fake.clone()),
    });
    let n = wake_answer(&relay, &client_dev, &["desk"]).await;
    assert!(n[0].contains("`power`"), "{n:?}");
    assert!(fake.asked.lock().unwrap().is_empty());

    // With power but no relay on this host.
    external_store(&host_dev)
        .change_permissions("client", |p| p.with(Permission::Power))
        .unwrap();
    let n = wake_answer(&plain, &client_dev, &["desk"]).await;
    assert!(n[0].contains("not set up"), "{n:?}");

    // With power and a relay: success, the cooldown, then (after it) an unknown name.
    let n = wake_answer(&relay, &client_dev, &["desk", "desk"]).await;
    assert_eq!(n[0], "Sent a wake signal to desk.");
    assert!(n[1].contains("Wait a moment"), "{n:?}");
    assert_eq!(fake.asked.lock().unwrap().as_slice(), ["desk"]);
    tokio::time::sleep(Duration::from_millis(3100)).await;
    let n = wake_answer(&relay, &client_dev, &["laptop"]).await;
    assert!(n[0].contains("no computer called laptop"), "{n:?}");
}
