//! A queue of questions for a person, for front ends that cannot answer inside the call.
//!
//! The session code awaits an answer; the front end (Electron, SwiftUI) polls
//! [`PromptQueue::pending`], shows what is there, and calls [`PromptQueue::answer`].
//! A question that times out, or whose session ends, leaves the queue by itself.

use crate::session::{BoxFuture, Confirmer, PeerInfo};
use crate::sas::Sas;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// A paired device wants to connect: allow or deny.
    Connection,
    /// Pairing: do the symbols match what the other device shows?
    Verify,
}

impl PromptKind {
    pub fn name(self) -> &'static str {
        match self {
            PromptKind::Connection => "connection",
            PromptKind::Verify => "verify",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Prompt {
    pub id: u64,
    pub kind: PromptKind,
    pub peer_name: String,
    pub peer_platform: String,
    pub peer_fingerprint: String,
    /// (symbol, name) pairs. Empty for a connection prompt.
    pub emoji: Vec<(String, String)>,
    /// Empty for a connection prompt.
    pub digits: String,
}

#[derive(Default)]
pub struct PromptQueue {
    next: AtomicU64,
    pending: Mutex<Vec<(Prompt, Option<oneshot::Sender<bool>>)>>,
}

struct Leave<'a> {
    queue: &'a PromptQueue,
    id: u64,
}

impl Drop for Leave<'_> {
    fn drop(&mut self) {
        self.queue
            .pending
            .lock()
            .expect("prompt lock")
            .retain(|(p, _)| p.id != self.id);
    }
}

impl PromptQueue {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Questions waiting for an answer, oldest first.
    pub fn pending(&self) -> Vec<Prompt> {
        let q = self.pending.lock().expect("prompt lock");
        q.iter().map(|(p, _)| p.clone()).collect()
    }

    /// Answers a question. False if it is gone (timed out, or already answered).
    pub fn answer(&self, id: u64, accept: bool) -> bool {
        let mut q = self.pending.lock().expect("prompt lock");
        let Some(i) = q.iter().position(|(p, _)| p.id == id) else {
            return false;
        };
        let (_, tx) = q.remove(i);
        tx.is_some_and(|tx| tx.send(accept).is_ok())
    }

    async fn ask(&self, mut prompt: Prompt) -> bool {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        prompt.id = id;
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("prompt lock")
            .push((prompt, Some(tx)));
        let _leave = Leave { queue: self, id };
        rx.await.unwrap_or(false)
    }
}

/// A [`Confirmer`] that puts its questions on a [`PromptQueue`].
pub struct QueueConfirmer {
    pub queue: Arc<PromptQueue>,
    /// Ask before letting an already paired device connect.
    pub ask_connections: bool,
    /// Ask people to compare symbols when pairing.
    pub ask_verification: bool,
}

fn prompt_for(kind: PromptKind, peer: &PeerInfo) -> Prompt {
    Prompt {
        id: 0,
        kind,
        peer_name: peer.name.clone(),
        peer_platform: peer.platform.to_string(),
        peer_fingerprint: peer.fingerprint(),
        emoji: Vec::new(),
        digits: String::new(),
    }
}

impl Confirmer for QueueConfirmer {
    fn approve_connection<'a>(&'a self, peer: &'a PeerInfo) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            if !self.ask_connections {
                return true;
            }
            self.queue.ask(prompt_for(PromptKind::Connection, peer)).await
        })
    }

    fn verify_sas<'a>(&'a self, sas: &'a Sas, peer: &'a PeerInfo) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            if !self.ask_verification {
                return true;
            }
            let mut prompt = prompt_for(PromptKind::Verify, peer);
            prompt.emoji = sas
                .emoji()
                .iter()
                .map(|(s, n)| ((*s).to_owned(), (*n).to_owned()))
                .collect();
            prompt.digits = sas.digits();
            self.queue.ask(prompt).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tether_proto::Platform;

    fn peer() -> PeerInfo {
        PeerInfo {
            name: "laptop".into(),
            platform: Platform::Windows,
            public_key: [4; 32],
        }
    }

    fn confirmer(queue: &Arc<PromptQueue>) -> QueueConfirmer {
        QueueConfirmer {
            queue: queue.clone(),
            ask_connections: true,
            ask_verification: true,
        }
    }

    #[tokio::test]
    async fn answers_reach_the_waiting_session() {
        let queue = PromptQueue::new();
        let c = confirmer(&queue);
        let p = peer();
        let asked = c.approve_connection(&p);
        let answer = async {
            loop {
                if let Some(prompt) = queue.pending().first() {
                    assert_eq!(prompt.kind, PromptKind::Connection);
                    assert_eq!(prompt.peer_name, "laptop");
                    assert!(queue.answer(prompt.id, true));
                    assert!(!queue.answer(prompt.id, true), "only once");
                    return;
                }
                tokio::task::yield_now().await;
            }
        };
        let (allowed, ()) = tokio::join!(asked, answer);
        assert!(allowed);
        assert!(queue.pending().is_empty());
    }

    #[tokio::test]
    async fn a_dropped_question_leaves_the_queue() {
        let queue = PromptQueue::new();
        let c = confirmer(&queue);
        let p = peer();
        let timed_out = tokio::time::timeout(
            std::time::Duration::from_millis(30),
            c.approve_connection(&p),
        )
        .await;
        assert!(timed_out.is_err());
        assert!(queue.pending().is_empty());
    }

    #[tokio::test]
    async fn switched_off_questions_say_yes_without_asking() {
        let queue = PromptQueue::new();
        let c = QueueConfirmer {
            queue: queue.clone(),
            ask_connections: false,
            ask_verification: false,
        };
        let p = peer();
        assert!(c.approve_connection(&p).await);
        assert!(c.verify_sas(&Sas::derive(&[0; 32], &[0; 32], &[0; 32]), &p).await);
        assert!(queue.pending().is_empty());
    }
}
