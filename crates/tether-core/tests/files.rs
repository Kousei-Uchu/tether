//! File transfer between a client session and a host that shares a folder.

use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tether_core::{
    files::FileShare,
    identity::Identity,
    pairing::PairingOffer,
    session::{accept, connect, ClientMode, HostConfig, Outcome, Session, SessionError},
    trust::TrustStore,
};
use tether_proto::Permission;
use tokio::{
    io::{duplex, DuplexStream},
    task::JoinHandle,
};

struct Dev {
    identity: Arc<Identity>,
    trust: Arc<Mutex<TrustStore>>,
    _dir: tempfile::TempDir,
}

fn dev() -> Dev {
    let dir = tempfile::tempdir().unwrap();
    Dev {
        identity: Arc::new(Identity::generate().unwrap()),
        trust: Arc::new(Mutex::new(
            TrustStore::load(dir.path().join("trusted.json")).unwrap(),
        )),
        _dir: dir,
    }
}

/// Pairs a fresh client with a fresh host, connects, and leaves the host serving.
#[allow(clippy::await_holding_lock)]
async fn setup(
    grant: Vec<Permission>,
    share: Option<&Path>,
) -> (Session<DuplexStream>, JoinHandle<()>, Dev) {
    let (host, client) = (dev(), dev());
    let offer = PairingOffer::new(Duration::from_secs(60)).with_grant(grant.into());
    let code = offer.display_code();
    let cfg = Arc::new(HostConfig {
        identity: host.identity.clone(),
        trust: host.trust.clone(),
        pairing: Arc::new(Mutex::new(Some(offer))),
        device_name: "host".into(),
        media: None,
        input: None,
        wake: None,
        clipboard: None,
        files: share.map(|p| Arc::new(FileShare::new(p).unwrap())),
    });
    {
        let (a, b) = duplex(1 << 16);
        let mut trust = client.trust.lock().unwrap();
        let (h, c) = tokio::join!(
            accept(a, &cfg),
            connect(b, &client.identity, &mut trust, "client", ClientMode::Pair { code })
        );
        assert!(matches!(h, Ok(Outcome::Paired(_))));
        assert!(matches!(c, Ok(Outcome::Paired(_))));
    }
    let (a, b) = duplex(1 << 16);
    let (h, c) = {
        let mut trust = client.trust.lock().unwrap();
        tokio::join!(
            accept(a, &cfg),
            connect(b, &client.identity, &mut trust, "client", ClientMode::Connect)
        )
    };
    let (Ok(Outcome::Authenticated(host_session)), Ok(Outcome::Authenticated(client_session))) =
        (h, c)
    else {
        panic!("connect failed");
    };
    let serving = tokio::spawn(async move {
        let _ = host_session.serve(&cfg).await;
    });
    (client_session, serving, client)
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

fn no_progress() -> impl FnMut(u64, u64) -> bool {
    |_, _| true
}

#[tokio::test]
async fn a_file_goes_up_and_comes_back_intact() {
    let shared = tempfile::tempdir().unwrap();
    let (mut s, _serving, _client) = setup(vec![Permission::Files], Some(shared.path())).await;
    let local = tempfile::tempdir().unwrap();
    let data = pattern(200_000); // several pieces, not a multiple of the piece size
    fs::write(local.path().join("up.bin"), &data).unwrap();

    let mut seen = Vec::new();
    let name = s
        .upload_file(1, &local.path().join("up.bin"), "up.bin", &mut |d, t| {
            seen.push((d, t));
            true
        })
        .await
        .unwrap();
    assert_eq!(name, "up.bin");
    assert_eq!(seen.last(), Some(&(200_000, 200_000)));
    assert!(seen.len() > 3, "progress is reported as it goes");
    assert_eq!(fs::read(shared.path().join("up.bin")).unwrap(), data);

    let listed = s.list_files("").await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!((listed[0].name.as_str(), listed[0].size, listed[0].dir), ("up.bin", 200_000, false));

    let dest = local.path().join("down.bin");
    let size = s.download_file(2, "up.bin", &dest, &mut no_progress()).await.unwrap();
    assert_eq!(size, 200_000);
    assert_eq!(fs::read(&dest).unwrap(), data);

    // A second upload with the same name is kept alongside, not over it.
    let again = s
        .upload_file(3, &local.path().join("up.bin"), "up.bin", &mut no_progress())
        .await
        .unwrap();
    assert_eq!(again, "up (1).bin");

    // Empty files work too.
    fs::write(local.path().join("empty"), b"").unwrap();
    s.upload_file(4, &local.path().join("empty"), "empty", &mut no_progress())
        .await
        .unwrap();
    let dest = local.path().join("empty.down");
    assert_eq!(s.download_file(5, "empty", &dest, &mut no_progress()).await.unwrap(), 0);
    assert!(s.ping(1).await.is_ok());
}

#[tokio::test]
async fn nothing_outside_the_shared_folder_can_be_reached() {
    let root = tempfile::tempdir().unwrap();
    let shared = root.path().join("shared");
    fs::create_dir(&shared).unwrap();
    fs::write(root.path().join("secret.txt"), b"secret").unwrap();
    let (mut s, _serving, _client) = setup(vec![Permission::Files], Some(&shared)).await;
    let local = tempfile::tempdir().unwrap();

    for bad in ["../secret.txt", "..\\secret.txt", "a/../../secret.txt", "C:secret.txt"] {
        let r = s
            .download_file(1, bad, &local.path().join("x"), &mut no_progress())
            .await;
        assert!(matches!(r, Err(SessionError::Files(_))), "{bad}: {r:?}");
        assert!(!local.path().join("x").exists());
    }
    assert!(matches!(s.list_files("..").await, Err(SessionError::Files(_))));
    fs::write(local.path().join("f"), b"x").unwrap();
    let r = s
        .upload_file(2, &local.path().join("f"), "../escaped", &mut no_progress())
        .await;
    assert!(matches!(r, Err(SessionError::Files(_))));
    assert!(!root.path().join("escaped").exists());
    // Refusals do not end the session.
    assert!(s.ping(1).await.is_ok());
}

#[tokio::test]
async fn refusals_say_why_and_keep_the_session() {
    let (mut s, _serving, _client) = setup(vec![Permission::Files], None).await;
    match s.list_files("").await {
        Err(SessionError::Files(m)) => assert!(m.contains("turned off"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(s.ping(1).await.is_ok());

    let shared = tempfile::tempdir().unwrap();
    let (mut s, _serving, _client) = setup(vec![Permission::View], Some(shared.path())).await;
    match s.list_files("").await {
        Err(SessionError::Files(m)) => assert!(m.contains("not allowed"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(s.ping(1).await.is_ok());
}

#[tokio::test]
async fn a_cancelled_download_leaves_nothing_and_the_session_carries_on() {
    let shared = tempfile::tempdir().unwrap();
    fs::write(shared.path().join("big.bin"), pattern(500_000)).unwrap();
    let (mut s, _serving, _client) = setup(vec![Permission::Files], Some(shared.path())).await;
    let local = tempfile::tempdir().unwrap();
    let dest = local.path().join("big.bin");

    let r = s.download_file(1, "big.bin", &dest, &mut |d, _| d < 70_000).await;
    assert!(matches!(&r, Err(SessionError::Files(m)) if m == "cancelled"), "{r:?}");
    assert!(!dest.exists());
    assert_eq!(fs::read_dir(local.path()).unwrap().count(), 0, "no partial file");

    // Pieces still in flight are ignored, and the next request works.
    assert_eq!(s.list_files("").await.unwrap().len(), 1);
    let size = s.download_file(2, "big.bin", &dest, &mut no_progress()).await.unwrap();
    assert_eq!(size, 500_000);
}

#[tokio::test]
async fn a_cancelled_upload_leaves_no_partial_file_on_the_host() {
    let shared = tempfile::tempdir().unwrap();
    let (mut s, _serving, _client) = setup(vec![Permission::Files], Some(shared.path())).await;
    let local = tempfile::tempdir().unwrap();
    fs::write(local.path().join("big.bin"), pattern(500_000)).unwrap();

    let r = s
        .upload_file(1, &local.path().join("big.bin"), "big.bin", &mut |d, _| d < 70_000)
        .await;
    assert!(matches!(&r, Err(SessionError::Files(m)) if m == "cancelled"), "{r:?}");
    // The host handles the cancel before it answers this.
    assert!(s.list_files("").await.unwrap().is_empty());
    assert_eq!(fs::read_dir(shared.path()).unwrap().count(), 0);
}
