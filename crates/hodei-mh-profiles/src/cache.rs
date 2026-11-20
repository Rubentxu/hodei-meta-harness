//! Opt-in build cache (pure): identity, integrity, scope and quota.
//!
//! The cache is a host-side store of archives keyed by the project, the
//! pinned toolchain image, the dependency lock and the cache scope. Nothing
//! in this module touches the filesystem: the identity, the restore verdict
//! and the eviction plan are total functions, so the safety rules can be
//! tested offline and the adapter only carries out their decisions.
//!
//! Two invariants are the reason the cache exists in this shape:
//!
//! - Different project, image, toolchain, lockfile or scope is a different
//!   key: stale mutable artifacts can never be read by another build.
//! - An archive that does not match the digest the manifest recorded is a
//!   MISS, never an input: a corrupted or replaced cache is discarded and
//!   the build runs cold.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// Bumped whenever the meaning of a key or an archive changes: an archive
/// written under another format is unrecognizable, so it is not restored.
pub const CACHE_FORMAT_VERSION: u32 = 1;

/// Byte size of one mebibyte, for the CLI's `--cache-quota-mib`.
pub const MIB: u64 = 1024 * 1024;

/// The default host cache quota: 1 GiB, pruned oldest-first.
pub const DEFAULT_QUOTA_BYTES: u64 = 1024 * MIB;

/// The directory, inside the store root, that holds the archives.
pub const ENTRIES_DIR: &str = "entries";

/// The manifest that names the archives and their integrity digests.
pub const MANIFEST_FILE: &str = "manifest.json";

/// What part of the build a cache archive carries. The download cache is
/// preferred: compiled outputs are only shared when the operator asks for
/// it, because they are the mutable artifacts a wrong key could poison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheScope {
    /// The toolchain's dependency download/registry cache.
    Downloads,
    /// The compiled build outputs (the target directory, classes, ...).
    BuildOutputs,
}

impl CacheScope {
    pub fn as_str(self) -> &'static str {
        match self {
            CacheScope::Downloads => "downloads",
            CacheScope::BuildOutputs => "build-outputs",
        }
    }
}

/// Whether the caller is the developer loop or the verifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheUse {
    Development,
    Verification,
}

/// The CLI and environment inputs the policy reads. The caller reads them
/// at the edge; this module never looks at the process environment.
#[derive(Debug, Clone, Copy, Default)]
pub struct CacheEnv<'a> {
    pub enabled: Option<&'a str>,
    pub store_root: Option<&'a str>,
}

/// The resolved policy for one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheSettings {
    pub enabled: bool,
    pub store_root: String,
    pub quota_bytes: u64,
    pub scope: CacheScope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheSettingsError {
    InvalidQuota,
}

impl fmt::Display for CacheSettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CacheSettingsError::InvalidQuota => write!(
                f,
                "--cache-quota-mib expects a positive integer number of MiB"
            ),
        }
    }
}

impl std::error::Error for CacheSettingsError {}

fn flag_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|argument| argument == name)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}

/// Resolve the OPT-IN cache policy.
///
/// The cache is off unless `--cache` (or `HODEI_MH_BUILD_CACHE=1`) asks for
/// it; `--no-cache` wins over both. A verification caller is ALWAYS off: a
/// cache written by an earlier build is not evidence, and a clean checkout
/// is the whole point of verification.
///
/// `--cache-target` extends the cache from the dependency downloads to the
/// compiled outputs. `--cache-dir` and `--cache-quota-mib` override the
/// store root and the quota.
pub fn resolve_settings(
    args: &[String],
    env: CacheEnv<'_>,
    default_store_root: &str,
    usage: CacheUse,
) -> Result<CacheSettings, CacheSettingsError> {
    let quota_mib = match flag_value(args, "--cache-quota-mib") {
        None => DEFAULT_QUOTA_BYTES / MIB,
        Some(raw) => raw
            .parse::<u64>()
            .ok()
            .filter(|mib| *mib > 0)
            .ok_or(CacheSettingsError::InvalidQuota)?,
    };
    let requested = args.iter().any(|argument| argument == "--cache") || env.enabled == Some("1");
    let refused = args.iter().any(|argument| argument == "--no-cache");
    let enabled = match usage {
        CacheUse::Verification => false,
        CacheUse::Development => requested && !refused,
    };
    let scope = if args.iter().any(|argument| argument == "--cache-target") {
        CacheScope::BuildOutputs
    } else {
        CacheScope::Downloads
    };
    let store_root = flag_value(args, "--cache-dir")
        .or(env.store_root)
        .unwrap_or(default_store_root)
        .to_string();
    Ok(CacheSettings {
        enabled,
        store_root,
        quota_bytes: quota_mib * MIB,
        scope,
    })
}

/// The cache identity of one build: every field that can change the
/// meaning of a mutable artifact is part of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildCacheIdentity {
    pub profile: String,
    /// The pinned image (`name:tag@sha256:...`), so another toolchain is
    /// another cache.
    pub image: String,
    pub toolchain: String,
    /// Digest of the dependency lock, or `none` when the project has none.
    pub lockfile: String,
    /// Per-project token derived from the canonical absolute path.
    pub project: String,
    pub scope: CacheScope,
}

impl BuildCacheIdentity {
    pub fn new(
        profile: &str,
        image: &str,
        toolchain: &str,
        lockfile: &str,
        project: &str,
        scope: CacheScope,
    ) -> Self {
        Self {
            profile: profile.to_string(),
            image: image.to_string(),
            toolchain: toolchain.to_string(),
            lockfile: lockfile.to_string(),
            project: project.to_string(),
            scope,
        }
    }

    /// The stable hex key of this identity. Every field is length-prefixed
    /// so no two different identities can collide by concatenation.
    pub fn key(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(CACHE_FORMAT_VERSION.to_le_bytes());
        for field in [
            self.profile.as_str(),
            self.image.as_str(),
            self.toolchain.as_str(),
            self.lockfile.as_str(),
            self.project.as_str(),
            self.scope.as_str(),
        ] {
            hasher.update((field.len() as u64).to_le_bytes());
            hasher.update(field.as_bytes());
        }
        format!("{:x}", hasher.finalize())
    }
}

/// Digest of the dependency locks: the named files in a stable order. An
/// empty list is `none`, so a lock that appears later is a different cache.
pub fn lockfile_digest(files: &[(&str, &[u8])]) -> String {
    if files.is_empty() {
        return "none".to_string();
    }
    let mut ordered: Vec<&(&str, &[u8])> = files.iter().collect();
    ordered.sort_by(|a, b| a.0.cmp(b.0));
    let mut hasher = Sha256::new();
    for (name, bytes) in ordered {
        hasher.update((name.len() as u64).to_le_bytes());
        hasher.update(name.as_bytes());
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    format!("{:x}", hasher.finalize())
}

/// The subdirectory of the plan's cache directory a scope archives, or
/// `None` when this profile has nothing of that kind (the JVM profile has
/// no dependency download cache). The empty string means the whole cache
/// directory.
pub fn scope_dir(profile: &str, scope: CacheScope) -> Option<&'static str> {
    match (profile, scope) {
        ("rust", CacheScope::Downloads) => Some("cargo"),
        (_, CacheScope::Downloads) => None,
        (_, CacheScope::BuildOutputs) => Some(""),
    }
}

/// The archive name of one key. The key is hex, so it is a safe file name.
pub fn archive_name(key: &str) -> String {
    format!("{key}.tar.gz")
}

pub fn archive_path(store_root: &str, key: &str) -> String {
    format!(
        "{}/{ENTRIES_DIR}/{}",
        store_root.trim_end_matches('/'),
        archive_name(key)
    )
}

pub fn manifest_path(store_root: &str) -> String {
    format!("{}/{}", store_root.trim_end_matches('/'), MANIFEST_FILE)
}

/// SHA-256 of the archive bytes, as the manifest records it.
pub fn archive_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// One stored archive, as the manifest describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheEntry {
    pub key: String,
    /// SHA-256 of the archive bytes the cache stored.
    pub digest: String,
    pub size_bytes: u64,
    /// Seconds since the Unix epoch: eviction order, oldest first.
    pub last_used_epoch_secs: u64,
}

/// The store's index: which archives exist and what they must contain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheManifest {
    pub version: u32,
    pub entries: Vec<CacheEntry>,
}

impl Default for CacheManifest {
    fn default() -> Self {
        Self {
            version: CACHE_FORMAT_VERSION,
            entries: Vec::new(),
        }
    }
}

impl CacheManifest {
    pub fn is_current(&self) -> bool {
        self.version == CACHE_FORMAT_VERSION
    }

    pub fn find(&self, key: &str) -> Option<&CacheEntry> {
        self.entries.iter().find(|entry| entry.key == key)
    }

    /// Record (or replace) the entry of one key.
    pub fn record(&mut self, entry: CacheEntry) {
        self.entries.retain(|existing| existing.key != entry.key);
        self.entries.push(entry);
    }

    pub fn remove(&mut self, key: &str) {
        self.entries.retain(|entry| entry.key != key);
    }

    pub fn total_bytes(&self) -> u64 {
        self.entries
            .iter()
            .fold(0u64, |total, entry| total.saturating_add(entry.size_bytes))
    }
}

/// What a stored archive is worth for the build that is about to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreVerdict {
    /// Nothing usable for this key: build cold.
    ColdMiss,
    /// The archive is the one the manifest recorded for this key.
    Verified,
    /// The archive is not the one the manifest recorded (or the entry
    /// belongs to another key): DISCARD it, never restore it.
    RejectedCorrupted { reason: &'static str },
}

/// Decide whether a stored archive may be restored. Only a manifest entry
/// for THIS key whose digest matches the bytes may be used: a cache that
/// was corrupted or replaced is a miss, never a silent input to a build.
pub fn restore_verdict(
    entry: Option<&CacheEntry>,
    key: &str,
    observed_digest: &str,
) -> RestoreVerdict {
    match entry {
        None => RestoreVerdict::ColdMiss,
        Some(entry) if entry.key != key => RestoreVerdict::RejectedCorrupted {
            reason: "the manifest entry belongs to another key",
        },
        Some(entry) if entry.digest != observed_digest => RestoreVerdict::RejectedCorrupted {
            reason: "the archive digest does not match the manifest",
        },
        Some(_) => RestoreVerdict::Verified,
    }
}

/// Whether one archive fits the quota at all: storing an entry larger than
/// the whole budget would leave the cache permanently over it.
pub fn fits_quota(size_bytes: u64, quota_bytes: u64) -> bool {
    size_bytes <= quota_bytes
}

/// The keys to evict, oldest first, so the cache fits its quota after the
/// `keep` entry is recorded. `keep` is never evicted: the build that just
/// produced it paid for it, and `fits_quota` already refused it when it
/// could not fit on its own. The plan is deterministic (ties break on the
/// key), so two runs with the same manifest evict the same archives.
pub fn plan_eviction(entries: &[CacheEntry], quota_bytes: u64, keep: &str) -> Vec<String> {
    let total = entries
        .iter()
        .fold(0u64, |sum, entry| sum.saturating_add(entry.size_bytes));
    if total <= quota_bytes {
        return Vec::new();
    }
    let mut candidates: Vec<&CacheEntry> =
        entries.iter().filter(|entry| entry.key != keep).collect();
    candidates.sort_by(|a, b| {
        a.last_used_epoch_secs
            .cmp(&b.last_used_epoch_secs)
            .then_with(|| a.key.cmp(&b.key))
    });
    let mut remaining = total;
    let mut evicted = Vec::new();
    for entry in candidates {
        if remaining <= quota_bytes {
            break;
        }
        remaining = remaining.saturating_sub(entry.size_bytes);
        evicted.push(entry.key.clone());
    }
    evicted
}

/// Archive files on disk that no manifest entry describes. They can never
/// be verified, so they are garbage, not cache: the adapter removes them.
pub fn unreferenced_archives(referenced_keys: &[String], present: &[String]) -> Vec<String> {
    present
        .iter()
        .filter(|name| {
            !referenced_keys
                .iter()
                .any(|key| archive_name(key) == **name)
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn identity(project: &str, image: &str, lockfile: &str) -> BuildCacheIdentity {
        BuildCacheIdentity::new(
            "rust",
            image,
            "rust 1.100.0",
            lockfile,
            project,
            CacheScope::Downloads,
        )
    }

    fn entry(key: &str, size_bytes: u64, last_used_epoch_secs: u64) -> CacheEntry {
        CacheEntry {
            key: key.to_string(),
            digest: format!("digest-of-{key}"),
            size_bytes,
            last_used_epoch_secs,
        }
    }

    #[test]
    fn cache_is_opt_in_and_no_cache_wins() {
        let default = resolve_settings(
            &args(&[]),
            CacheEnv::default(),
            "/store",
            CacheUse::Development,
        )
        .expect("default settings");
        assert!(!default.enabled);
        assert_eq!(default.scope, CacheScope::Downloads);

        let asked = resolve_settings(
            &args(&["--cache"]),
            CacheEnv::default(),
            "/store",
            CacheUse::Development,
        )
        .expect("enabled settings");
        assert!(asked.enabled);

        let env_enabled = resolve_settings(
            &args(&[]),
            CacheEnv {
                enabled: Some("1"),
                store_root: None,
            },
            "/store",
            CacheUse::Development,
        )
        .expect("env settings");
        assert!(env_enabled.enabled);

        let refused = resolve_settings(
            &args(&["--cache", "--no-cache"]),
            CacheEnv {
                enabled: Some("1"),
                store_root: None,
            },
            "/store",
            CacheUse::Development,
        )
        .expect("refused settings");
        assert!(!refused.enabled);
    }

    #[test]
    fn verification_never_enables_the_cache_even_when_asked() {
        let settings = resolve_settings(
            &args(&["--cache", "--cache-target"]),
            CacheEnv {
                enabled: Some("1"),
                store_root: None,
            },
            "/store",
            CacheUse::Verification,
        )
        .expect("verification settings");
        assert!(!settings.enabled);
    }

    #[test]
    fn store_root_and_quota_follow_flag_then_environment_then_default() {
        let from_default = resolve_settings(
            &args(&["--cache"]),
            CacheEnv::default(),
            "/default",
            CacheUse::Development,
        )
        .expect("default store");
        assert_eq!(from_default.store_root, "/default");
        assert_eq!(from_default.quota_bytes, DEFAULT_QUOTA_BYTES);

        let from_env = resolve_settings(
            &args(&["--cache", "--cache-quota-mib", "7"]),
            CacheEnv {
                enabled: None,
                store_root: Some("/env"),
            },
            "/default",
            CacheUse::Development,
        )
        .expect("env store");
        assert_eq!(from_env.store_root, "/env");
        assert_eq!(from_env.quota_bytes, 7 * MIB);

        let from_flag = resolve_settings(
            &args(&["--cache", "--cache-dir", "/flag"]),
            CacheEnv {
                enabled: None,
                store_root: Some("/env"),
            },
            "/default",
            CacheUse::Development,
        )
        .expect("flag store");
        assert_eq!(from_flag.store_root, "/flag");
    }

    #[test]
    fn an_invalid_or_zero_quota_is_refused() {
        for raw in ["0", "-1", "lots"] {
            let resolved = resolve_settings(
                &args(&["--cache", "--cache-quota-mib", raw]),
                CacheEnv::default(),
                "/store",
                CacheUse::Development,
            );
            assert_eq!(
                resolved,
                Err(CacheSettingsError::InvalidQuota),
                "quota {raw:?}"
            );
        }
    }

    #[test]
    fn target_caching_is_a_separate_opt_in() {
        let downloads = resolve_settings(
            &args(&["--cache"]),
            CacheEnv::default(),
            "/store",
            CacheUse::Development,
        )
        .expect("downloads");
        let outputs = resolve_settings(
            &args(&["--cache", "--cache-target"]),
            CacheEnv::default(),
            "/store",
            CacheUse::Development,
        )
        .expect("outputs");
        assert_eq!(downloads.scope, CacheScope::Downloads);
        assert_eq!(outputs.scope, CacheScope::BuildOutputs);
        assert_ne!(downloads.scope, outputs.scope);
    }

    #[test]
    fn the_key_separates_project_image_toolchain_lockfile_and_scope() {
        let base = identity("project-a", "rust:1@sha256:aaa", "lock-a");
        assert_eq!(
            base.key(),
            identity("project-a", "rust:1@sha256:aaa", "lock-a").key()
        );

        let variants = [
            identity("project-b", "rust:1@sha256:aaa", "lock-a"),
            identity("project-a", "rust:1@sha256:bbb", "lock-a"),
            BuildCacheIdentity::new(
                "jvm",
                "rust:1@sha256:aaa",
                "rust 1.100.0",
                "lock-a",
                "project-a",
                CacheScope::Downloads,
            ),
            BuildCacheIdentity::new(
                "rust",
                "rust:1@sha256:aaa",
                "rust 2.0.0",
                "lock-a",
                "project-a",
                CacheScope::Downloads,
            ),
            identity("project-a", "rust:1@sha256:aaa", "lock-b"),
            BuildCacheIdentity::new(
                "rust",
                "rust:1@sha256:aaa",
                "rust 1.100.0",
                "lock-a",
                "project-a",
                CacheScope::BuildOutputs,
            ),
        ];
        for variant in variants {
            assert_ne!(
                base.key(),
                variant.key(),
                "identity {variant:?} shares a key"
            );
        }
    }

    #[test]
    fn the_key_is_length_prefixed_so_concatenation_cannot_collide() {
        let left =
            BuildCacheIdentity::new("ab", "c", "tool", "lock", "project", CacheScope::Downloads);
        let right =
            BuildCacheIdentity::new("a", "bc", "tool", "lock", "project", CacheScope::Downloads);
        assert_ne!(left.key(), right.key());
        assert_eq!(left.key().len(), 64);
    }

    #[test]
    fn the_lockfile_digest_is_order_independent_and_content_sensitive() {
        let a = lockfile_digest(&[("Cargo.lock", b"one"), ("flake.lock", b"two")]);
        let b = lockfile_digest(&[("flake.lock", b"two"), ("Cargo.lock", b"one")]);
        assert_eq!(a, b);
        assert_ne!(
            a,
            lockfile_digest(&[("Cargo.lock", b"one"), ("flake.lock", b"2")])
        );
        assert_eq!(lockfile_digest(&[]), "none");
    }

    #[test]
    fn the_scope_names_the_directory_it_archives() {
        assert_eq!(scope_dir("rust", CacheScope::Downloads), Some("cargo"));
        assert_eq!(scope_dir("jvm", CacheScope::Downloads), None);
        assert_eq!(scope_dir("rust", CacheScope::BuildOutputs), Some(""));
        assert_eq!(scope_dir("jvm", CacheScope::BuildOutputs), Some(""));
    }

    #[test]
    fn store_paths_are_derived_from_the_hex_key() {
        let key = "a".repeat(64);
        assert_eq!(archive_name(&key), format!("{key}.tar.gz"));
        assert_eq!(
            archive_path("/store/", &key),
            format!("/store/{ENTRIES_DIR}/{key}.tar.gz")
        );
        assert_eq!(manifest_path("/store/"), format!("/store/{MANIFEST_FILE}"));
    }

    #[test]
    fn a_recorded_entry_replaces_the_previous_one() {
        let mut manifest = CacheManifest::default();
        assert!(manifest.is_current());
        manifest.record(entry("k", 10, 1));
        manifest.record(entry("k", 20, 2));
        assert_eq!(manifest.entries.len(), 1);
        assert_eq!(manifest.find("k").map(|e| e.size_bytes), Some(20));
        manifest.remove("k");
        assert!(manifest.entries.is_empty());
        assert_eq!(manifest.total_bytes(), 0);
    }

    #[test]
    fn a_manifest_of_another_version_is_not_current() {
        let stale = CacheManifest {
            version: CACHE_FORMAT_VERSION + 1,
            entries: vec![],
        };
        assert!(!stale.is_current());
    }

    #[test]
    fn only_a_digest_matching_entry_for_this_key_is_verified() {
        let stored = entry("k", 10, 1);
        assert_eq!(
            restore_verdict(Some(&stored), "k", "digest-of-k"),
            RestoreVerdict::Verified
        );
        assert!(matches!(
            restore_verdict(Some(&stored), "k", "tampered"),
            RestoreVerdict::RejectedCorrupted { .. }
        ));
        assert!(matches!(
            restore_verdict(Some(&stored), "other", "digest-of-k"),
            RestoreVerdict::RejectedCorrupted { .. }
        ));
        assert_eq!(
            restore_verdict(None, "k", "digest-of-k"),
            RestoreVerdict::ColdMiss
        );
    }

    #[test]
    fn eviction_is_oldest_first_and_never_evicts_the_entry_just_stored() {
        let entries = [
            entry("oldest", 40, 1),
            entry("middle", 40, 2),
            entry("newest", 40, 3),
        ];
        assert!(plan_eviction(&entries, 200, "newest").is_empty());

        // 120 bytes, 100-byte quota: evict the oldest until it fits.
        assert_eq!(plan_eviction(&entries, 100, "newest"), vec!["oldest"]);
        // A tighter quota evicts the two oldest, never `keep`.
        assert_eq!(
            plan_eviction(&entries, 50, "newest"),
            vec!["oldest", "middle"]
        );
        // The entry just stored accounts for itself only; the plan stays
        // deterministic even when the clock ties.
        let tied = [entry("b", 60, 1), entry("a", 60, 1)];
        assert_eq!(plan_eviction(&tied, 60, "b"), vec!["a"]);
    }

    #[test]
    fn an_archive_larger_than_the_whole_quota_is_refused() {
        assert!(fits_quota(100, 100));
        assert!(!fits_quota(101, 100));
    }

    #[test]
    fn archives_no_entry_describes_are_reported_for_removal() {
        let referenced = vec!["k1".to_string(), "k2".to_string()];
        let present = vec![
            archive_name("k1"),
            archive_name("k2"),
            "orphan.tar.gz".to_string(),
            "garbage".to_string(),
        ];
        assert_eq!(
            unreferenced_archives(&referenced, &present),
            vec!["orphan.tar.gz".to_string(), "garbage".to_string()]
        );
    }
}
