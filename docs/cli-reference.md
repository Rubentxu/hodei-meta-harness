# hodei-mh CLI reference — one section per subcommand

This is the reference for the `hodei-mh` binary. Every flag, refusal and
exit code here is present in the source: the command list is the `USAGE`
constant in `crates/hodei-mh/src/main.rs`, and each behaviour is stated
where it is implemented. Nothing here is inferred from "how it would
normally work".

```
usage: hodei-mh <command> [options]
```

Calling the binary with no command, or with a command that is not listed,
prints the usage on **stderr** and exits `2`. `--version` (`-V`) prints
`hodei-mh <version>` and exits `0`. `--help` (`-h`, `help`) prints the
usage on **stdout**, followed by the `documentation:` line that points at
the `docs/profiles.md` the consumer really has (installed next to the
binary, or the workspace's copy — resolved at run time), and exits `0`.

The wire contract of the supervisor surface (events, cursor, page bounds,
authentication) is not repeated here; it lives in
[supervisor-protocol.md](supervisor-protocol.md). The profile pinning and
the egress prerequisites live in [profiles.md](profiles.md); the
environment variables are collected in
[configuration.md](configuration.md) (that file is the subject of a
separate task, so the link may not resolve yet).

## Conventions shared by the product commands

| Item | Value |
|---|---|
| Lifecycle server default | `http://127.0.0.1:8081` (`--server`) |
| Egress proxy default | `http://hodei-proxy:8888` (`--proxy-url`, overridable through `HODEI_MH_PROXY_URL`) |
| Control socket default | `$XDG_RUNTIME_DIR/hodei-mh/supervisor.sock` (`--socket`) |
| Exit `0` | the command did what it said |
| Exit `1` | a refusal or a failure of the operation itself, stated on stderr |
| Exit `2` | a usage error: the request was not well-formed, so nothing was attempted |

---

## `doctor`

**Purpose.** The read-only checks of a host plus the availability gate for
one profile. It never creates, starts or removes anything; every check is
a read. See [profiles.md](profiles.md) for what each check measures.

**Flags.**

| Flag | Default |
|---|---|
| `--profile <name>` | `rust`; the supported product profiles are `rust` and `jvm` |
| `--image <ref>` | unset; when passed, the profile is a **user** image instead of a pinned one |
| `--server <url>` | `http://127.0.0.1:8081` |
| `--proxy-url <url>` | `HODEI_MH_PROXY_URL`, else `http://hodei-proxy:8888` |
| `--subscription-tier <tier>` | `none` |
| `--subscription-limitation <text>` | `no credentials configured; networked builds will fail to fetch dependencies` |

`--image` wins over `--profile`: with `--image` the profile name comes
from `--profile` (defaulting to `user`) and the image is used as given.

**Output.** One line per check on stdout, labelled by its weight:

```
[ok] <name>: <detail>        passing
[MISS] <name>: <detail>      failing and blocking
[note] <name>: <detail>      failing or noting, non-blocking
```

then the decision: `gate: PROCEED`, or `gate: REFUSE — no native (YOLO)
fallback exists` followed by one `  missing: <piece>` line per missing
requirement.

**Exit codes.** `0` gate PROCEED · `1` gate REFUSE · `2` the profile
request itself is bad (unknown profile name; an image reference carrying
credentials).

**Refusals.**

* **Pinned image not present locally** — `pinned-image-local` reports
  `<ref> not present locally — the docker daemon must pull it first (the
  profile never runs without it)` and the gate refuses with
  `pinned image (pull required; the daemon fetches it, the profile never
  runs without it)`.
* **Digest mismatch** — a local image whose digest is not the pinned one
  is reported as `MISMATCH` and the gate refuses with `local image digest
  does not match the pinned digest`. A foreign image is never trusted.
* **No restricted egress** — the proxy did not answer a TCP connect; the
  gate refuses with `restricted egress path (proxy)`. There is no native
  fallback: the build does not happen.
* **Lifecycle API unreachable** — `GET /ready` did not answer; the gate
  refuses with `opensandbox lifecycle api`.
* **Unpinned user image** — `profile-pinned` notes it without blocking
  (reproducibility depends on the registry tag), but the same image must
  still be present locally for the gate to proceed.
* **Usage errors** (`exit 2`) — `unknown profile '<x>'; supported: rust,
  jvm (or pass --image for a user image)`, and `the image reference must
  not carry credentials` (a `user:pass@` reference would travel to the
  daemon and into the run record).
* **Sandbox egress isolation** is reported as `[note]` with
  `not measured: isolation must be verified from inside a sandbox (this
  check reports unknown, not a pass)`. The doctor never claims a fact it
  did not measure.

---

## `profile-run`

**Purpose.** Run a project's build plan inside a pinned sandbox: doctor →
gate → sandbox → observe the effective limits → build → record. The
record states what was measured, not what was requested.

**Flags.** Everything `doctor` accepts, plus:

| Flag | Default |
|---|---|
| `--project <DIR>` | **required**; the host directory to build |
| `--cpu <limit>` | `1` (requested; the record also holds the observed value) |
| `--memory <limit>` | `1Gi` (requested) |
| `--export-untracked` | off; ships the unversioned changes as well as the tracked ones |
| `--cache` | off; the build cache is opt-in |
| `--no-cache` | wins over `--cache` and over `HODEI_MH_BUILD_CACHE=1` |
| `--cache-target` | off; extends the cache from dependency downloads to compiled outputs |
| `--cache-dir <path>` | `$XDG_CACHE_HOME/hodei-mh/build-cache`, else `$HOME/.cache/hodei-mh/build-cache`, else a temporary directory |
| `--cache-quota-mib <n>` | `1024` (1 GiB), pruned oldest-first |

The environment can enable and relocate the cache through
`HODEI_MH_BUILD_CACHE=1` and `HODEI_MH_BUILD_CACHE_DIR`; flags win over
the environment (see [configuration.md](configuration.md)).

**Output.** A single pretty-printed JSON object on stdout. On a gate
refusal the object carries `"gate": "REFUSE"` with the checks, the
`missing` list, `"yolo_fallback": false`, `"build_executed_on_host":
false` and the subscription note. On a run it carries the requested and
observed image digests and limits, the sandbox's own egress measurement
(`measured_from: "inside the sandbox"`), the cache report
(`restore`/`persist` with `cold`, `warm`, `skipped`, `unsupported`,
`rejected`, `refused` or `failed`), the redacted command records, the
project-export report, the cleanup verdict, the durable-export digest,
`secret_leak_positions_in_commands` and `elapsed_seconds`. Credentials
never enter commands, logs or the record; the output tail is redacted
**before** it is truncated.

**Exit codes.** `0` gate proceeded, every command succeeded and no leak
was found · `1` gate refusal, or the run failed / a leak was found ·
`2` a usage or configuration error before anything ran.

**Refusals.**

* **Every `doctor` refusal** — with `"gate": "REFUSE"` in the record and
  exit `1`, including the pinned **image not present locally** case. No
  sandbox is created for a run the gate will not admit.
* **`--project DIR is required`**, a **project dir that does not exist**,
  an **invalid `--proxy-url`** (`proxy URL must be an HTTP origin without
  credentials`, `invalid proxy URL`, `proxy URL lacks a host`, `proxy URL
  lacks a port`), an **image reference carrying credentials**, an
  **unknown profile**, and an **invalid `--cache-quota-mib`**
  (`--cache-quota-mib expects a positive integer number of MiB`) are all
  exit `2`.
* **Project export refused or failed** — `project export refused: <error>`
  / `project export failed: <error>`, exit `1`. It is decided before the
  sandbox exists, so a refusal leaves nothing to clean up.
* **Sandbox create unresolved** — `sandbox create unresolved: <reason>
  (request-tag <tag>)`, exit `1`. The effect is searched by the request
  tag **before** any retry; a failed search aborts, it never licenses a
  second sandbox.
* **Budget exhausted (429)** — the lifecycle server answers a create the
  host's aggregate budget cannot admit with `429 Too Many Requests` and
  the code `SANDBOX::BUDGET_EXHAUSTED`. Nothing was created, the tag
  search finds no effect, the create is retried exactly once with the
  same request tag, and a second refusal is reported as `sandbox create
  retry returned 429 Too Many Requests` with exit `1`.
* **execd never became usable** — `sandbox <id> never became usable: its
  execd did not answer within 60s`, exit `1`.
* **Network policy could not be verified** — measured from inside the
  sandbox: it requires `PROXY_REACHABLE` **and** `DIRECT_INTERNET_BLOCKED`
  from a successful probe. `sandbox <id>: network policy could not be
  verified`, exit `1`.
* **Project staging failed** — `project staging failed: uploaded=<bool>
  staged_ok=<bool>`, exit `1`.
* **A poisoned cache is discarded, never restored** — an archive whose
  digest is not the recorded one is removed and reported as `rejected`;
  an archive with no manifest entry is reported as `cold`.
* **A failed build stores no cache** — `persist: {"status": "skipped",
  "reason": "the build did not succeed"}`.

---

## `supervisor`

**Purpose.** The single controller of one project. It serves the control
socket until interrupted, and logs every event to stdout with its
provenance and after credential redaction. Clients never start a
controller; they speak to the one that is already serving.

**Flags.**

| Flag | Default |
|---|---|
| `--socket <path>` | `$XDG_RUNTIME_DIR/hodei-mh/supervisor.sock` |
| `--project <name>` | `hodei-mh` |
| `--rehearsal` | off; the rehearsal runtime — a deterministic run with no external effects |
| `--live` | off; the live runtime — the real tracker, the real sandboxes, the real managed branch |
| `--server <url>` | **required with `--live`** |
| `--image <ref>` | **required with `--live`** |
| `--project-dir <path>` | **required with `--live`** |
| `--tracker-dir <path>` | **required with `--live`** |
| `--managed-branch <branch>` | **required with `--live`** |
| `--proxy-url <url>` | `http://hodei-proxy:8888` |
| `--permission allow-once\|deny` | `allow-once` |

`--live` and `--rehearsal` are exclusive. Credentials for the live
runtime come from the environment only (`ANTHROPIC_AUTH_TOKEN`); they are
never a flag.

**Output.** One line on stdout at boot: `supervisor listening on <socket>;
logs identify provenance, credentials are redacted; runtime: <label>`.
Then every recorded event, in sequence order, as a readable line prefixed
with its provenance tag (`[human]`, `[auto]`, `[telemetry]`,
`[telemetry-UNKNOWN]`), flushed as it is written. The event fields and
the cursor semantics are the wire contract in
[supervisor-protocol.md](supervisor-protocol.md). The live runtime prints
its checkpoint recovery notes on stderr (`live checkpoint: …`) and the
`supervisor/boot` event carries the runtime identity.

**Exit codes.** `0` the control surface stopped cleanly · `1` a boot or
runtime refusal, a socket that could not be bound, a control surface that
stopped with an error, or a log that could not be written · `2` never (no
usage path here).

**Refusals.**

* **`--live` together with `--rehearsal`** — `refusing to start: --live and
  --rehearsal are exclusive (a rehearsal measures nothing; a live run
  executes)`, exit `1`.
* **No runtime wired** — without either flag the controller binds and
  serves, and every `start` is refused by the engine with `no runtime is
  wired: this supervisor cannot execute an objective yet`; the boot line
  says `runtime: (none wired — a Start is refused)`. A run nobody can
  execute is not a run, so the refusal is stated instead of pretending.
* **A live boot that cannot execute** — `refusing the live supervisor:
  <reason>`, exit `1`, one reason per fact:
  `--<flag> is required with --live` for each of `server`, `image`,
  `project-dir`, `tracker-dir`, `managed-branch`;
  `--permission has the value '<x>': expected allow-once or deny`;
  `ANTHROPIC_AUTH_TOKEN is empty: the live runtime takes its credentials
  from the environment only`;
  `<path> is not a git repository: the managed branch lives in a
  repository`; `the managed branch '<b>' does not exist in <path>`;
  `the repository <path> has no user.name/user.email: the harness authors
  the candidate commit with the host identity, so git must be configured`;
  `the tracker did not answer at boot: …`; `git in <path> could not
  answer: …`; `the claims of the project could not be opened: …`; `the
  live checkpoint could not be read at boot: …`. The runtime is wired
  **before** the socket is bound, so a controller that cannot execute
  never owns a project identity.
* **Second controller on the same socket** — `cannot bind supervisor socket:
  a supervisor is already listening on <path>: refusing to start a second
  controller`, exit `1`. Ownership is decided by two exclusive locks (the
  socket path and the project identity), not by probing a name.
* **Second controller for the same project** — `another controller already
  serves the project '<project>': refusing to start a second controller`,
  exit `1`.
* **A non-socket in the way** — `<path> is not a socket: refusing to
  remove it and to start`, exit `1`. The file the operator put there is
  never deleted.
* **Permission denied by the agent policy** — with `--permission deny` the
  harness re-decides every `session/request_permission` with `deny_all`
  and emits a structured refusal event naming the tool, the rule and the
  effect that stops; under `allow-once`, a request whose options carry no
  `allow_once` option is an explicit refusal (`permission refused: the
  request offers no allow_once option (<detail>)`). A policy refusal ends
  the conversation at once instead of waiting out a timeout.
* **Budget exhausted (429)** — a live attempt that cannot create its
  sandbox fails against the server's admission (`SANDBOX::BUDGET_EXHAUSTED`,
  HTTP `429`); the attempt's refusal is recorded with its reason, the
  durable checkpoint is written before any claim, and a pending checkpoint
  refuses the next attempt rather than duplicating an effect.
* **A log that cannot be written** — `cannot write the supervisor output:
  <error>` / `cannot flush the supervisor output: <error>`, exit `1`. A
  service whose log is stuck in a buffer is no log. When the *reader*
  closes the pipe, the controller keeps serving and the silence is not an
  error.

---

## `sup-status`

**Purpose.** The CLI view of the supervisor snapshot, rendered through the
same contract the TUI uses.

**Flags.** `--socket <path>` (default as above).

**Output.** The rendered snapshot on stdout: project, phase, pause state,
ticket/goal, revision, resource observation (or its `UNKNOWN` reason),
TTL, epoch, last action, degraded flag.

**Exit codes.** `0` snapshot printed · `1` the supervisor is unreachable
(`supervisor not reachable at <path>: <source> (no socket: no second
controller is started)`) or rejected the request (`supervisor rejected
the request: <error>`) · `0` also when the reader closed the pipe.

**Refusals.** Read-only: it never starts a controller and never sends an
action.

---

## `sup-action`

**Purpose.** Drive the supervisor: `start | pause | resume | stop | renew`,
through the same contract the TUI keys drive.

**Flags and arguments.**

```
hodei-mh sup-action <action> [--socket PATH] [action flags]
```

| Action | Flags |
|---|---|
| `start` | `--ticket <id>`, `--revision <ref>`, `--project <name>` |
| `start --objective` | `--objective <text>` **requires** `--accept <command>`; `--expect-exit <N>` (default `0`) |
| `pause` / `resume` / `stop` | none |
| `renew` | `--added-seconds <n>` (default `300`) |

With no `--project` on a `start`, the CLI reads the supervisor's own
project from a snapshot and uses it; the supervisor still refuses a Start
for a project other than the one it serves.

**Output.** One line on stdout: `applied: <detail> (the supervisor
applied it)` or `refused: <detail> (this was NOT executed)`.

**Exit codes.** `0` applied · `1` not applied, or the supervisor is
unreachable / rejected the request · `2` usage: no action named, an
unknown action, or a refusal before sending.

**Refusals.**

* **Nothing named** — `usage: hodei-mh sup-action
  <start|pause|resume|stop|renew> [--socket PATH]`, exit `2`.
* **Unknown action** — `unknown action <name>; expected
  start|pause|resume|stop|renew`, exit `2`.
* **An objective with no criterion** — `refusing to send the action: a
  custom objective needs an operator termination criterion: pass
  --accept <command> (and optionally --expect-exit <N>); without one it
  could never be confirmed`, exit `2`.
* **An objective that also names a ticket** — `a custom objective owns no
  ticket: name a ticket or an objective, not both`, exit `2`.
* **A criterion without an objective** — `--accept and --expect-exit
  belong to --objective: ticket work is judged by its own acceptance
  criteria`, exit `2`.
* **A non-numeric criterion exit** — `--expect-exit is not an integer:
  <value>` (the value is named), exit `2`.
* **A non-numeric renewal** — `--added-seconds must be a number of
  seconds`, exit `2`.
* **Engine refusals, reported as `refused:` with exit `1`** —
  `project mismatch: this supervisor serves '<served>', not '<asked>'`;
  `no runtime is wired: this supervisor cannot execute an objective yet`;
  `a run is already in flight: not a second controller`; `the previous
  attempt is still winding down: one attempt at a time`. A renewal beyond
  the ceiling is not applied, and an unsupported renewal answers with its
  reason and leaves the tracked TTL where it was.

---

## `sup-events`

**Purpose.** Replay the supervisor's structured event log from a sequence
number. It reads the log; it never changes the run.

**Flags.**

| Flag | Default |
|---|---|
| `--socket <path>` | the default control socket |
| `--since <N>` | `0` (an unparseable value is read as `0`) |
| `--jsonl` | off; one JSON object per line, exactly as the socket carries it |
| `--pretty` | off; the readable tail, `[<provenance>] <rendered event>` |

With neither flag the channel follows stdout: **JSONL when stdout is a
pipe, a file or a subprocess** (a parser reads JSON, not a tail) and the
**readable tail when stdout is a terminal**. The flags win over the
terminal, and they are exclusive.

**Output.** Events in sequence order on stdout. Every page of the socket's
bounded history is followed through its **cursor**, so a paginated log is
never presented as complete when it is not. Events are read from `--since`
onward.

**Exit codes.** `0` the history was read · `1` the supervisor is
unreachable or rejected the request · `0` when the reader closed the pipe
(`sup-events | head` ends the way a pipeline ends) · `1` for any other
write failure · `2` `--jsonl` with `--pretty`.

**Refusals and notices.**

* **Two channels at once** — `refusing to read the events: --jsonl and
  --pretty are exclusive (--jsonl is one JSON object per line for a
  script, --pretty is the readable tail for a person)`, exit `2`.
* **A hole in the history** — the events that are gone are announced,
  never hidden: `history lost: the events before #<seq> are no longer
  retained`. In the JSONL channel the notice travels on **stderr** so
  every stdout line stays exactly one event; in the readable tail it
  goes to stdout with the events.
* **An unencodable event body** — it does not vanish and it does not
  pretend to be the event: a line is emitted with `seq`, `epoch` and a
  `render_error` naming the failure.

The event shape, the epoch, the page bounds and the cursor are defined in
[supervisor-protocol.md](supervisor-protocol.md).

---

## `attach-terminal`

**Purpose.** A separate, **read-only** terminal viewer: it polls the
structured events and never touches the ACP conversation. Leaving it does
not affect the objective.

**Flags.**

| Flag | Default |
|---|---|
| `--socket <path>` | the default control socket |
| `--exit-after-events <N>` | `0` — no limit; otherwise it returns once N events have been delivered |
| `--deadline-seconds <n>` | `60` — turns a supervisor that never answers into an explicit failure instead of a hang |

**Output.** `attached terminal viewer: read-only event tail; leaving does
not affect the objective`, then `[viewer] <rendered event>` lines on
stdout. A rejected poll is printed as `[viewer] supervisor rejected the
poll: <error>` and an unreachable supervisor as `[viewer] supervisor not
reachable (<error>); still attached, still read-only`; a history hole is
printed as `[viewer] history lost: the events before #<seq> are no longer
retained`.

**Exit codes.** `0` the requested number of events was delivered (`--exit-after-events`), or the reader closed the pipe · `1` the deadline was reached first: `viewer deadline reached after <n>s: delivered
<d> events (wanted <m>)` on stderr. Without `--exit-after-events` the
viewer polls until its deadline: it is a tail, not a one-shot command.

**Refusals.** It is a client, so it never starts a controller and never
sends an action; a supervisor that is not there is reported, not replaced.
The event and cursor contract is in
[supervisor-protocol.md](supervisor-protocol.md).

---

## `tui`

**Purpose.** The Ratatui client of the supervisor socket: the snapshot and
the structured events in one panel, driven by the same actions the CLI
sends.

**Flags.**

| Flag | Default |
|---|---|
| `--socket <path>` | the default control socket |
| `--simulated` | off; renders a fixed state with **no socket at all** (and says so — nothing simulated is presented as real) |
| `--frames <N>` | `0`; render N frames non-interactively and leave, with no raw mode and no alternate screen |

**Keys.** `s` start · `p` pause · `r` resume · `t` stop · `n` renew
(+300s) · `q` quit.

**Output.** The rendered interface on stdout, with the warning banner
first: a clipped warning is a lie, so a simulated or degraded state is
announced above the state panel. With no snapshot yet the panel reads
`(no snapshot yet — waiting for the supervisor)` and the event panel reads
`(no events yet)`.

**Exit codes.** `0` quit (or the frames ran) · `1` the terminal could not
be initialised (`cannot init TUI terminal: <error>`) or raw mode could not
be enabled (`cannot enable raw mode: <error>`).

**Refusals.** `--simulated` exists so the surface can be exercised without
a supervisor; it opens no socket and its resource observation is
`Unknown { reason: "the simulated state measured nothing" }`, which is
displayed as `UNKNOWN`, never as a healthy reading. The TUI is a client of
the same contract: it never boots a second controller.

---

## `package`

**Purpose.** Build the installable bundle: the two release binaries, the
product's profile documentation, `install.sh`, `INSTALL.md` and
`SHA256SUMS`. Nothing else — no hidden plugins, no undemonstrated
dependencies.

**Flags.**

| Flag | Default |
|---|---|
| `--out <dir>` | `/tmp/hodei-mh-package` |

The tarball is written next to the output directory as
`hodei-mh-v<version>-<os>-<arch>.tar.gz`, named after the platform the
binaries were **compiled** for (read from their ELF headers), not the one
that happens to run the command.

**Output.** One JSON manifest on stdout: the package path and size, the
contents listing (read back from the tar), the documentation origin
(`installed` or `workspace`) and digest, the SHA-256 of every shipped file
(the digests are read from the staged bytes, so they describe what
travels), and the measured `guarantees`: `yolo_fallback: false`,
`standalone_binaries`, and `undemonstrated_dependencies` — the entries
that are not part of the documented product, listed as scanned.

**Exit codes.** `0` the bundle was built and the manifest printed ·
`1` every refusal below.

**Refusals.**

* **A debug build** — `refusing to package a debug build: <dir> — the
  release profile is the artifact that ships`, exit `1`.
* **Missing binaries outside a build tree** — `release binaries missing at
  <dir> and it is not a build directory: there is nothing to build here`.
  Inside a build tree it builds them (`cargo build --release`, offline,
  `-j2`, up to 5 attempts) and reports `could not build release binaries;
  package aborted` if that fails.
* **The architecture does not match the name** — `refusing to package
  <bin>: it is a <found> binary, but the artifact would be <os>-<arch>`.
* **The dynamic loader is required** (the standalone gate) — `refusing to
  package non-standalone binaries: <bin>: <linkage>, … — build them
  statically (cargo build --release with -C target-feature=+crt-static) or
  the bundle would need the host's glibc`, exit `1`. The linkage is read
  from the binaries, never assumed, and anything that is not a readable
  ELF64 image without a `PT_INTERP` is reported as needing a dynamic
  loader rather than as standalone.
* **No trusted documentation** — `refusing to package without the
  product's documentation: no <prefix>/share/hodei-mh/docs/profiles.md and
  no workspace docs/profiles.md found — the bundle must carry the
  documentation of the product it ships`. A `docs/profiles.md` that merely
  happens to sit in the working directory is not the product's
  documentation, and the workspace is found by its marker
  (`crates/hodei-mh/Cargo.toml`).
* **Staging, installer, digest or tar failure** — `staging failed for
  <relative>`, `failed to write installer files`, `cannot read the staged
  <relative> to compute its digest`, `failed to write the bundle's
  SHA256SUMS`, `tar failed`; each exits `1`.

---

## Probes

The probe subcommands are evidence about this host, not product features;
they are driven by `scripts/probes.sh` (the **integration** lane) and are
listed in `USAGE`:

```
release-probe | profile-probe | supervisor-probe | multiproject-probe | verify-probe | trust-probe
```

`trust-probe` measures the trust boundary from the host and from inside
the sandbox. A probe is removed only when its invariant is asserted in
the productive path; until then the probe is the evidence.
