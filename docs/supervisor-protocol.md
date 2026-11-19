# The supervisor control surface: contract, identity and bounds

The supervisor is the ONE controller of a project. Clients (the CLI, the
TUI, the attached terminal) never start a controller; they speak to the
socket one already serves. This document is the contract of that surface:
what a client may send, what it gets back, who is allowed to talk to it,
and what happens when a client misbehaves.

Everything stated here has a counterpart in the code or in a test. A claim
without one is a bug in this document.

## 1. Where the socket lives

The default socket is **not** in the shared temporary directory:

```
$XDG_RUNTIME_DIR/hodei-mh/supervisor.sock      (default)
```

`$XDG_RUNTIME_DIR` is `/run/user/<uid>` on a normal Linux session; when it
is not set the harness falls back to `/run/user/<uid>` and then to a
per-uid directory under the system temporary directory. The containing
directory is created with mode `0700` and the socket file with mode
`0600`, so only the owner of the session can reach the control surface.

`--socket PATH` still overrides the location (the probes and the tests
need isolated sockets). The override does NOT relax the protections: the
socket file is still `0600` and the peers are still authenticated.

The old default (`/tmp/hodei-mh-supervisor.sock`) was readable by every
local user. That is a migration: see §7.

## 2. Identity: two exclusive locks, no name probe

"Who owns this control surface" was decided by asking whether a file with
a given NAME existed and whether somebody answered on it. Both questions
are racy, and a race here is not a nuisance: it produces **two
controllers**, each with its own state, and the clients then disagree.

The controller now takes two **exclusive advisory locks** (`flock`), both
atomic and both released by the kernel when the process dies:

| Lock | File | What it excludes |
|---|---|---|
| Project | `$XDG_RUNTIME_DIR/hodei-mh/<project>.lock` | a second controller for the SAME project, whatever socket it uses |
| Socket | `<socket>.lock` | a second owner of the SAME socket path |

The project lock is the identity: the product serves ONE controller per
project, and the engine already refuses a `Start` for a foreign project.
The socket lock makes "this path is mine" atomic.

Both lock files record who holds them (project, pid, start time), so a
refusal can name the holder instead of saying "already running".

A socket file left behind by a crashed controller is **stale garbage** and
is reused: the new controller removes it and binds its own. A file at the
socket path that is **not a socket** (a regular file, a directory, a
symlink to one) is **never removed**: the controller refuses to start and
says what occupies the path. Deleting a file that the operator put there
is not the controller's decision.

## 3. Who may talk to the supervisor

Two layers, both always on:

1. **Filesystem permissions.** The socket is `0600` inside a `0700`
   directory. A different local user cannot open it.
2. **Peer credentials** (Linux). The server reads the peer's credentials
   from the kernel (`SO_PEERCRED`) and refuses a connection whose
   effective user is not the controller's own. On platforms without peer
   credentials the filesystem permissions are the only layer, and the
   controller says so when it starts.

## 4. Bounds

A control surface that a client can make grow without limit is a denial
of service against the operator. Every dimension of the exchange is
capped, and every cap produces an explicit answer — never a silent
truncation and never a hang.

| Bound | Value | What happens when it is exceeded |
|---|---|---|
| Request line | 64 KiB | the request is refused with an error response and the connection is closed |
| Response line | 1 MiB | the response is replaced by an error that names the limit; the client is told to ask for a smaller page |
| Events per response | 256 | the page ends early and the cursor points at the next event |
| Event payload | 8 KiB | the payload is replaced by a typed marker that states the original size |
| Retained events | 1000 | the oldest fall off the ring; the client is told how many it lost |
| Write to a client | 10 s | the connection is dropped; the slow client never blocks the others |
| Idle connection | 5 min | the connection is closed |
| Concurrent connections | 64 | the extra connection is refused with an error response |
| Collected command output | 1 MiB | the output stops growing and the result says how many bytes were dropped |

"Excessive" is bounded, not forbidden: a normal client keeps its service
while an abusive one is being cut down, because every connection is
handled by its own task and no lock is held across an await.

## 5. The wire contract (protocol version 2)

One JSON object per line, in both directions. `Request` is
`{"req": ...}`, `Response` is `{"resp": ...}`.

```json
{"req":"snapshot"}
{"req":"events","since":0}
{"req":"action","action":{"action":"start","project":"gc","ticket":"TASK-9","revision":null}}
```

A response is one of:

```json
{"resp":"snapshot","protocol_version":2,"project":"gc","phase":"running", ...}
{"resp":"events","protocol_version":2,"events":[...],"next_seq":42,"oldest_seq":3,"lost_before":0}
{"resp":"action","action":{...},"applied":true,"detail":"..."}
{"resp":"error","message":"..."}
```

### 5.1 Events keep their structure

An event is:

```json
{
  "seq": 12,
  "epoch": 3,
  "kind": "objective/outcome",
  "payload": {"work_ref": "TASK-9", "detail": "published"},
  "provenance": {"prov":"automatic","rule":"runtime-outcome"}
}
```

- `payload` is a **typed JSON value**. It used to be flattened into a
  string, which forced every client to re-parse text to recover the
  structure it had lost.
- `epoch` says which run produced the event. The sequence number says
  when; the epoch says to which run it belongs.
- `provenance` still says where the event came from, and the payload is
  redacted BEFORE it enters the record: a credential never reaches the
  wire, and the redaction walks the whole tree (objects, arrays and
  strings), not only its top level.

### 5.2 The cursor, and the announcement of a lost history

`next_seq` is the CURSOR: the sequence number the client must ask for to
continue. It is the sequence after the last event of the page, so a
client can walk the whole log in pages of at most 256 events.

The engine retains the last 1000 events. A client that asks for events
older than the retained window does NOT get a shorter answer and no
notice: the response carries

- `oldest_seq`: the sequence number of the oldest retained event, and
- `lost_before`: how many events the client asked for that are gone.

`lost_before > 0` is the announcement: the client knows its history has
a hole, and the CLI and the attached terminal say so in their output. A
client that reconnects with a cursor older than the window is told, not
silently given a truncated log.

## 6. The command output quota

The one-shot transport collects what a command prints. That output was
unbounded: a command that printed a gigabyte exhausted the memory of the
harness. The collection now has a quota (1 MiB) and, when it is reached:

- the collected bytes stop growing (the collection stays a faithful
  PREFIX of the session output),
- the bytes that were received and dropped are COUNTED,
- the result says `truncated: true` and how many bytes were dropped, and
  the text itself carries the marker.

The distinction that already existed is preserved: `missing_bytes` are
the bytes that never arrived (a hole in the stream), `dropped_bytes` are
the bytes that arrived and were discarded by the quota. A truncated
output is never announced as a complete one.

## 7. Migration

- The default socket moved from `/tmp/hodei-mh-supervisor.sock` to
  `$XDG_RUNTIME_DIR/hodei-mh/supervisor.sock`. Pass `--socket` to keep
  the old path; it is still honoured, with the same protections.
- `Response::events` carries `protocol_version`, `oldest_seq` and
  `lost_before`, and `StructuredEvent` carries `epoch` and a typed
  `payload`. Clients of the previous shape must be updated: the payload
  is no longer a string, and the event page is no longer a pair.
- The TTL of a managed environment is renewed with a checked addition and
  a ceiling. A renewal that would overflow, or that would exceed the
  ceiling, is refused with its reason.

## 8. What this contract does NOT change

- The transport stays NDJSON over a Unix socket. Moving to gRPC is not
  justified by the bounds above: the problem was unboundedness and a
  racy identity, not the encoding.
- The engine remains the ONE state machine. The bounds live in the
  adapters (the socket server, the transport), not in the pure core.
- Clients still never start a controller. A client that cannot reach the
  socket says so.
