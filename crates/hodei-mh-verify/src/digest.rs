//! Candidate identity (AC-2): a candidate is identified by a content digest,
//! so evidence is bound to the exact state of the work — not to a name or a
//! claim. An empty commit produces the SAME digest: it cannot vouch progress
//! (AC-3).

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// Hex-encoded SHA-256 over the candidate's content, in a canonical
/// (sorted, path-prefixed) order.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CandidateDigest(pub String);

/// One content file of the candidate: repo-relative path plus bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct ContentFile {
    pub path: String,
    pub bytes: Vec<u8>,
}

impl CandidateDigest {
    /// Digest over a file set: paths sorted, each file framed as
    /// `path\nlen\nbytes` so concatenations cannot collide.
    pub fn from_files(files: &[ContentFile]) -> Self {
        let mut sorted: Vec<&ContentFile> = files.iter().collect();
        sorted.sort_by(|a, b| a.path.cmp(&b.path));
        let mut hasher = Sha256::new();
        for file in sorted {
            hasher.update(file.path.as_bytes());
            hasher.update(b"\n");
            hasher.update(file.bytes.len().to_le_bytes());
            hasher.update(b"\n");
            hasher.update(&file.bytes);
        }
        Self(format!("{:x}", hasher.finalize()))
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(format!("{:x}", Sha256::digest(bytes)))
    }
}

/// Whether a candidate transition represents real content progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// Content changed: previous evidence may be invalidated.
    Advanced,
    /// Same content (e.g. an empty commit): no progress can be claimed.
    NoChange,
}

/// AC-3: an empty commit (same tree) does not credit advancement.
pub fn candidate_progress(previous: &CandidateDigest, current: &CandidateDigest) -> Progress {
    if previous == current {
        Progress::NoChange
    } else {
        Progress::Advanced
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> Vec<ContentFile> {
        vec![
            ContentFile {
                path: "src/main.rs".into(),
                bytes: b"fn main() {}".to_vec(),
            },
            ContentFile {
                path: "Cargo.toml".into(),
                bytes: b"[package]\nname=\"x\"".to_vec(),
            },
        ]
    }

    #[test]
    fn digest_is_deterministic_and_order_insensitive() {
        let a = CandidateDigest::from_files(&files());
        let mut shuffled = files();
        shuffled.reverse();
        let b = CandidateDigest::from_files(&shuffled);
        assert_eq!(a, b);
        assert_eq!(a.0.len(), 64);
    }

    #[test]
    fn any_content_change_changes_the_digest() {
        let mut changed = files();
        changed[0].bytes = b"fn main() { println!(); }".to_vec();
        assert_ne!(
            CandidateDigest::from_files(&files()),
            CandidateDigest::from_files(&changed)
        );
    }

    #[test]
    fn framing_prevents_path_boundary_collisions() {
        let x = CandidateDigest::from_files(&[ContentFile {
            path: "ab".into(),
            bytes: b"c".to_vec(),
        }]);
        let y = CandidateDigest::from_files(&[ContentFile {
            path: "a".into(),
            bytes: b"bc".to_vec(),
        }]);
        assert_ne!(x, y);
    }

    #[test]
    fn empty_commit_is_not_progress() {
        let d1 = CandidateDigest::from_files(&files());
        let d2 = CandidateDigest::from_files(&files());
        assert_eq!(candidate_progress(&d1, &d2), Progress::NoChange);
        let mut changed = files();
        changed[1].bytes = b"[package]\nname=\"y\"".to_vec();
        let d3 = CandidateDigest::from_files(&changed);
        assert_eq!(candidate_progress(&d1, &d3), Progress::Advanced);
    }
}
