//! Pairing verification and connection approval, with two people simulated by `Person`.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tether_core::{
    identity::Identity,
    pairing::PairingOffer,
    sas::Sas,
    session::{
        accept_with, connect_with, BoxFuture, ClientMode, Confirmer, HostConfig, Outcome,
        PeerInfo, SessionError,
    },
    trust::TrustStore,
};
use tokio::io::{duplex, DuplexStream};

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

fn host_config(d: &Dev, offer: Option<PairingOffer>) -> HostConfig {
    HostConfig {
        identity: d.identity.clone(),
        trust: d.trust.clone(),
        pairing: Arc::new(Mutex::new(offer)),
        device_name: "host".into(),
        media: None,
        input: None,
        wake: None,
        clipboard: None,
        files: None,
    }
}

/// Answers every question the same way and remembers what it was shown.
struct Person {
    says_match: bool,
    allows: bool,
    saw: Mutex<Vec<String>>,
}

impl Person {
    fn new(says_match: bool, allows: bool) -> Self {
        Self {
            says_match,
            allows,
            saw: Mutex::new(Vec::new()),
        }
    }
}

impl Confirmer for Person {
    fn approve_connection<'a>(&'a self, _peer: &'a PeerInfo) -> BoxFuture<'a, bool> {
        Box::pin(async move { self.allows })
    }

    fn verify_sas<'a>(&'a self, sas: &'a Sas, _peer: &'a PeerInfo) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            self.saw.lock().unwrap().push(sas.digits());
            self.says_match
        })
    }
}

type Pair = (
    Result<Outcome<DuplexStream>, SessionError>,
    Result<Outcome<DuplexStream>, SessionError>,
);

#[allow(clippy::await_holding_lock)]
async fn pair(host: &Dev, client: &Dev, host_person: &Person, client_person: &Person) -> Pair {
    let offer = PairingOffer::new(Duration::from_secs(60));
    let code = offer.display_code();
    let cfg = host_config(host, Some(offer));
    let (a, b) = duplex(1 << 16);
    let mut trust = client.trust.lock().unwrap();
    tokio::join!(
        accept_with(a, &cfg, Some(host_person)),
        connect_with(
            b,
            &client.identity,
            &mut trust,
            "client",
            ClientMode::Pair { code },
            Some(client_person),
        )
    )
}

fn trusted(d: &Dev) -> usize {
    d.trust.lock().unwrap().peers().len()
}

#[tokio::test]
async fn both_people_see_the_same_symbols_and_confirm() {
    let (host, client) = (dev(), dev());
    let (hp, cp) = (Person::new(true, true), Person::new(true, true));
    let (h, c) = pair(&host, &client, &hp, &cp).await;
    assert!(matches!(h, Ok(Outcome::Paired(_))));
    assert!(matches!(c, Ok(Outcome::Paired(_))));
    assert_eq!(trusted(&host), 1);
    assert_eq!(trusted(&client), 1);
    let (hs, cs) = (hp.saw.lock().unwrap(), cp.saw.lock().unwrap());
    assert_eq!(hs.len(), 1);
    assert_eq!(*hs, *cs, "both devices must show the same string");
}

#[tokio::test]
async fn each_pairing_shows_a_fresh_string() {
    let mut seen = std::collections::HashSet::new();
    for _ in 0..4 {
        let (host, client) = (dev(), dev());
        let (hp, cp) = (Person::new(true, true), Person::new(true, true));
        let _ = pair(&host, &client, &hp, &cp).await;
        seen.insert(hp.saw.lock().unwrap()[0].clone());
    }
    assert_eq!(seen.len(), 4);
}

#[tokio::test]
async fn nobody_is_stored_if_the_host_says_no() {
    let (host, client) = (dev(), dev());
    let (hp, cp) = (Person::new(false, true), Person::new(true, true));
    let (h, c) = pair(&host, &client, &hp, &cp).await;
    assert!(matches!(h, Err(SessionError::VerificationFailed(_))));
    assert!(matches!(c, Err(SessionError::VerificationFailed(_))));
    assert_eq!(trusted(&host), 0);
    assert_eq!(trusted(&client), 0);
}

#[tokio::test]
async fn nobody_is_stored_if_the_client_says_no() {
    let (host, client) = (dev(), dev());
    let (hp, cp) = (Person::new(true, true), Person::new(false, true));
    let (h, c) = pair(&host, &client, &hp, &cp).await;
    assert!(matches!(h, Err(SessionError::VerificationFailed(_))));
    assert!(matches!(c, Err(SessionError::VerificationFailed(_))));
    assert_eq!(trusted(&host), 0);
    assert_eq!(trusted(&client), 0);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_paired_device_can_be_turned_away_at_the_door() {
    let (host, client) = (dev(), dev());
    let yes = Person::new(true, true);
    let (h, c) = pair(&host, &client, &yes, &yes).await;
    assert!(h.is_ok() && c.is_ok());

    for (allows, expect_ok) in [(false, false), (true, true)] {
        let door = Person::new(true, allows);
        let cfg = host_config(&host, None);
        let (a, b) = duplex(1 << 16);
        let mut trust = client.trust.lock().unwrap();
        let (h, c) = tokio::join!(
            accept_with(a, &cfg, Some(&door)),
            connect_with(
                b,
                &client.identity,
                &mut trust,
                "client",
                ClientMode::Connect,
                None
            )
        );
        if expect_ok {
            assert!(matches!(h, Ok(Outcome::Authenticated(_))));
            assert!(matches!(c, Ok(Outcome::Authenticated(_))));
        } else {
            assert!(matches!(h, Err(SessionError::Declined)));
            match c {
                Err(SessionError::Rejected(reason)) => assert!(reason.contains("declined")),
                _ => panic!("the client should be told it was declined"),
            }
        }
    }
}
