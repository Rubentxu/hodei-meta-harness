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
`0600`.

The old default (`/tmp/hodei-mh-supervisor.sock`) sat in a directory that
every local user can write to: its PATH could be taken over, and the
socket inherited a mode that depends on the umask of whoever started the
controller. That is the exposure this move removes.

`--socket PATH` still overrides the location (the probes and the tests
need isolated sockets). The override does NOT relax the protections: the
socket file is still `0600`, the peers are still authenticated, and the
locks still live in the private runtime directory.

## 2. Identity: two exclusive locks, no name probe

"Who owns this control surface" was decided by asking whether a file with
a given NAME existed and whether somebody answered on it. Both questions
are racy, and a race here is not a nuisance: it produces **two
controllers**, each with its own state, and the clients then disagree.

The controller now takes two **exclusive advisory locks** (`flock`), both
atomic and both released by the kernel when the process dies. Neither
lock file is ever created next to the socket: the socket may live in a
shared directory, and a lock file there would be a denial of service
that any local user could prepare before the controller starts.

| Lock | File | What it excludes |
|---|---|---|
| Project | `$XDG_RUNTIME_DIR/hodei-mh/projects/<key>.lock` | a second controller for the SAME project, whatever socket it uses |
| Socket | `$XDG_RUNTIME_DIR/hodei-mh/sockets/<key>.lock` | a second owner of the SAME socket path |

The project lock is the IDENTITY: the product serves one controller per
project, and the engine refuses a `Start` for a foreign project. The key
of a lock is a digest of the identity it protects, so an identity is
always exactly one path segment, whatever the caller put in it.

Both lock files record who holds them (project, pid, start time), so a
refusal can name the holder instead of saying "already running". The
record is a convenience for the operator: the EXCLUSION is the kernel's
lock, never that text.

A socket file left behind by a crashed controller is **stale garbage**
and is reused: the new controller removes it and binds its own. A file at
the socket path that is **not a socket** (a regular file, a directory, a
symlink to one) is **never removed**: the controller refuses to start and
says what occupies the path. Deleting a file that the operator put there
is not the controller's decision.

A controller that stops **cleanly** removes its own socket file, and only
its own: the file is compared by device and inode against the one it
bound, so a successor that already replaced the file keeps its socket. A
crash leaves the file behind — nobody ran the cleanup — and the next
controller takes it over, as described above.

## 3. Who may talk to the supervisor

Two layers, both always on:

1. **Filesystem permissions.** The socket is `0600` inside a `0700`
   directory. A different local user cannot open it.
2. **Peer credentials** (Linux). The server reads the peer's credentials
   from the kernel (`SO_PEERCRED`) and refuses a connection whose
   effective user is not the controller's own. The credentials are read
   BEFORE the connection is split, because they only live in the socket.
   On platforms without peer credentials the filesystem permissions are
   the only layer.

## 4. Bounds

A control surface that a client can make grow without limit is a denial
of service against the operator. Every dimension of the exchange is
capped, and every cap produces an explicit answer — never a silent
truncation and never a hang.

| Bound | Value | What happens when it is exceeded |
|---|---|---|
| Request line | 64 KiB | the request is refused with an error response and the connection is closed |
| Response line | 1 MiB | the page is cut down to what fits; the cursor says where to continue |
| Events per page | 256 | the page ends early and the cursor points at the next event |
| Event body | 4 KiB | the body is replaced by a typed marker that states its original size |
| Retained events | 1000 | the oldest fall off the ring; the client is told how many it lost |
| Write to a client | 10 s | the connection is dropped |
| Idle connection | 5 min | the connection is closed |
| Concurrent connections | 64 | the extra connection is refused with an error response |
| Collected command output | 1 MiB | the output stops growing and the result says how many bytes were dropped |

The bounds compose: a full page of full bodies is 256 × 4 KiB = 1 MiB,
the size of one answer. A client that wants a smaller page asks for one
(`limit`), and the answer is cut to fit whatever the client asked for.

A refused request is READ by the client that sent it: after writing the
refusal the supervisor discards what the client keeps sending (bounded
by four times the request limit, and by the silence of the client) and
only then closes the connection. A socket closed with data still unread
in its queue is RESET, and the client would lose the answer that says
why it was refused.

"Excessive" is bounded, not forbidden: a normal client keeps its service
while an abusive one is being cut down, because every connection is
handled by its own task, the number of connections is bounded, and no
lock is held across an await.

## 5. The wire contract (protocol version 2)

One JSON object per line, in both directions. `Request` is
`{"req": ...}`, `Response` is `{"resp": ...}`.

```json
{"req":"snapshot"}
{"req":"events","since":0,"limit":64}
{"req":"action","action":{"action":"start","project":"gc","ticket":"TASK-9","revision":null}}
```

Every answer states the version of the contract it was produced with, and
a client that reads another version REFUSES the answer instead of
guessing what it means:

```json
{"resp":"events","protocol_version":2,"events":[...],"next_seq":42,"oldest_seq":3,"history_lost":false}
{"resp":"action","protocol_version":2,"outcome":{"action":{...},"applied":true,"detail":"..."}}
{"resp":"error","protocol_version":2,"message":"..."}
{"resp":"snapshot", ...}
```

The snapshot carries the version too, inside the projection itself.

### 5.1 Events keep their structure

An event is:

```json
{
  "seq": 12,
  "epoch": 3,
  "kind": "objective/outcome",
  "payload": {"outcome": "confirmed", "work_ref": "TASK-9", "detail": "published"},
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
  wire. The redaction walks the whole tree — objects, arrays and strings,
  at any depth — not only its top level.

### 5.2 The cursor, and the announcement of a lost history

`next_seq` is the CURSOR: the sequence number the client must ask for to
continue. It is the sequence after the last event of the page, so a
client can walk the whole log in pages of at most 256 events. It never
goes backwards: a client that asks for events that do not exist yet keeps
its own position.

The engine retains the last 1000 events. A client that asks for events
older than the retained window does NOT get a shorter answer and no
notice: the response carries

- `oldest_seq`: the sequence number of the oldest retained event, and
- `history_lost`: whether the events the client asked for are gone.

When the history was lost the page starts at `oldest_seq`: the hole is
announced instead of being papered over with events that never existed.

`history_lost` is announced, not implied: the CLI and the attached
terminal say it in their output. A client that reconnects with a cursor
older than the window is told, not silently given a truncated log.

## 6. The command output quota

The one-shot transport collects what a command prints. That output was
unbounded: a command that printed a gigabyte exhausted the memory of the
harness. The collection now has a quota (1 MiB) and, when it is reached:

- the collected BYTES stop growing (they stay a faithful PREFIX of the
  session output);
- the bytes that were received and dropped are COUNTED apart from the
  bytes that never arrived;
- the result says `truncated: true` and how many bytes were dropped, and
  the text itself carries the marker.

Three counters keep the truth apart, and they are never mixed:

| Counter | What it counts |
|---|---|
| the collected bytes | what was kept (at most the quota) |
| `missing_bytes` | the bytes that NEVER arrived (a hole in the stream) |
| `dropped_bytes` | the bytes that arrived and were discarded by the quota |

The text of the result also carries the diagnostics (a failed attach, a
failed replay) and the truncation marker, so a reader that only looks at
the text still learns that the output is not the whole one. The
completeness is always measured against the BYTES, never against the
text.

## 7. Migration

- The default socket moved from `/tmp/hodei-mh-supervisor.sock` to
  `$XDG_RUNTIME_DIR/hodei-mh/supervisor.sock`. Pass `--socket` to keep
  the old path; it is still honoured, with the same protections.
- The contract is version 2 and every answer states its version. A
  client that does not check it refuses the answer.
- `Response::events` carries `protocol_version`, `oldest_seq` and
  `history_lost`; `Response::action` carries its outcome under
  `outcome`; `StructuredEvent` carries `epoch` and a typed `payload`.
  Clients of the previous shape must be updated.
- A client that asks for events must FOLLOW the cursor. A client that
  reads one page and ignores it shows a truncated history as if it were
  the whole one; that is what the cursor is for.
- The TTL of a managed environment is renewed with a checked addition
  and a ceiling of seven days. A renewal that would overflow, or that
  would pass the ceiling, is refused with its reason.
- A controller for a project that another controller already serves is
  refused, with the name of the holder. A file that is not a socket at
  the socket path is refused and left alone.
- A controller that stops cleanly removes its own socket file. A file
  that is not the one it bound is left alone, even at the moment it
  stops.
- A connection may now end without an answer: a client that does not
  read within ten seconds, or that stays silent for five minutes, is
  dropped. A request that exceeds the limit is refused with an answer
  that the client can read.

## 8. What this contract does NOT change

- The transport stays NDJSON over a Unix socket. Moving to gRPC is not
  justified by the bounds above: the problem was unboundedness and a
  racy identity, not the encoding.
- The engine remains the ONE state machine. The bounds live in the
  adapters (the socket server, the transport), not in the pure core.
- Clients still never start a controller. A client that cannot reach the
  socket says so.
