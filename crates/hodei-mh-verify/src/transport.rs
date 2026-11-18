//! The transport of a candidate: the bytes that carry it.
//!
//! The transport is not the candidate. A bundle can be truncated, swapped
//! or replaced between the moment it is declared and the moment it is
//! executed, so its digest is observed before anything runs: a transport
//! that does not match its declared digest is rejected before the checks.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// The identity of the bytes that carry a candidate.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TransportDigest(String);

impl TransportDigest {
    /// The digest of the transport bytes.
    pub fn of(bytes: &[u8]) -> Self {
        Self(format!("{:x}", Sha256::digest(bytes)))
    }

    /// The digest as hexadecimal.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_transport_digest_is_the_digest_of_its_bytes() {
        let digest = TransportDigest::of(b"candidate bundle");
        assert_eq!(digest, TransportDigest::of(b"candidate bundle"));
        assert_ne!(digest, TransportDigest::of(b"candidate bundle "));
        assert_eq!(digest.as_str().len(), 64);
    }

    #[test]
    fn an_altered_transport_is_not_the_same_transport() {
        let original = TransportDigest::of(b"the bundle as it was");
        let altered = TransportDigest::of(b"the bundle as it was tampered");
        assert_ne!(original, altered);
    }
}
