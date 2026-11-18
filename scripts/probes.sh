#!/usr/bin/env bash
# The EXPENSIVE lane: the probes that need real infrastructure (Docker and
# the OpenSandbox lifecycle server).
#
#   scripts/probes.sh                # one round of the probes
#   scripts/probes.sh --rounds 3     # three consecutive rounds
#   scripts/probes.sh --probe profile-probe
#   scripts/probes.sh --preflight    # only check the prerequisites
#
# These probes are NOT part of the fast gate: they start sandboxes and run
# real builds. They are kept in an explicit lane so that the cost of the
# cheap checks never hides the cost of these, and so that a failure here is
# a failure, never a skipped check.
#
# The gate NEVER turns a failure into a pass: every probe must exit 0, and
# the lane fails if the host is left with residue.
#
# PREREQUISITES. The lane states what it needs and refuses to run without
# it, with the reason: the official Backlog.md CLI (the probes use the real
# tracker), Docker, and a restricted egress proxy (the profiles' gate
# refuses to build without one, and it does not fall back). A missing
# prerequisite fails the lane; it never degrades it in silence.
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT" || exit 1

PORT="${PORT:-8081}"
ROUNDS=1
PROBES="release-probe supervisor-probe profile-probe multiproject-probe verify-probe"
OUT="${OUT:-/tmp/hodei-probes}"
BIN="target/x86_64-unknown-linux-gnu/release/hodei-mh"
SERVER="target/x86_64-unknown-linux-gnu/release/hodei-mh-lifecycle-server"

# The restricted egress proxy the profiles require. The address is the one
# the probes use to reach it (DEFAULT_PROXY_URL in
# crates/hodei-mh/src/profile_run.rs): the gateway of the Docker bridge.
PROXY_HOST="${PROXY_HOST:-172.17.0.1}"
PROXY_PORT="${PROXY_PORT:-8888}"
PROXY_URL="http://${PROXY_HOST}:${PROXY_PORT}"

# The official Backlog.md CLI the probes use. The version is only reported:
# the lane needs the CLI, not a particular build of it.
BACKLOG_CLI_VERSION="${BACKLOG_CLI_VERSION:-1.55.3}"

# Preflight only: check the prerequisites and report, without running a
# probe and without starting the control plane.
PREFLIGHT=0

while [ $# -gt 0 ]; do
  case "$1" in
    --rounds) ROUNDS="${2:-1}"; shift 2 ;;
    --probe) PROBES="${2:-}"; shift 2 ;;
    --port) PORT="${2:-8081}"; shift 2 ;;
    --preflight) PREFLIGHT=1; shift ;;
    -h|--help) sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

mkdir -p "$OUT"
say() { printf '%s\n' "$*"; }

# The lane stops with a REASON. A missing prerequisite is never a skip.
die() {
  say "probes: $*" >&2
  exit 1
}

# Whether a TCP port answers, measured and not assumed.
tcp_reachable() {
  python3 - "$1" "$2" <<'PY'
import socket
import sys

sock = socket.socket()
sock.settimeout(2)
try:
    sock.connect((sys.argv[1], int(sys.argv[2])))
except OSError:
    sys.exit(1)
finally:
    sock.close()
PY
}

# The official Backlog.md CLI: the probes drive the REAL tracker, so
# without it there is nothing to probe.
require_backlog_cli() {
  command -v backlog >/dev/null 2>&1 || die \
    "the official Backlog.md CLI is not in PATH (the probes drive the real tracker): install it with 'npm install -g backlog.md@${BACKLOG_CLI_VERSION}' or put its bin directory in PATH"
  local version
  version="$(backlog --version 2>/dev/null | head -1)"
  [ -n "$version" ] || die "the Backlog.md CLI in PATH does not answer --version"
  say "   Backlog.md CLI: $version"
}

# Docker, and the address the sandboxes use to reach the proxy: the
# gateway of the bridge network. If the host's gateway is another address,
# the probes would look for the proxy where it cannot be, and the lane
# would fail later for a reason that has nothing to do with the candidate.
require_docker_gateway() {
  command -v docker >/dev/null 2>&1 || die "docker is not installed: the probes need it"
  docker version --format '{{.Server.Version}}' >/dev/null 2>&1 \
    || die "the docker daemon is not answering: the probes need it"
  local gateway
  gateway="$(docker network inspect bridge --format '{{(index .IPAM.Config 0).Gateway}}' 2>/dev/null)"
  [ -n "$gateway" ] || die "the docker bridge network has no gateway address"
  [ "$gateway" = "$PROXY_HOST" ] || die \
    "the docker bridge gateway is $gateway but the probes reach the proxy at $PROXY_HOST: the sandbox would not find the proxy"
  say "   docker bridge gateway: $gateway"
}

# The restricted egress proxy. One already answering is reused; otherwise
# the lane starts the versioned one and stops it at the end.
start_proxy() {
  say "── starting the restricted egress proxy (scripts/egress-proxy.py)"
  python3 scripts/egress-proxy.py --bind "$PROXY_HOST" --port "$PROXY_PORT" \
    > "$OUT/proxy.log" 2>&1 &
  PROXY_PID=$!
  local attempt
  for attempt in $(seq 1 20); do
    if tcp_reachable "$PROXY_HOST" "$PROXY_PORT"; then
      say "   restricted egress proxy reachable at $PROXY_URL (pid $PROXY_PID)"
      return 0
    fi
    kill -0 "$PROXY_PID" 2>/dev/null || break
    sleep 0.5
  done
  say "the proxy output:" >&2
  cat "$OUT/proxy.log" >&2 2>/dev/null
  die "the restricted egress proxy did not become reachable at $PROXY_URL"
}

ensure_proxy() {
  if tcp_reachable "$PROXY_HOST" "$PROXY_PORT"; then
    say "   restricted egress proxy already reachable at $PROXY_URL"
    return 0
  fi
  start_proxy
}

# The lane owns its port: a stale server answering on it would make the
# probes run against the wrong binary and the lane look green.
port_is_free() {
  python3 - "$1" <<'PY'
import socket
import sys

sock = socket.socket()
try:
    sock.bind(("127.0.0.1", int(sys.argv[1])))
except OSError as error:
    print(error)
    sys.exit(1)
finally:
    sock.close()
PY
}

# The processes the lane owns: the control plane and, when the lane
# started it, the restricted egress proxy. Both stop when the lane ends,
# whatever the reason.
SERVER_PID=""
PROXY_PID=""
cleanup() {
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null
    wait "$SERVER_PID" 2>/dev/null
  fi
  if [ -n "$PROXY_PID" ] && kill -0 "$PROXY_PID" 2>/dev/null; then
    kill "$PROXY_PID" 2>/dev/null
    wait "$PROXY_PID" 2>/dev/null
  fi
}
trap cleanup EXIT

say "── the probe prerequisites"
require_backlog_cli
require_docker_gateway
if tcp_reachable "$PROXY_HOST" "$PROXY_PORT"; then
  say "   restricted egress proxy reachable at $PROXY_URL"
else
  say "   restricted egress proxy not running at $PROXY_URL (the lane starts its own)"
fi

if [ "$PREFLIGHT" = "1" ]; then
  say ""
  say "probes: the prerequisites are present"
  exit 0
fi

say "── building the candidate binaries (release profile)"
cargo build --release -p hodei-mh -p hodei-mh-lifecycle-server || {
  say "probes: the candidate binaries did not build"; exit 1;
}

if ! port_is_free "$PORT" > "$OUT/port.log" 2>&1; then
  say "probes: port $PORT is already in use — the lane cannot own it:"
  cat "$OUT/port.log"
  exit 1
fi

# The restricted egress proxy belongs to the lane: it starts it before the
# probes and stops it at the end.
ensure_proxy

say "── starting the control plane on port $PORT"
PORT="$PORT" "$SERVER" > "$OUT/server.log" 2>&1 &
SERVER_PID=$!

ready=0
for _ in $(seq 1 30); do
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    say "probes: the control plane process died while starting"
    cat "$OUT/server.log"
    exit 1
  fi
  code=$(curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:$PORT/ready" 2>/dev/null)
  if [ "$code" = "200" ]; then ready=1; break; fi
  sleep 1
done
if [ "$ready" != "1" ]; then
  say "probes: the control plane did not become ready on port $PORT"
  cat "$OUT/server.log"
  exit 1
fi
# The server that answers must be the one this lane started.
if ! kill -0 "$SERVER_PID" 2>/dev/null; then
  say "probes: the control plane that answered on port $PORT is not the one this lane started"
  cat "$OUT/server.log"
  exit 1
fi
say "   control plane ready (pid $SERVER_PID)"

status=0
for round in $(seq 1 "$ROUNDS"); do
  for probe in $PROBES; do
    log="$OUT/$probe-$round.json"
    if "$BIN" "$probe" --server "http://127.0.0.1:$PORT" > "$log" 2> "$log.err"; then
      say "   round $round: $probe PASS"
    else
      say "   round $round: $probe FAIL (see $log and $log.err)"
      status=1
    fi
  done
done

say "── residue left on the host"
sandboxes=$(curl -s "http://127.0.0.1:$PORT/v1/sandboxes" 2>/dev/null \
  | python3 -c 'import json,sys; print(len((json.load(sys.stdin) or {}).get("items",[])))' 2>/dev/null)
containers=$(docker ps -a --filter "name=sandbox-" --format '{{.Names}}' 2>/dev/null | wc -l)
say "   sandboxes=${sandboxes:-unknown} containers=${containers}"
if [ "${sandboxes:-1}" != "0" ] || [ "$containers" != "0" ]; then
  say "probes: the lane left residue on the host"
  status=1
fi

say ""
if [ "$status" = "0" ]; then
  say "probes: PASS"
else
  say "probes: FAIL"
fi
exit "$status"
