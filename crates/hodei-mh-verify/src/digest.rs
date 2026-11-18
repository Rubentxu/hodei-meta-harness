//! The identity of a content blob: a digest over bytes.
//!
//! The identity of a candidate in git is not a digest: it is the commit
//! and the tree git names for it (see `git`). A digest over bytes is what
//! identifies a durable artifact — an export that is written to storage
//! and read back — where there is no git to ask.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// Hex-encoded SHA-256 over a content blob.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CandidateDigest(pub String);

impl CandidateDigest {
    /// Digest over the bytes of a content blob.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(format!("{:x}", Sha256::digest(bytes)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_digest_is_the_digest_of_its_bytes() {
        let digest = CandidateDigest::from_bytes(b"the exported candidate");
        assert_eq!(
            digest,
            CandidateDigest::from_bytes(b"the exported candidate")
        );
        assert_ne!(
            digest,
            CandidateDigest::from_bytes(b"the exported candidate ")
        );
        assert_eq!(digest.0.len(), 64);
    }
}
