//! Short authentication strings: the emoji (or digits) both people compare when pairing.
//!
//! Both devices compute the same string from the Noise handshake hash and two random values,
//! one from each side. The hash ties it to the keys this session really uses, so a
//! man-in-the-middle, who holds a different session with each device, ends up with a
//! different string on each side.
//!
//! The random values are exchanged commit-then-reveal. The client sends a hash of its value
//! first, the host answers with its own value, and only then does the client reveal. Without
//! that order, an attacker who learns both values early could keep generating handshake keys
//! until the two sessions happened to produce the same short string. With it, the attacker
//! has to fix both sessions before it knows what either string will be, and gets one guess
//! (1 in 2^42) per attempt, which the people see fail.

use sha2::{Digest, Sha256};

const DERIVE_TAG: &[u8] = b"tether/sas/derive/1";
const COMMIT_TAG: &[u8] = b"tether/sas/commit/1";

/// 64 symbols, 6 bits each. Names are shown under the symbol so the comparison also works
/// for people who cannot see the picture, or whose device draws emoji differently.
pub const EMOJI: [(&str, &str); 64] = [
    ("🐶", "Dog"),
    ("🐱", "Cat"),
    ("🦁", "Lion"),
    ("🐎", "Horse"),
    ("🦄", "Unicorn"),
    ("🐷", "Pig"),
    ("🐘", "Elephant"),
    ("🐰", "Rabbit"),
    ("🐼", "Panda"),
    ("🐓", "Rooster"),
    ("🐧", "Penguin"),
    ("🐢", "Turtle"),
    ("🐟", "Fish"),
    ("🐙", "Octopus"),
    ("🦋", "Butterfly"),
    ("🌷", "Flower"),
    ("🌳", "Tree"),
    ("🌵", "Cactus"),
    ("🍄", "Mushroom"),
    ("🌏", "Globe"),
    ("🌙", "Moon"),
    ("☁️", "Cloud"),
    ("🔥", "Fire"),
    ("🍌", "Banana"),
    ("🍎", "Apple"),
    ("🍓", "Strawberry"),
    ("🌽", "Corn"),
    ("🍕", "Pizza"),
    ("🎂", "Cake"),
    ("❤️", "Heart"),
    ("😀", "Smiley"),
    ("🤖", "Robot"),
    ("🎩", "Hat"),
    ("👓", "Glasses"),
    ("🔧", "Spanner"),
    ("🎅", "Santa"),
    ("👍", "Thumbs up"),
    ("☂️", "Umbrella"),
    ("⌛", "Hourglass"),
    ("⏰", "Clock"),
    ("🎁", "Gift"),
    ("💡", "Light bulb"),
    ("📕", "Book"),
    ("✏️", "Pencil"),
    ("📎", "Paperclip"),
    ("✂️", "Scissors"),
    ("🔒", "Lock"),
    ("🔑", "Key"),
    ("🔨", "Hammer"),
    ("☎️", "Telephone"),
    ("🏁", "Flag"),
    ("🚂", "Train"),
    ("🚲", "Bicycle"),
    ("✈️", "Airplane"),
    ("🚀", "Rocket"),
    ("🏆", "Trophy"),
    ("⚽", "Ball"),
    ("🎸", "Guitar"),
    ("🎺", "Trumpet"),
    ("🔔", "Bell"),
    ("⚓", "Anchor"),
    ("🎧", "Headphones"),
    ("📁", "Folder"),
    ("📌", "Pin"),
];

pub type Nonce = [u8; 32];

/// What the client sends before it reveals its value.
pub fn commitment(handshake_hash: &[u8], client_nonce: &Nonce) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(COMMIT_TAG);
    h.update([handshake_hash.len() as u8]);
    h.update(handshake_hash);
    h.update(client_nonce);
    h.finalize().into()
}

#[derive(Clone, PartialEq, Eq)]
pub struct Sas {
    /// 42 significant bits.
    value: u64,
}

impl std::fmt::Debug for Sas {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sas(..)")
    }
}

impl Sas {
    pub fn derive(handshake_hash: &[u8], client_nonce: &Nonce, host_nonce: &Nonce) -> Self {
        let mut h = Sha256::new();
        h.update(DERIVE_TAG);
        h.update([handshake_hash.len() as u8]);
        h.update(handshake_hash);
        h.update(client_nonce);
        h.update(host_nonce);
        let digest = h.finalize();
        let mut six = [0u8; 8];
        six[2..].copy_from_slice(&digest[..6]);
        // Keep the top 42 of the 48 bits.
        Self {
            value: u64::from_be_bytes(six) >> 6,
        }
    }

    /// Seven symbols with their names.
    pub fn emoji(&self) -> [(&'static str, &'static str); 7] {
        std::array::from_fn(|i| EMOJI[((self.value >> (36 - 6 * i)) & 63) as usize])
    }

    /// The same 42 bits as digits, for a terminal that cannot draw emoji: `0000-0000-00000`.
    pub fn digits(&self) -> String {
        let n = self.value; // < 2^42 < 10^13
        format!("{:04}-{:04}-{:05}", n / 1_000_000_000, (n / 100_000) % 10_000, n % 100_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn table_has_64_distinct_entries() {
        let symbols: HashSet<_> = EMOJI.iter().map(|e| e.0).collect();
        let names: HashSet<_> = EMOJI.iter().map(|e| e.1).collect();
        assert_eq!(symbols.len(), 64);
        assert_eq!(names.len(), 64);
    }

    #[test]
    fn both_sides_agree_and_any_change_alters_it() {
        let (h, c, s) = ([1u8; 32], [2u8; 32], [3u8; 32]);
        let a = Sas::derive(&h, &c, &s);
        assert_eq!(a, Sas::derive(&h, &c, &s));
        assert_eq!(a.emoji(), Sas::derive(&h, &c, &s).emoji());
        assert_ne!(a, Sas::derive(&[9u8; 32], &c, &s), "different session");
        assert_ne!(a, Sas::derive(&h, &[9u8; 32], &s), "different client value");
        assert_ne!(a, Sas::derive(&h, &c, &[9u8; 32]), "different host value");
        assert_ne!(a, Sas::derive(&h, &s, &c), "roles are not interchangeable");
    }

    #[test]
    fn commitment_binds_the_value_and_the_session() {
        let (h, n) = ([1u8; 32], [5u8; 32]);
        let c = commitment(&h, &n);
        assert_eq!(c, commitment(&h, &n));
        assert_ne!(c, commitment(&h, &[6u8; 32]));
        assert_ne!(c, commitment(&[2u8; 32], &n));
    }

    #[test]
    fn digits_have_a_fixed_shape() {
        for seed in 0..50u8 {
            let d = Sas::derive(&[seed; 32], &[1; 32], &[2; 32]).digits();
            assert_eq!(d.len(), 15, "{d}");
            assert!(d.chars().all(|c| c.is_ascii_digit() || c == '-'));
        }
        let max = Sas { value: (1 << 42) - 1 };
        assert_eq!(max.digits(), "4398-0465-11103");
    }
}
