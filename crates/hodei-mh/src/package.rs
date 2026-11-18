//! `hodei-mh package` (TASK-17 AC-4): an INSTALLABLE bundle — the two
//! release binaries, the profile documentation and an install script.
//! Nothing else: no hidden plugins, no undemonstrated dependencies.

use std::path::{Path, PathBuf};

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
echo "installed to $PREFIX/bin (hodei-mh, hodei-mh-lifecycle-server)"
echo "next: hodei-mh doctor --profile rust   (see $PREFIX/share/hodei-mh/docs/profiles.md)"
"#;

const INSTALL_MD: &str = r#"# hodei-meta-harness — install & first run

Contents: `bin/hodei-mh` (control plane client + TUI + profiles),
`bin/hodei-mh-lifecycle-server` (OpenSandbox lifecycle over Docker),
`docs/profiles.md` (pinned Rust/JVM profiles), `install.sh`.

Install: `sh install.sh` (bins go to `~/.local/bin`).

The binaries are STANDALONE: statically linked, with no dynamic loader,
so they need no glibc, no libgcc and no other system library from the
host. They run on any Linux x86_64 whose kernel is 3.2 or newer.

Prerequisites: Docker, a networked host for image pulls, and the
restricted-egress proxy for sandboxes (see docs/profiles.md).

First run:

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

/// Where the release binaries live. An INSTALLED binary must not look
/// for them on the machine that compiled it: the path is resolved from
/// the running executable, with the repository as an explicit fallback.
fn resolve_release_dir(repo: &Path) -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        // The binaries NEXT TO the running one are the pair it was
        // built with — in the repo (target/<triple>/release) and in
        // an installation (<prefix>/bin) alike. This is the rule
        // that ships the SAME build that is running.
        if parent.join("hodei-mh-lifecycle-server").exists() {
            return parent.to_path_buf();
        }
        // Installed layout: <prefix>/bin/hodei-mh -> <prefix>/lib/hodei-mh
        for candidate in [
            parent.join("../lib/hodei-mh"),
            parent.join("../target/x86_64-unknown-linux-gnu/release"),
            parent.join("../target/release"),
            parent.join("target/release"),
        ] {
            let resolved = candidate.canonicalize().unwrap_or(candidate.clone());
            if resolved.join("hodei-mh").exists() {
                return resolved;
            }
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
    repo.join("target/release")
}

pub async fn run_package(args: &[String]) -> i32 {
    let version = env!("CARGO_PKG_VERSION");
    let out_dir = PathBuf::from(flag(args, "--out", "/tmp/hodei-mh-package"));
    let repo =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string()))
            .parent()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
    let release = resolve_release_dir(&repo);

    // Build the release binaries if they are not there yet (bounded
    // retries: the cargo lock can be transiently held by another session).
    let bins = [
        release.join("hodei-mh"),
        release.join("hodei-mh-lifecycle-server"),
    ];
    if bins.iter().any(|b| !b.exists()) {
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

    // Stage the bundle.
    let _ = std::fs::remove_dir_all(&out_dir);
    let staged = [
        ("bin/hodei-mh", bins[0].clone()),
        ("bin/hodei-mh-lifecycle-server", bins[1].clone()),
        ("docs/profiles.md", repo.join("docs/profiles.md")),
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
    let install_md_text = format!(
        "{INSTALL_MD}\nRelease: v{version} ({}-{})\n",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
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

    // Tar it up. The name carries the version, the OS and the
    // architecture: a bundle is a release artifact, and its name is
    // part of its identity.
    let version = env!("CARGO_PKG_VERSION");
    let tar_path = out_dir.parent().unwrap_or(Path::new("/tmp")).join(format!(
        "hodei-mh-v{version}-{}-{}.tar.gz",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
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
    let mut undemonstrated: Vec<String> = Vec::new();
    for entry in &contents {
        let normalised = entry.trim_start_matches("./");
        let known = normalised == "bin/hodei-mh"
            || normalised == "bin/hodei-mh-lifecycle-server"
            || normalised == "docs/profiles.md"
            || normalised == "docs/INSTALL.md"
            || normalised == "INSTALL.md"
            || normalised == "install.sh"
            || normalised.is_empty()
            || normalised.ends_with('/');
        if !known {
            undemonstrated.push(entry.clone());
        }
    }
    println!(
        "{}",
        serde_json::json!({
            "package": tar_path.to_string_lossy(),
            "version": version,
            "bytes": std::fs::metadata(&tar_path).map(|m| m.len()).unwrap_or(0),
            "contents": contents,
            "guarantees": {
                // The bundle contains no native build fallback: the
                // binaries are the only executables and the docs say
                // what they need.
                "yolo_fallback": false,
                "undemonstrated_dependencies": undemonstrated,
                // MEASURED, not asserted: the binaries carry no
                // dynamic loader, so the bundle needs no glibc, no
                // libgcc and no other system library from the host.
                "standalone_binaries": standalone,
            },
        })
    );
    0
}
