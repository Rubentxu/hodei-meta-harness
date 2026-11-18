#!/usr/bin/env bash
# The FAST local gate: the crate you touched and the crates that depend on
# it, and nothing else.
#
#   scripts/fast-check.sh                              # what changed vs the base
#   scripts/fast-check.sh crates/hodei-mh-verify/src/runner.rs
#   scripts/fast-check.sh --crate hodei-mh-verify
#   scripts/fast-check.sh --all                        # the whole workspace
#   scripts/fast-check.sh --packages hodei-mh,hodei-mh-workspace
#
# The gate NEVER turns a failure into a pass: every step propagates its
# exit code, nothing is allowed to fail open, and a step that cannot run
# is a failure, not a skip. A change that touches no compiled code is
# reported as such instead of being silently passed.
set -uo pipefail

CALLER_CWD="$(pwd)"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

BASE_REF="${BASE_REF:-}"
ALL=0
CRATE=""
PATHS=()
PACKAGES=""

usage() {
  sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-0}"
}

# Paths given on the command line belong to the caller's directory, not to
# the repository root.
absolute_path() {
  case "$1" in
    /*) printf '%s' "$1" ;;
    *) printf '%s' "$CALLER_CWD/$1" ;;
  esac
}

while [ $# -gt 0 ]; do
  case "$1" in
    --all) ALL=1; shift ;;
    --crate) CRATE="${2:-}"; shift 2 ;;
    --packages) PACKAGES="${2:-}"; shift 2 ;;
    --base) BASE_REF="${2:-}"; shift 2 ;;
    -h|--help) usage 0 ;;
    -*) echo "unknown option: $1" >&2; usage 2 ;;
    *) PATHS+=("$(absolute_path "$1")"); shift ;;
  esac
done

say() { printf '%s\n' "$*"; }
fail() { printf 'fast-check: %s\n' "$*" >&2; exit 1; }

cd "$REPO_ROOT" || exit 1

META_FILE="$(mktemp)"
CHANGED_FILE="$(mktemp)"
trap 'rm -f "$META_FILE" "$CHANGED_FILE"' EXIT

# ── The metadata of the workspace ────────────────────────────────────────────
cargo metadata --format-version 1 --no-deps > "$META_FILE" 2>/dev/null \
  || fail "cargo metadata failed: the workspace cannot be read"

# ── Which packages are affected ──────────────────────────────────────────────
# The classifier: a path belongs to the package that owns it, a change to a
# workspace-wide file reaches every crate, and a change outside the compiled
# code reaches none of them.
classify_paths() {
  python3 - "$META_FILE" "$CHANGED_FILE" <<'PY'
import json, os, sys

meta = json.load(open(sys.argv[1]))
paths = [p for p in open(sys.argv[2], "rb").read().split(b"\0") if p]
packages = {os.path.abspath(p["manifest_path"].rsplit("/Cargo.toml", 1)[0]): p["name"]
            for p in meta["packages"]}
every = set(packages.values())
root = os.path.abspath(meta["workspace_root"])

# A change to these files reaches every crate: the workspace manifest
# carries the shared dependency versions, the lock file pins them, and the
# cargo config pins the build target and the static link.
workspace_wide = {"Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo/config.toml"}

def owner_of(path):
    """The package a path belongs to, walking up to the nearest manifest."""
    directory = path if path in packages else os.path.dirname(path)
    while True:
        if directory in packages:
            return packages[directory]
        parent = os.path.dirname(directory)
        if parent == directory:
            return None
        directory = parent

affected = set()
for raw in paths:
    path = os.path.abspath(raw.decode())
    if not (path == root or path.startswith(root + os.sep)):
        affected |= every          # outside the repository: everything
        continue
    owner = owner_of(path)
    if owner:
        affected.add(owner)
        continue
    relative = os.path.relpath(path, root)
    if relative in workspace_wide:
        affected |= every
        continue
    # Anything else at the root (docs, scripts, CI) changes no compiled code.
print(" ".join(sorted(affected)))
PY
}

affected=""
if [ "$ALL" = "1" ]; then
  affected="$(python3 -c '
import json,sys
meta=json.load(open(sys.argv[1]))
print(" ".join(sorted(p["name"] for p in meta["packages"])))
' "$META_FILE")"
elif [ -n "$PACKAGES" ]; then
  affected="$(printf '%s' "$PACKAGES" | tr ',' ' ')"
elif [ -n "$CRATE" ]; then
  affected="$CRATE"
else
  if [ ${#PATHS[@]} -gt 0 ]; then
    printf '%s\0' "${PATHS[@]}" > "$CHANGED_FILE"
  else
    # What changed against the base: the merge base when there is one.
    if [ -z "$BASE_REF" ]; then
      for candidate in origin/main origin/master main; do
        if git rev-parse --verify --quiet "$candidate" >/dev/null; then BASE_REF="$candidate"; break; fi
      done
    fi
    if [ -z "$BASE_REF" ]; then
      fail "no base ref to compare against: pass --base <ref> or use --all"
    fi
    base_commit="$(git merge-base "$BASE_REF" HEAD 2>/dev/null)" \
      || fail "cannot resolve the merge base against $BASE_REF"
    # The WORKING TREE, not only the commits: the local gate covers what
    # is being written right now, committed or not.
    {
      git diff --name-only -z "$base_commit" --
      git ls-files --others --exclude-standard -z
    } > "$CHANGED_FILE"
  fi
  affected="$(classify_paths)"
fi

# ── The crates that depend on them (transitively) ────────────────────────────
# A change in a leaf crate can break the crates above it: the gate covers
# the ones that actually depend on the change, not the whole workspace.
if [ -z "$affected" ]; then
  say "fast-check: the change touches no compiled code — checking formatting only"
  status=0
  cargo fmt --all -- --check || { status=1; say "fast-check: fmt FAILED"; }
  if [ "$status" = "0" ]; then
    say "fast-check: PASS (formatting only)"
  else
    say "fast-check: FAIL"
  fi
  exit "$status"
fi

dependents="$(python3 - "$META_FILE" "$affected" <<'PY'
import json, sys

meta = json.load(open(sys.argv[1]))
affected = set(sys.argv[2].split())
names = {p["name"] for p in meta["packages"]}
direct = {}
for p in meta["packages"]:
    direct[p["name"]] = {d["name"] for d in p.get("dependencies", []) if d.get("name") in names}
# The reverse closure: whoever depends on an affected crate.
selected = set(affected)
grew = True
while grew:
    grew = False
    for name, deps in direct.items():
        if name in selected:
            continue
        if deps & selected:
            selected.add(name)
            grew = True
print(" ".join(sorted(selected)))
PY
)"

say "fast-check: affected   = $affected"
say "fast-check: with deps  = $dependents"

# cargo takes ONE package per -p: the flags are built one by one.
pkg_flags() {
  local name
  for name in $1; do
    printf -- "-p %s " "$name"
  done
}
PKG_FLAGS="$(pkg_flags "$dependents")"

# ── The gate ─────────────────────────────────────────────────────────────────
status=0

say ""
say "── fmt ─────────────────────────────────────────────────────────────"
cargo fmt --all -- --check || { status=1; say "fast-check: fmt FAILED"; }

say ""
say "── clippy (affected + dependents) ──────────────────────────────────"
# shellcheck disable=SC2086
cargo clippy $PKG_FLAGS --all-targets -- -D warnings \
  || { status=1; say "fast-check: clippy FAILED"; }

say ""
say "── tests (affected + dependents) ───────────────────────────────────"
# shellcheck disable=SC2086
cargo test $PKG_FLAGS \
  || { status=1; say "fast-check: tests FAILED"; }

say ""
if [ "$status" = "0" ]; then
  say "fast-check: PASS"
else
  say "fast-check: FAIL"
fi
exit "$status"
