#!/bin/sh
# hodei-meta-harness bootstrap installer — the `curl | sh` entry point.
#
#   curl -fsSL https://github.com/Rubentxu/hodei-meta-harness/releases/latest/download/install.sh | sh
#
# Downloads the platform bundle and the release SHA256SUMS into a private
# temporary directory, verifies the bundle's digest BEFORE touching the
# install prefix, unpacks it, verifies every file inside the bundle, and
# only then runs the bundle's own installer.
#
# Configuration:
#   VERSION=<x.y.z>  or the first argument  (default: the latest release)
#   PREFIX=<dir>     install prefix          (default: $HOME/.local)
#   REPO=<owner/repo> release repository     (default: Rubentxu/hodei-meta-harness)
#   BASE_URL=<url>    release download base  (default: https://github.com/REPO/releases)
#   MIRROR_DIR=<dir>  local stand-in for the release download tree, as
#                     <dir>/<tag>/ (testing only: a directory cannot 404
#                     a stale digest the way the real endpoint does)
#   HODEI_MH_OS / HODEI_MH_ARCH  override platform detection (testing only)
set -eu

REPO="${REPO:-Rubentxu/hodei-meta-harness}"
BASE_URL="${BASE_URL:-https://github.com/${REPO}/releases}"
PREFIX="${PREFIX:-$HOME/.local}"
RUN_DIR=""

die() {
    printf 'hodei-mh install: %s\n' "$1" >&2
    exit 1
}

cleanup() {
    if [ -n "$RUN_DIR" ] && [ -d "$RUN_DIR" ]; then
        rm -rf "$RUN_DIR"
    fi
}
trap cleanup EXIT hup int term

require_cmd() {
    command -v "$1" >/dev/null 2>&1 || die \
        "$1 is required but not found on PATH — install it and rerun"
}

# One fetch path for the real endpoint and the test mirror. The mirror
# mirrors the URL tail under <dir>/<ref>/: the `latest` alias resolves
# to the real tag, so the mirror carries it under `latest/`.
fetch() {
    if [ -n "${MIRROR_DIR:-}" ]; then
        cp "${MIRROR_DIR}/${ref}/$(printf '%s' "$1" | sed "s|.*/||")" "$2" \
            2>/dev/null || return 1
    else
        curl -fsSL -o "$2" "$1"
    fi
}

# The SHA-256 tool, normalised to sha256sum-style output.
sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        die "no sha256sum or shasum on PATH — cannot verify the download"
    fi
}

# The digest recorded in a SHA256SUMS file for one exact filename, or
# empty when the file does not carry that asset.
recorded_digest() {
    awk -v name="$2" '$2 == name { print $1; exit }' "$1"
}

verify_digest() {
    actual=$(sha256_of "$2")
    if [ "$actual" != "$1" ]; then
        printf 'hodei-mh install: checksum mismatch for %s\n' "$3" >&2
        printf '  expected %s\n  actual   %s\n' "$1" "$actual" >&2
        printf '  the download is corrupted or tampered with; nothing was installed\n' >&2
        exit 1
    fi
}

os=$(uname -s)
case "$os" in
    Linux) os="linux" ;;
    *) die "unsupported operating system: $os — this release ships Linux bundles only" ;;
esac
case "$(uname -m)" in
    x86_64 | amd64) arch="x86_64" ;;
    aarch64 | arm64) arch="aarch64" ;;
    *) die "unsupported CPU architecture: $(uname -m) — this release ships x86_64 and aarch64 bundles" ;;
esac
# Test hooks only: the production path detects the host.
[ -n "${HODEI_MH_OS:-}" ] && os="$HODEI_MH_OS"
[ -n "${HODEI_MH_ARCH:-}" ] && arch="$HODEI_MH_ARCH"
# Fail closed on the platform BEFORE any download: a released platform
# the host cannot run is an actionable refusal, not a 404 hunt.
case "$os" in
    linux) ;;
    *) die "unsupported platform: $os-$arch — this release ships linux bundles only (x86_64, aarch64)" ;;
esac
case "$arch" in
    x86_64 | aarch64) ;;
    *) die "unsupported CPU architecture: $os-$arch — this release ships x86_64 and aarch64 bundles" ;;
esac

case "${1:-}" in
    -*) die "unknown option: $1 — pass a version as the argument, or set VERSION" ;;
esac
requested="${1:-${VERSION:-}}"
case "$requested" in
    '') display='latest'; ref='latest'; route='latest/download' ;;
    v*)  display="$requested";  ref="$requested";  route="download/$requested" ;;
    *)   display="v$requested"; ref="v$requested"; route="download/v$requested" ;;
esac
source_desc="${BASE_URL}/${route}"

require_cmd curl
require_cmd tar

work=$(mktemp -d "${TMPDIR:-/tmp}/hodei-mh-install.XXXXXX")
RUN_DIR="$work"

printf 'hodei-mh %s (%s-%s) -> %s\n' "$display" "$os" "$arch" "$PREFIX"

# 1. The release checksums: they authenticate the bundle. Under the
#    `latest` alias, SHA256SUMS is itself the source of the exact
#    versioned bundle name.
fetch "${source_desc}/SHA256SUMS" "$work/SHA256SUMS" \
    || die "cannot download SHA256SUMS from ${source_desc} — check the version and your network"
asset=$(awk -v os="$os" -v arch="$arch" \
    '$2 ~ "^hodei-mh-v[0-9].*-" os "-" arch "\\.tar\\.gz$" { print $2; exit }' \
    "$work/SHA256SUMS")
[ -n "$asset" ] || die \
    "the release SHA256SUMS has no ${os}-${arch} bundle line — this platform is not published for ${display}"
# The bundle name carries the concrete version: report it once known.
display=$(printf '%s' "$asset" | sed 's/^hodei-mh-//; s/-[a-z]*-.*$//')
printf 'resolved: hodei-mh %s\n' "$display"
bundle_want=$(recorded_digest "$work/SHA256SUMS" "$asset")

# 2. The bundle, verified against the release checksums. The install
#    prefix is untouched until this digest matches.
fetch "${source_desc}/${asset}" "$work/$asset" \
    || die "cannot download $asset from ${source_desc}"
verify_digest "$bundle_want" "$work/$asset" "$asset"

# 3. Unpack, then verify every file the bundle ships, before running the
#    bundle's own installer.
mkdir -p "$work/unpacked"
tar xzf "$work/$asset" -C "$work/unpacked" \
    || die "the downloaded $asset is not a readable tar.gz — nothing was installed"
cd "$work/unpacked"
[ -f install.sh ] || die "the bundle has no install.sh — refusing to improvise an installation"
[ -f SHA256SUMS ] || die "the bundle has no SHA256SUMS — refusing to install unverified files"
while read -r digest name; do
    [ -n "$digest" ] && [ -n "$name" ] || continue
    [ -f "$name" ] || die "the bundle is missing $name — nothing was installed"
    verify_digest "$digest" "$name" "$name"
done < SHA256SUMS

sh ./install.sh "$PREFIX"

printf 'hodei-mh installed. Next: hodei-mh doctor --profile rust\n'
case ":$PATH:" in
    *":$PREFIX/bin:"*) ;;
    *) printf '  add %s/bin to PATH to run hodei-mh directly\n' "$PREFIX" ;;
esac
