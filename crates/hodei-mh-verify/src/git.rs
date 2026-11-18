//! The identity of a candidate in git: the commit it claims and the
//! content it carries.
//!
//! A revision is observed, not declared: the runner reads the head of the
//! checkout it actually made and compares it with the revision the
//! candidate claimed. A candidate that names another commit is rejected
//! before its checks run. The tree is the content itself, as git names it:
//! two candidates with the same tree are the same content, whatever their
//! commits.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Length of a git object id (sha1).
const SHA1_HEX_LEN: usize = 40;
/// Length of a git object id (sha256).
const SHA256_HEX_LEN: usize = 64;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RevisionError {
    #[error("not a git object id: {0:?}")]
    NotAnObjectId(String),
}

/// Read a git object id as git wrote it: hexadecimal, either digest length,
/// in either case.
fn parse_object_id(text: &str) -> Result<String, RevisionError> {
    let trimmed = text.trim();
    let well_formed = (trimmed.len() == SHA1_HEX_LEN || trimmed.len() == SHA256_HEX_LEN)
        && trimmed.chars().all(|c| c.is_ascii_hexdigit());
    if well_formed {
        Ok(trimmed.to_ascii_lowercase())
    } else {
        Err(RevisionError::NotAnObjectId(trimmed.to_string()))
    }
}

/// The identity of a commit, as git names it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GitRevision(String);

impl GitRevision {
    /// Read a revision as git wrote it.
    pub fn parse(text: &str) -> Result<Self, RevisionError> {
        parse_object_id(text).map(Self)
    }

    /// The revision as git names it.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The identity of a content, as git names it: the tree of a commit.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GitTree(String);

impl GitTree {
    /// Read a tree id as git wrote it.
    pub fn parse(text: &str) -> Result<Self, RevisionError> {
        parse_object_id(text).map(Self)
    }

    /// The tree as git names it.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Whether the observed head is the declared revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevisionMatch {
    /// The checkout ran the declared revision.
    Matches,
    /// The checkout ran another commit.
    Differs {
        declared: GitRevision,
        observed: GitRevision,
    },
}

/// Compare the declared revision with the one observed in the checkout.
pub fn compare(declared: &GitRevision, observed: &GitRevision) -> RevisionMatch {
    if declared == observed {
        RevisionMatch::Matches
    } else {
        RevisionMatch::Differs {
            declared: declared.clone(),
            observed: observed.clone(),
        }
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

/// An empty commit (same tree) does not credit advancement.
pub fn tree_progress(previous: &GitTree, current: &GitTree) -> Progress {
    if previous == current {
        Progress::NoChange
    } else {
        Progress::Advanced
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "1f0c3a5b7d9e246183a5c7e9b0d3f5a7c9e1b3d5";
    const TREE: &str = "3d5f1b7c9e1b3d51f0c3a5b7d9e246183a5c7e9b";

    #[test]
    fn a_git_object_id_is_a_revision() {
        let revision = GitRevision::parse(HEAD).expect("the head is a revision");
        assert_eq!(revision.as_str(), HEAD);
    }

    #[test]
    fn a_revision_is_read_as_git_wrote_it() {
        let revision = GitRevision::parse(&format!("  {HEAD}\n")).expect("trimmed");
        assert_eq!(revision.as_str(), HEAD);
        let upper = GitRevision::parse(&HEAD.to_ascii_uppercase()).expect("hex is hex");
        assert_eq!(upper, revision);
    }

    #[test]
    fn something_that_is_not_an_object_id_is_not_a_revision() {
        for text in [
            "example-main",
            "",
            "abc",
            "zzzz1f0c3a5b7d9e246183a5c7e9b0d3f5a7c9e1b3d5",
        ] {
            assert!(
                GitRevision::parse(text).is_err(),
                "{text:?} is not a revision"
            );
            assert!(GitTree::parse(text).is_err(), "{text:?} is not a tree");
        }
    }

    #[test]
    fn the_declared_revision_is_compared_with_the_observed_one() {
        let declared = GitRevision::parse(HEAD).expect("revision");
        let observed =
            GitRevision::parse("2f0c3a5b7d9e246183a5c7e9b0d3f5a7c9e1b3d5").expect("revision");
        assert_eq!(compare(&declared, &declared), RevisionMatch::Matches);
        match compare(&declared, &observed) {
            RevisionMatch::Differs {
                declared: d,
                observed: o,
            } => {
                assert_eq!(d, declared);
                assert_eq!(o, observed);
            }
            other => panic!("the revisions differ: {other:?}"),
        }
    }

    #[test]
    fn a_tree_is_the_content_identity() {
        let tree = GitTree::parse(TREE).expect("a tree");
        assert_eq!(tree.as_str(), TREE);
        assert_ne!(tree, GitTree::parse(HEAD).expect("a tree"));
    }

    #[test]
    fn an_empty_commit_is_not_progress() {
        let before = GitTree::parse(TREE).expect("a tree");
        let after = GitTree::parse(TREE).expect("the same tree");
        assert_eq!(tree_progress(&before, &after), Progress::NoChange);
        let other = GitTree::parse(HEAD).expect("a tree");
        assert_eq!(tree_progress(&before, &other), Progress::Advanced);
    }
}
