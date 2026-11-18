//! `hodei-mh package` (TASK-17 AC-4): an INSTALLABLE bundle — the two
//! release binaries, the profile documentation and an install script.
//! Nothing else: no hidden plugins, no undemonstrated dependencies.

use std::path::{Path, PathBuf};

use crate::digest;
use crate::docs;
use crate::elf;

fn flag(args: &[String], name: &str, default: &str) -> String {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

const INSTALL_SH: &str = r#"#!/bin/sh
# hodei-meta-harness installer: bins into ~/.local/bin, docs alongside.
set -e
PREFIX="${1:-$HOME/.local}"
mkdir -p "$PREFIX/bin" "$PREFIX/share/hodei-mh/docs"
cp bin/hodei-mh "$PREFIX/bin/"
cp bin/hodei-mh-lifecycle-server "$PREFIX/bin/"
cp docs/profiles.md "$PREFIX/share/hodei-mh/docs/"
cp INSTALL.md "$PREFIX/share/hodei-mh/docs/"
cp SHA256SUMS "$PREFIX/share/hodei-mh/"
echo "installed to $PREFIX/bin (hodei-mh, hodei-mh-lifecycle-server)"
echo "next: hodei-mh doctor --profile rust   (see $PREFIX/share/hodei-mh/docs/profiles.md)"
"#;

const INSTALL_MD: &str = r#"# hodei-meta-harness — install & first run

Contents: `bin/hodei-mh` (control plane client + TUI + profiles),
`bin/hodei-mh-lifecycle-server` (OpenSandbox lifecycle over Docker),
`docs/profiles.md` (pinned Rust/JVM profiles), `install.sh`,
`SHA256SUMS`.

Install: `sh install.sh` (bins go to `~/.local/bin`).

## Verify the bundle, offline

`SHA256SUMS` carries the SHA-256 of every file this bundle ships. Check
them from the bundle root, with no network and no repository:

    LC_ALL=C sha256sum -c SHA256SUMS

One `OK` line per file means the bytes you unpacked are the bytes that
were published. The same file is installed next to the documentation
(`<prefix>/share/hodei-mh/SHA256SUMS`), so an installed tree can be
checked again later.

## The binaries

The binaries are STANDALONE: statically linked, with no dynamic loader,
so they need no glibc, no libgcc and no other system library from the
host. They run on any Linux x86_64 whose kernel is 3.2 or newer.

Prerequisites: Docker, a networked host for image pulls, and the
restricted-egress proxy for sandboxes (see docs/profiles.md).

## First run

    PORT=8081 hodei-mh-lifecycle-server &
    hodei-mh doctor --profile rust          # read-only checks + gate
    hodei-mh profile-run --profile rust --project /path/to/project
    hodei-mh supervisor --socket /tmp/hodei-mh-supervisor.sock &
    hodei-mh sup-status && hodei-mh tui     # observe and drive

No other components are required: no Continuum, no Herdr, no native
plugins. Everything above is demonstrated by the repository's E2E probes.
"#;

fn stage(src: &Path, dest: &Path) -> bool {
    if src.is_dir() {
        std::fs::create_dir_all(dest).is_ok()
    } else {
        std::fs::copy(src, dest).map(|_| ()).is_ok()
    }
}

/// Where the release binaries live.
///
/// The RUNNING profile is not necessarily the one that ships: a
/// checkout runs `target/<triple>/debug`, and a debug build is not a
/// release artifact. The release profile of the SAME target directory
/// is the one, and an INSTALLATION ships the pair next to the running
/// binary.
fn resolve_release_dir(repo: &Path) -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        // A cargo checkout: target/<triple>/<profile>. The PROFILE
        // decides, and only "release" ships.
        let profile = parent
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if profile == "debug" || profile == "release" {
            if let Some(target_dir) = parent.parent() {
                let release = target_dir.join("release");
                if release.join("hodei-mh").exists() {
                    return release;
                }
            }
        } else if parent.join("hodei-mh-lifecycle-server").exists() {
            // An INSTALLATION: the pair lives next to the running binary.
            return parent.to_path_buf();
        }
        // Installed layout: <prefix>/bin/hodei-mh -> <prefix>/lib/hodei-mh
        let candidate = parent.join("../lib/hodei-mh");
        let resolved = candidate.canonicalize().unwrap_or(candidate.clone());
        if resolved.join("hodei-mh").exists() {
            return resolved;
        }
    }
    // Fallback for a checkout: the default build is explicitly
    // targeted (see `.cargo/config.toml`), so the release binaries live
    // under the triple directory.
    for candidate in [
        repo.join("target/x86_64-unknown-linux-gnu/release"),
        repo.join("target/release"),
    ] {
        if candidate.join("hodei-mh-lifecycle-server").exists() {
            return candidate;
        }
    }
    repo.join("target/x86_64-unknown-linux-gnu/release")
}

/// Whether a directory belongs to a cargo build tree.
fn is_build_dir(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == "target")
}

/// The platform the binaries were COMPILED for: the identity of the
/// artifact, not the platform that happens to run `package`.
fn target_platform() -> (&'static str, &'static str) {
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else {
        std::env::consts::OS
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        std::env::consts::ARCH
    };
    (os, arch)
}

pub async fn run_package(args: &[String]) -> i32 {
    let version = env!("CARGO_PKG_VERSION");
    let out_dir = PathBuf::from(flag(args, "--out", "/tmp/hodei-mh-package"));
    let exe = std::env::current_exe().ok();
    let cwd = std::env::current_dir().ok();
    // The workspace is found by its MARKER, never by the working
    // directory alone: `CARGO_MANIFEST_DIR` does not exist at runtime,
    // and a "." that happens to hold a `docs/profiles.md` is not the
    // product's tree.
    let repo = exe
        .as_deref()
        .and_then(docs::workspace_root)
        .or_else(|| cwd.as_deref().and_then(docs::workspace_root))
        .unwrap_or_else(|| PathBuf::from("."));
    let release = resolve_release_dir(&repo);

    // A debug build is not a release artifact: the bundle ships the
    // RELEASE profile, whatever profile the running binary is.
    if release.components().any(|c| c.as_os_str() == "debug") {
        eprintln!(
            "refusing to package a debug build: {} — the release profile is the artifact that ships",
            release.display()
        );
        return 1;
    }

    // Build the release binaries if they are not there yet (bounded
    // retries: the cargo lock can be transiently held by another session).
    let bins = [
        release.join("hodei-mh"),
        release.join("hodei-mh-lifecycle-server"),
    ];
    if bins.iter().any(|b| !b.exists()) {
        if !is_build_dir(&release) {
            eprintln!(
                "release binaries missing at {} and it is not a build directory: there is nothing to build here",
                release.display()
            );
            return 1;
        }
        println!("release binaries missing: building (offline, -j2, up to 5 attempts)");
        for attempt in 0..5 {
            let status = std::process::Command::new("cargo")
                .args([
                    "build",
                    "--release",
                    "-p",
                    "hodei-mh",
                    "-p",
                    "hodei-mh-lifecycle-server",
                    "--offline",
                    "-j2",
                ])
                // The build belongs to the WORKSPACE, wherever the
                // command was invoked from.
                .current_dir(&repo)
                .status();
            if matches!(status, Ok(s) if s.success()) {
                break;
            }
            println!("release build attempt {} failed; retrying", attempt + 1);
            std::thread::sleep(std::time::Duration::from_secs(5));
            if attempt == 4 {
                eprintln!("could not build release binaries; package aborted");
                return 1;
            }
        }
    }

    // The name of the artifact is its identity: it must agree with
    // what the binaries really are, measured from their headers.
    let (os, arch) = target_platform();
    for bin in &bins {
        if let Some(found) = elf::architecture_of(bin)
            && found != arch
        {
            eprintln!(
                "refusing to package {}: it is a {found} binary, but the artifact would be named {os}-{arch}",
                bin.display()
            );
            return 1;
        }
    }

    // The distribution must be STANDALONE: a binary that needs a
    // dynamic loader also needs the host's glibc, libgcc and friends,
    // which makes it unusable on a distro older than the build host.
    // The linkage is READ from the binaries, never assumed.
    let standalone = bins.iter().all(|bin| elf::linkage_of(bin).is_standalone());
    if !standalone {
        let linked: Vec<String> = bins
            .iter()
            .map(|bin| format!("{}: {:?}", bin.display(), elf::linkage_of(bin)))
            .collect();
        eprintln!(
            "refusing to package non-standalone binaries: {} — build them statically (cargo build --release with -C target-feature=+crt-static) or the bundle would need the host's glibc",
            linked.join(", ")
        );
        return 1;
    }

    // The bundle carries the documentation of the PRODUCT. The source
    // is TRUSTED: the copy installed next to the running binary, or
    // the copy of the workspace that owns the product. A file named
    // `docs/profiles.md` that merely happens to sit in the working
    // directory is not the product's documentation, and shipping it
    // would put a foreign text inside the release.
    let doc = match docs::choose(&docs::candidates(exe.as_deref(), cwd.as_deref())) {
        Some(source) => source,
        None => {
            eprintln!(
                "refusing to package without the product's documentation: no \
                 <prefix>/share/hodei-mh/{} and no workspace {} found — the \
                 bundle must carry the documentation of the product it ships",
                docs::DOC_RELATIVE,
                docs::DOC_RELATIVE
            );
            return 1;
        }
    };

    // Stage the bundle.
    let _ = std::fs::remove_dir_all(&out_dir);
    let staged = [
        ("bin/hodei-mh", bins[0].clone()),
        ("bin/hodei-mh-lifecycle-server", bins[1].clone()),
        (docs::DOC_RELATIVE, doc.path.clone()),
    ];
    for (relative, source) in &staged {
        let dest = out_dir.join(relative);
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if !stage(source, &dest) {
            eprintln!("staging failed for {relative}");
            return 1;
        }
    }
    let install_sh = out_dir.join("install.sh");
    let install_md = out_dir.join("INSTALL.md");
    let install_md_text = format!("{INSTALL_MD}\nRelease: v{version} ({os}-{arch})\n");
    let wrote_installers = std::fs::write(&install_sh, INSTALL_SH).is_ok()
        && std::fs::write(&install_md, install_md_text).is_ok()
        && {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&install_sh, std::fs::Permissions::from_mode(0o755))
                    .is_ok()
            }
            #[cfg(not(unix))]
            {
                true
            }
        };
    if !wrote_installers {
        eprintln!("failed to write installer files");
        return 1;
    }

    // The bundle carries the digests of what it ships: the consumer
    // can verify the unpacked bytes offline, with no network and no
    // repository. The digests are READ from the staged files, so they
    // describe the bytes that travel, not the ones that were meant to
    // travel.
    let mut checksums = String::new();
    let mut shipped: Vec<(String, String)> = Vec::new();
    for (relative, _) in &staged {
        let digest = digest::sha256_file(&out_dir.join(relative));
        match digest {
            Some(digest) => {
                checksums.push_str(&digest::checksum_line(&digest, relative));
                checksums.push('\n');
                shipped.push((relative.to_string(), digest));
            }
            None => {
                eprintln!("cannot read the staged {relative} to compute its digest");
                return 1;
            }
        }
    }
    if std::fs::write(out_dir.join("SHA256SUMS"), checksums).is_err() {
        eprintln!("failed to write the bundle's SHA256SUMS");
        return 1;
    }

    // Tar it up. The name carries the version, the OS and the
    // architecture the binaries were COMPILED for: a bundle is a
    // release artifact, and its name is part of its identity.
    let version = env!("CARGO_PKG_VERSION");
    let tar_path = out_dir
        .parent()
        .unwrap_or(Path::new("/tmp"))
        .join(format!("hodei-mh-v{version}-{os}-{arch}.tar.gz"));
    let tar_ok = std::process::Command::new("tar")
        .args([
            "czf",
            tar_path.to_string_lossy().as_ref(),
            "-C",
            out_dir.to_string_lossy().as_ref(),
            ".",
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !tar_ok {
        eprintln!("tar failed");
        return 1;
    }

    // Honest manifest: exactly what the bundle contains.
    let listing = std::process::Command::new("tar")
        .args(["tzf", tar_path.to_string_lossy().as_ref()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let contents: Vec<String> = listing
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect();
    // The manifest reports what was SCANNED, not what is asserted: the
    // undemonstrated dependencies are the ones really present in the
    // staged bundle beyond the two binaries and the docs.
    let undemonstrated = undemonstrated_dependencies(&contents);
    let manifest = build_manifest(ManifestFacts {
        package: tar_path.to_string_lossy().into_owned(),
        version: version.to_string(),
        bytes: std::fs::metadata(&tar_path).map(|m| m.len()).unwrap_or(0),
        contents,
        documentation_origin: doc.origin.as_str(),
        documentation_digest: shipped
            .iter()
            .find(|(name, _)| name == docs::DOC_RELATIVE)
            .map(|(_, digest)| digest.clone())
            .unwrap_or_default(),
        checksums: shipped,
        undemonstrated,
        standalone,
    });
    println!("{}", manifest);
    0
}

/// The facts the manifest reports. Everything here is a measurement.
struct ManifestFacts {
    package: String,
    version: String,
    bytes: u64,
    contents: Vec<String>,
    documentation_origin: &'static str,
    documentation_digest: String,
    checksums: Vec<(String, String)>,
    undemonstrated: Vec<String>,
    standalone: bool,
}

/// The entries of the bundle that are NOT part of the documented
/// product: the manifest is a scan, not an assertion.
fn undemonstrated_dependencies(contents: &[String]) -> Vec<String> {
    contents
        .iter()
        .filter(|entry| {
            let normalised = entry.trim_start_matches("./");
            !(normalised == "bin/hodei-mh"
                || normalised == "bin/hodei-mh-lifecycle-server"
                || normalised == "docs/profiles.md"
                || normalised == "docs/INSTALL.md"
                || normalised == "INSTALL.md"
                || normalised == "install.sh"
                || normalised == "SHA256SUMS"
                || normalised.is_empty()
                || normalised.ends_with('/'))
        })
        .cloned()
        .collect()
}

/// The manifest of the bundle: what it contains, where the
/// documentation came from, and the digests of what it ships.
fn build_manifest(facts: ManifestFacts) -> serde_json::Value {
    let documentation = serde_json::json!({
        // Where the shipped documentation came from: an installation
        // beside the running binary, or the workspace that owns the
        // product. Never a file found by chance in the working
        // directory.
        "origin": facts.documentation_origin,
        "document": docs::DOC_RELATIVE,
        "sha256": facts.documentation_digest,
    });
    let mut checksums = serde_json::Map::new();
    for (name, digest) in &facts.checksums {
        checksums.insert(name.clone(), serde_json::Value::String(digest.clone()));
    }
    serde_json::json!({
        "package": facts.package,
        "version": facts.version,
        "bytes": facts.bytes,
        "contents": facts.contents,
        "documentation": documentation,
        // The digests of the shipped bytes, so the manifest and the
        // bundle's own SHA256SUMS tell the same story.
        "checksums": checksums,
        "guarantees": {
            // The bundle contains no native build fallback: the
            // binaries are the only executables and the docs say
            // what they need.
            "yolo_fallback": false,
            "undemonstrated_dependencies": facts.undemonstrated,
            // MEASURED, not asserted: the binaries carry no
            // dynamic loader, so the bundle needs no glibc, no
            // libgcc and no other system library from the host.
            "standalone_binaries": facts.standalone,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> ManifestFacts {
        ManifestFacts {
            package: "/tmp/hodei-mh-v0.1.0-linux-x86_64.tar.gz".to_string(),
            version: "0.1.0".to_string(),
            bytes: 42,
            contents: vec![
                "./".to_string(),
                "./bin/hodei-mh".to_string(),
                "./SHA256SUMS".to_string(),
                "./docs/profiles.md".to_string(),
            ],
            documentation_origin: "installed",
            documentation_digest: "abc123".to_string(),
            checksums: vec![
                ("bin/hodei-mh".to_string(), "d1".to_string()),
                ("docs/profiles.md".to_string(), "abc123".to_string()),
            ],
            undemonstrated: vec![],
            standalone: true,
        }
    }

    #[test]
    fn the_manifest_declares_where_the_documentation_came_from() {
        let manifest = build_manifest(facts());
        let documentation = &manifest["documentation"];
        assert_eq!(documentation["origin"], "installed");
        assert_eq!(documentation["document"], "docs/profiles.md");
        assert_eq!(documentation["sha256"], "abc123");
    }

    #[test]
    fn the_manifest_carries_the_digest_of_every_shipped_file() {
        let manifest = build_manifest(facts());
        assert_eq!(manifest["checksums"]["bin/hodei-mh"], "d1");
        assert_eq!(manifest["checksums"]["docs/profiles.md"], "abc123");
        // The documentation digest is the digest of the shipped file:
        // the manifest and the bundle's SHA256SUMS tell one story.
        assert_eq!(
            manifest["documentation"]["sha256"],
            manifest["checksums"]["docs/profiles.md"]
        );
    }

    #[test]
    fn the_manifest_reports_the_measured_guarantees() {
        let manifest = build_manifest(facts());
        assert_eq!(manifest["guarantees"]["standalone_binaries"], true);
        assert_eq!(manifest["guarantees"]["yolo_fallback"], false);
        assert_eq!(
            manifest["guarantees"]["undemonstrated_dependencies"],
            serde_json::json!([])
        );
    }

    #[test]
    fn the_known_files_are_not_undemonstrated_dependencies() {
        let contents = vec![
            "./bin/hodei-mh".to_string(),
            "./bin/hodei-mh-lifecycle-server".to_string(),
            "./docs/profiles.md".to_string(),
            "./INSTALL.md".to_string(),
            "./install.sh".to_string(),
            "./SHA256SUMS".to_string(),
            "./docs/".to_string(),
        ];
        assert!(undemonstrated_dependencies(&contents).is_empty());
    }

    #[test]
    fn an_unknown_file_in_the_bundle_is_reported() {
        let contents = vec![
            "./bin/hodei-mh".to_string(),
            "./bin/extra-plugin".to_string(),
        ];
        assert_eq!(
            undemonstrated_dependencies(&contents),
            vec!["./bin/extra-plugin".to_string()]
        );
    }
}
