#!/usr/bin/env bash
# The EXPENSIVE lane: the probes that need real infrastructure (Docker and
# the OpenSandbox lifecycle server).
#
#   scripts/probes.sh                # one round of the probes
#   scripts/probes.sh --rounds 3     # three consecutive rounds
#   scripts/probes.sh --probe profile-probe
#   scripts/probes.sh --probe trust-probe
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
# tracker), Docker, an operator-configured restricted egress proxy, and the
# images of the profiles. Set HODEI_MH_EGRESS_PROXY_CONTAINER to the trusted
# running container and HODEI_MH_PROXY_CHECK_URL to its host-reachable HTTP
# origin. PROXY_PORT selects its listening port inside sandbox networks (8888
# by default). Its client ACL must allow Docker's dynamic sandbox network pools.
# The doctor is read-only and refuses an image that is not
# already in the local daemon. A missing prerequisite fails the lane; it
# never degrades it in silence.
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT" || exit 1

PORT="${PORT:-8081}"
ROUNDS=1
PROBES="release-probe supervisor-probe profile-probe multiproject-probe verify-probe trust-probe"
OUT="${OUT:-/tmp/hodei-probes}"
BIN="target/x86_64-unknown-linux-gnu/release/hodei-mh"
SERVER="target/x86_64-unknown-linux-gnu/release/hodei-mh-lifecycle-server"

# The lifecycle server attaches the operator's trusted proxy to each
# sandbox network under this stable alias. The host check uses a separate URL.
PROXY_PORT="${PROXY_PORT:-8888}"
PROXY_URL="http://hodei-proxy:${PROXY_PORT}"

# The official Backlog.md CLI the probes use. The version is only reported:
# the lane needs the CLI, not a particular build of it.
BACKLOG_CLI_VERSION="${BACKLOG_CLI_VERSION:-1.55.3}"

# The images the probes need in the local daemon. The doctor is read-only:
# it never pulls, and it refuses an image that is not already there, so
# the lane pulls what is missing before the probes and says what it did.
# The pinned profiles are in docs/profiles.md.
# The trust probe measures the boundary from INSIDE a sandbox, so its
# image must carry a python interpreter: the measurement is a program,
# not a shell pipeline.
PROBE_IMAGES="${PROBE_IMAGES:-rust:1-bookworm opensandbox/code-interpreter:v1.1.0 python:3.12-slim}"

# Preflight only: check the prerequisites and report, without running a
# probe and without starting the control plane.
PREFLIGHT=0

while [ $# -gt 0 ]; do
  case "$1" in
    --rounds) ROUNDS="${2:-1}"; shift 2 ;;
    --probe) PROBES="${2:-}"; shift 2 ;;
    --port) PORT="${2:-8081}"; shift 2 ;;
    --preflight) PREFLIGHT=1; shift ;;
    -h|--help) sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
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

# Docker must be available to create the per-sandbox networks.
require_docker() {
  command -v docker >/dev/null 2>&1 || die "docker is not installed: the probes need it"
  docker version --format '{{.Server.Version}}' >/dev/null 2>&1 \
    || die "the docker daemon is not answering: the probes need it"
  say "   Docker daemon available"
}

# The images the probes need: the doctor refuses an image that is not in
# the local daemon, and it never pulls one itself.
require_images() {
  local image log
  for image in $PROBE_IMAGES; do
    if docker image inspect "$image" >/dev/null 2>&1; then
      say "   image present: $image"
      continue
    fi
    say "   pulling the image the probes need: $image"
    log="$OUT/pull-$(printf '%s' "$image" | tr ':/' '--').log"
    if ! docker pull "$image" > "$log" 2>&1; then
      cat "$log" >&2
      die "the image $image is not present locally and could not be pulled"
    fi
  done
}

# The proxy is an operator-owned trust decision, not a container selected by
# a sandbox request. Its host endpoint is only a reachability check; actual
# sandbox reachability and restricted egress are exercised by the probes.
require_proxy() {
  [ -n "${HODEI_MH_EGRESS_PROXY_CONTAINER:-}" ] || die "set HODEI_MH_EGRESS_PROXY_CONTAINER to the running trusted proxy container"
  [ -n "${HODEI_MH_PROXY_CHECK_URL:-}" ] || die "set HODEI_MH_PROXY_CHECK_URL to the host-reachable proxy HTTP origin"
  [ "$(docker inspect --format '{{.State.Running}}' "$HODEI_MH_EGRESS_PROXY_CONTAINER" 2>/dev/null)" = true ] \
    || die "the configured trusted proxy container is not running"
  python3 - "$HODEI_MH_PROXY_CHECK_URL" <<'PY'
import socket
import sys
from urllib.parse import urlsplit

try:
    url = urlsplit(sys.argv[1])
    if (url.scheme != 'http' or not url.hostname or url.port is None
            or url.username is not None or url.password is not None
            or url.path not in ('', '/') or url.query or url.fragment):
        raise ValueError('expected an HTTP origin with an explicit port')
    with socket.create_connection((url.hostname, url.port), timeout=3):
        pass
except (ValueError, OSError) as error:
    print(f'proxy host check failed: {error}', file=sys.stderr)
    sys.exit(1)
PY
  [ "$?" = 0 ] || die "the host proxy check URL is invalid or unreachable"
  say "   trusted proxy container running; host check reachable; sandbox endpoint: $PROXY_URL"
}

# The lane owns its port: a stale server answering on it would make the
# probes run against the wrong binary and the lane look green.
port_is_free() {
  python3 - "$1" <<'PY'
import socket
import sys

sock = socket.socket()
# The server binds with SO_REUSEADDR, so a port left in TIME_WAIT by the
# previous round is still usable. The check asks the question the server
# will actually face.
sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
try:
    sock.bind(("127.0.0.1", int(sys.argv[1])))
except OSError as error:
    print(error)
    sys.exit(1)
finally:
    sock.close()
PY
}

# The lane owns the control plane, but never the operator's proxy.
SERVER_PID=""
cleanup() {
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null
    wait "$SERVER_PID" 2>/dev/null
  fi
}
trap cleanup EXIT

say "── the probe prerequisites"
require_backlog_cli
require_docker
require_images
require_proxy

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

[ -n "${HODEI_MH_CONTROL_TOKEN:-}" ] || die "set HODEI_MH_CONTROL_TOKEN for the server and probe clients"
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

# The server attaches the configured proxy when it creates each sandbox.
export HODEI_MH_PROXY_URL="$PROXY_URL"

status=0
for round in $(seq 1 "$ROUNDS"); do
  for probe in $PROBES; do
    log="$OUT/$probe-$round.json"
    if "$BIN" "$probe" --server "http://127.0.0.1:$PORT" --proxy-url "$PROXY_URL" > "$log" 2> "$log.err"; then
      say "   round $round: $probe PASS"
    else
      say "   round $round: $probe FAIL (see $log and $log.err)"
      status=1
    fi
  done
done

say "── residue left on the host"
sandboxes=$(python3 - "$PORT" <<'PY'
import json
import os
import sys
import urllib.request

request = urllib.request.Request(
    f"http://127.0.0.1:{sys.argv[1]}/v1/sandboxes",
    headers={"Authorization": "Bearer " + os.environ["HODEI_MH_CONTROL_TOKEN"]},
)
try:
    with urllib.request.urlopen(request, timeout=5) as response:
        print(len(json.load(response)["items"]))
except (OSError, KeyError, ValueError):
    sys.exit(1)
PY
)
containers=$(docker ps -a --filter "name=sandbox-" --format '{{.Names}}' 2>/dev/null | wc -l) || containers=unknown
networks=$(docker network ls --filter 'label=hodei.io/network-owner=lifecycle-server' --format '{{.ID}}' 2>/dev/null | wc -l) || networks=unknown
say "   sandboxes=${sandboxes:-unknown} containers=${containers} networks=${networks}"
if [ "${sandboxes:-1}" != "0" ] || [ "$containers" != "0" ] || [ "$networks" != "0" ]; then
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
