#!/usr/bin/env bash
# The EXPENSIVE lane: the probes that need real infrastructure (Docker and
# the OpenSandbox lifecycle server).
#
#   scripts/probes.sh                # one round of the four probes
#   scripts/probes.sh --rounds 3     # three consecutive rounds
#   scripts/probes.sh --probe profile-probe
#
# These probes are NOT part of the fast gate: they start sandboxes and run
# real builds. They are kept in an explicit lane so that the cost of the
# cheap checks never hides the cost of these, and so that a failure here is
# a failure, never a skipped check.
#
# The gate NEVER turns a failure into a pass: every probe must exit 0, and
# the lane fails if the host is left with residue.
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT" || exit 1

PORT="${PORT:-8081}"
ROUNDS=1
PROBES="release-probe supervisor-probe profile-probe multiproject-probe"
OUT="${OUT:-/tmp/hodei-probes}"
BIN="target/x86_64-unknown-linux-gnu/release/hodei-mh"
SERVER="target/x86_64-unknown-linux-gnu/release/hodei-mh-lifecycle-server"

while [ $# -gt 0 ]; do
  case "$1" in
    --rounds) ROUNDS="${2:-1}"; shift 2 ;;
    --probe) PROBES="${2:-}"; shift 2 ;;
    --port) PORT="${2:-8081}"; shift 2 ;;
    -h|--help) sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

mkdir -p "$OUT"
say() { printf '%s\n' "$*"; }

# The lane OWNS its port: a stale server answering on it would make the
# probes run against the wrong binary and the lane look green.
port_is_free() {
  python3 - "$1" <<'PY'
import socket, sys
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

say "── building the candidate binaries (release profile)"
cargo build --release -p hodei-mh -p hodei-mh-lifecycle-server || {
  say "probes: the candidate binaries did not build"; exit 1;
}

if ! port_is_free "$PORT" > "$OUT/port.log" 2>&1; then
  say "probes: port $PORT is already in use — the lane cannot own it:"
  cat "$OUT/port.log"
  exit 1
fi

say "── starting the control plane on port $PORT"
PORT="$PORT" "$SERVER" > "$OUT/server.log" 2>&1 &
SERVER_PID=$!
cleanup() {
  if kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null
    wait "$SERVER_PID" 2>/dev/null
  fi
}
trap cleanup EXIT

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
