# Quickstart — one full turn of the objective loop

This is a narrated walkthrough of the goal loop with the real product
commands. Every command is copied from the `USAGE` constant and the CLI
dispatch in `crates/hodei-mh/src/main.rs`, and every expected output was
captured from a real run of the binary. Where the printed `USAGE` and the
implemented behaviour disagree, this document states the implemented
behaviour and marks the `USAGE` note.

Two routes, in increasing cost:

1. [The rehearsal route](#1-the-rehearsal-route-no-docker-no-network-no-model)
   — no Docker, no network, no model. You drive a complete objective loop
   against the deterministic rehearsal runtime, in seconds.
2. [The live route](#2-the-live-route-the-real-loop) — the real lifecycle
   server, real sandboxes, the real tracker and the real managed branch.

The per-command reference (every flag, refusal and exit code) lives in
[cli-reference.md](cli-reference.md); the socket contract in
[supervisor-protocol.md](supervisor-protocol.md); the profile pinning and
the egress prerequisites in [profiles.md](profiles.md). Environment
variables are collected in
[configuration.md](configuration.md), and the plain-language
"what is this" introduction lives in [overview.md](overview.md).

## 0. Build the binaries

From the repository root:

```bash
cargo build --release -p hodei-mh
BIN=target/x86_64-unknown-linux-gnu/release
```

The live route additionally builds the control plane
(`cargo build --release -p hodei-mh-lifecycle-server`, see
[profiles.md](profiles.md#install)). The examples below call the binary
as `$BIN/hodei-mh`.

Sanity check:

```bash
$BIN/hodei-mh --version
```

prints `hodei-mh <version>` and exits `0`. Calling the binary with no
command prints the usage on stderr and exits `2`.

## 1. The rehearsal route (no Docker, no network, no model)

The rehearsal runtime (`--rehearsal`) runs the SAME flow as the product —
select, work, verify, publish — with scripted adapters that touch nothing
outside the process. It never closes a ticket and never moves a ref, and
it says so in every event.

### 1.1 Start the controller on an exclusive socket

A socket path admits exactly one controller: the supervisor takes two
exclusive kernel locks (one for the socket path, one for the project
identity), so a second controller on the same socket is *refused*, not
silently forked ([supervisor-protocol.md §2](supervisor-protocol.md#2-identity-two-exclusive-locks-no-name-probe)).
For the walkthrough we use a dedicated rehearsal socket instead of the
default `$XDG_RUNTIME_DIR/hodei-mh/supervisor.sock`:

```bash
SOCK=/tmp/hodei-quickstart.sock
$BIN/hodei-mh supervisor --rehearsal --socket "$SOCK" --project hodei-mh
```

The controller answers with one boot line and then keeps serving (it
logs every event to stdout as it happens; stop it with Ctrl-C):

```
supervisor listening on /tmp/hodei-quickstart.sock; logs identify provenance, credentials are redacted; runtime: rehearsal (trial-default)
[auto] #1 [auto[boot]] supervisor/boot (epoch 0) "listening on /tmp/hodei-quickstart.sock; credentials will be redacted; runtime: rehearsal (trial-default)"
```

If you try to start a second controller on the same socket you get, with
exit `1`:

```
cannot bind supervisor socket: a supervisor is already listening on /tmp/hodei-quickstart.sock: refusing to start a second controller
```

### 1.2 Start a run: `sup-action start`

From another shell, while the controller is serving:

```bash
$BIN/hodei-mh sup-action start --socket "$SOCK"
```

```
applied: starting for hodei-mh (epoch 1); awaiting the runtime's confirmation (the supervisor applied it)
```

Exit `0`. A `Start` no longer means `running`: the run is `starting`
until the runtime confirms it, and the confirmation is what moves it
forward (see [cli-reference.md](cli-reference.md#supervisor)). The
rehearsal confirms and completes almost instantly, so by the time you
ask, the loop has already turned.

`sup-action` also accepts `start --ticket <id>` (rehearse a named ticket
— the rehearsal rehearses the work it is given), the custom-objective
form `start --objective <text> --accept <command> [--expect-exit <N>]`,
and `pause | resume | stop | renew [--added-seconds <n>]`.

### 1.3 Observe: `sup-status`

```bash
$BIN/hodei-mh sup-status --socket "$SOCK"
```

The snapshot is rendered through the same contract the TUI uses
(`project`, `phase`, the runtime kind, the last action, …):

```
project: hodei-mh
phase: idle
ticket: (none)
goal: tracker work
revision: (none)
runtime: rehearsal (trial-default)
objective: idle: no eligible work
resources: UNKNOWN (no telemetry yet)
context available: no
waiting: (not waiting)
ttl remaining: (untracked)
epoch: 1
grant scope: (none — the supervisor authorizes no effect by itself)
last action: start(hodei-mh) applied=true (starting for hodei-mh (epoch 1); awaiting the runtime's confirmation)
```

Note the two honesty markers: `runtime: rehearsal (trial-default)` —
a rehearsal never presents itself as a live execution — and
`resources: UNKNOWN (no telemetry yet)` — the surface reports what it did
not measure instead of guessing. Exit `0`.

### 1.4 Replay the log: `sup-events --jsonl`

```bash
$BIN/hodei-mh sup-events --socket "$SOCK" --jsonl
```

One JSON object per line, exactly as the socket carries it — the whole
turn of the loop in five events:

```json
{"seq":1,"epoch":0,"kind":"supervisor/boot","payload":"listening on /tmp/hodei-quickstart.sock; credentials will be redacted; runtime: rehearsal (trial-default)","provenance":{"prov":"automatic","rule":"boot"}}
{"seq":2,"epoch":1,"kind":"supervisor/action","payload":{"action":{"action":"start","project":"hodei-mh","revision":null,"ticket":null},"applied":true,"detail":"starting for hodei-mh (epoch 1); awaiting the runtime's confirmation"},"provenance":{"prov":"human"}}
{"seq":3,"epoch":1,"kind":"objective/confirmed","payload":{"detail":"the runtime confirmed the objective","epoch":1,"runtime":"rehearsal (trial-default)"},"provenance":{"prov":"automatic","rule":"runtime-confirmation"}}
{"seq":4,"epoch":1,"kind":"objective/outcome","payload":{"detail":"the rehearsal recorded the publication of T-1; nothing was closed and no ref moved","outcome":"confirmed","work_ref":"T-1"},"provenance":{"prov":"automatic","rule":"runtime-outcome"}}
{"seq":5,"epoch":1,"kind":"objective/outcome","payload":{"outcome":"idle","reason":"no eligible work"},"provenance":{"prov":"automatic","rule":"runtime-outcome"}}
```

Event 4 is the publication the rehearsal *would have* made, and it says
so: *nothing was closed and no ref moved*. Exit `0`.

Channel rules (`main.rs` `USAGE` and `supervisor_cli.rs::resolve_emit`):
with neither flag the answer is JSONL when stdout is a pipe or a file
and the readable tail when stdout is a terminal; `--jsonl` and `--pretty`
force one channel and are exclusive (asking for both exits `2` with the
reason). `--since <N>` replays from a sequence number; a hole in the
retained history is announced, never hidden
([supervisor-protocol.md §5.2](supervisor-protocol.md#52-the-cursor-and-the-announcement-of-a-lost-history)).

### 1.5 What a controller with NO runtime does

Without `--rehearsal` or `--live` there is no runtime wired. The
implemented behaviour (`supervisor_cli.rs::run_supervisor`): the
controller **still binds and serves**, its boot line ends with
`runtime: (none wired — a Start is refused)`, and every start is refused
by the engine:

```bash
$BIN/hodei-mh sup-action start --socket "$SOCK"
```

```
refused: no runtime is wired: this supervisor cannot execute an objective yet (this was NOT executed)
```

Exit `1`. *`USAGE` note:* the help text says that without a runtime flag
"the controller … refuses to start". What actually refuses is every
`Start`, after a controller that started and served; the distinction is
documented faithfully in
[cli-reference.md](cli-reference.md#supervisor) and here. The product's
reason is the same either way: a run nobody can execute is not a run.

## 2. The live route (the real loop)

The live runtime wires the real Backlog.md tracker, the real sandboxes
through the lifecycle server, and the real managed branch. All five
required flags come from `USAGE` (`main.rs`, lines 73–79) and are
enforced in `crates/hodei-mh/src/live/mod.rs::config_from_args`.

### 2.1 Prerequisites

The live route refuses instead of degrading. Before starting, make sure:

- Docker is running and the pinned profile image is present locally
  (`profiles.md` lists the pins); the profile never runs without it.
- A restricted egress proxy is reachable (the gate does not fall back —
  see the doctor output in step 2.2).
- The official Backlog.md CLI is installed: the live tracker writes
  through it (`backlog`), from the `--tracker-dir`.
- The project directory is a git repository with the managed branch and
  `user.name`/`user.email` configured — the harness authors the
  candidate commit with the host identity.
- The provider credential is in the environment of the supervisor
  process only (section 3 below; never a flag, never argv).

### 2.2 Start the lifecycle server

Exactly as in [profiles.md](profiles.md#start-the-control-plane):

```bash
# Shared bearer secret between the server and its authorized clients.
export HODEI_MH_CONTROL_TOKEN="$(openssl rand -hex 32)"
# Name or ID of the operator-provisioned restricted proxy container.
export HODEI_MH_EGRESS_PROXY_CONTAINER="your-proxy-container"
export HODEI_MH_PROXY_URL="http://hodei-proxy:8888"
export HODEI_MH_PROXY_CHECK_URL="http://127.0.0.1:8888"
PORT=8081 $BIN/hodei-mh-lifecycle-server &
```

The server listens on loopback only (a non-loopback `HOST` is refused),
reads the control credential from `HODEI_MH_CONTROL_TOKEN`
(`crates/opensandbox/src/control.rs`, `CONTROL_TOKEN_ENV`) and serves
the lifecycle API the product's default `--server http://127.0.0.1:8081`
points at.

### 2.3 Watch the gates: `doctor` and `profile-run --profile rust`

`doctor` is read-only and states the gate; with no proxy and no server
answering, the real output on this host is:

```bash
$BIN/hodei-mh doctor --profile rust
```

```
[ok] profile-pinned: rust:1-bookworm@sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea
[ok] pinned-image-local: pinned sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea; local sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea
[MISS] restricted-egress: egress proxy NOT reachable — builds needing the network would refuse to run
[note] sandbox-egress-isolation: not measured: isolation must be verified from inside a sandbox (this check reports unknown, not a pass)
[MISS] opensandbox-api: lifecycle API unreachable — no sandbox can be created
[ok] subscription-limitations: tier none: no credentials configured; networked builds will fail to fetch dependencies
gate: REFUSE — no native (YOLO) fallback exists
  missing: opensandbox lifecycle api
  missing: restricted egress path (proxy)
```

Exit `1` — a check that cannot run is a failure, not a skip. With the
proxy and the server up, the gate answers `gate: PROCEED` (exit `0`).

`profile-run --profile rust` runs one project's build plan inside the
pinned sandbox and prints a single JSON record. It requires `--project
DIR` (see the `USAGE` note below); run it from a checkout of the project
to build:

```bash
$BIN/hodei-mh profile-run --profile rust --project .
```

When the gate refuses, the record says exactly what is missing and what
did **not** happen — captured live on an unprepared host (exit `1`):

```json
{
  "build_executed_on_host": false,
  "gate": "REFUSE",
  "image": "rust:1-bookworm@sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea",
  "missing": [
    "opensandbox lifecycle api",
    "restricted egress path (proxy)"
  ],
  "note": "no credentials configured; networked builds will fail to fetch dependencies",
  "profile": "rust",
  "yolo_fallback": false,
  "checks": [ "…one entry per doctor check…" ]
}
```

(The full `checks` array mirrors the doctor lines above; the record also
carries `secret_leak_positions_in_commands` — the run record is checked
for leaks before it is written.) On a successful run the record carries
the requested **and observed** image digest and limits, the egress
measurement taken from inside the sandbox, the redacted command records
and the cache report — see
[cli-reference.md](cli-reference.md#profile-run).

*`USAGE` note:* the help line shows only `profile-run --profile <name>`
(with `--export-untracked` in parentheses). The implementation
(`profile_run.rs::run_profile`) additionally **requires** `--project
DIR`: without it the command exits `2` with `--project DIR is required`.
Other accepted flags (`--server`, `--proxy-url`, `--cpu`, `--memory`,
`--cache*`) are in the reference.

### 2.4 Start the live controller: `supervisor --live`

Every flag below is from `USAGE` lines 73–81 (`main.rs`); the five
required ones are enforced in `live/mod.rs::config_from_args`, and the
live configuration is wired **before** the socket is bound: a controller
that cannot execute never takes the project's identity.

```bash
$BIN/hodei-mh supervisor --live \
  --socket "$SOCK" --project hodei-mh \
  --server http://127.0.0.1:8081 \
  --image 'rust:1-bookworm@sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea' \
  --project-dir /path/to/project \
  --tracker-dir /path/to/project \
  --managed-branch integration \
  --proxy-url http://hodei-proxy:8888 \
  --permission allow-once
```

| Flag | Required with `--live`? | Meaning |
|---|---|---|
| `--server <url>` | yes | the lifecycle API (`http://127.0.0.1:8081` elsewhere by default) |
| `--image <ref>` | yes | the pinned sandbox image the sandboxes run |
| `--project-dir <path>` | yes | the host repository that owns the managed branch |
| `--tracker-dir <path>` | yes | the directory the Backlog.md CLI runs from |
| `--managed-branch <branch>` | yes | the branch verified results promote to |
| `--proxy-url <url>` | no | defaults to `http://hodei-proxy:8888` (`profile_run::DEFAULT_PROXY_URL`) |
| `--permission allow-once\|deny` | no | the ACP permission policy; default `allow-once`; any other value is refused |
| `--agent claude-code\|qoder` | no | the ACP agent runtime the session drives; default `claude-code`; any other value is refused |

The `--agent` flag only chooses the runtime; the credential channel does
not change: `qoder` reads `QODER_PERSONAL_ACCESS_TOKEN` from the
environment exactly the way `claude-code` reads `ANTHROPIC_AUTH_TOKEN`
(see §3).

A successful boot answers with the same shape as the rehearsal, naming
the live runtime:

```
supervisor listening on /tmp/hodei-quickstart.sock; logs identify provenance, credentials are redacted; runtime: live (hodei-mh)
```

and checkpoint recovery notes may follow on stderr. Then drive the loop
with the same clients as before:

```bash
$BIN/hodei-mh sup-action start --socket "$SOCK" --ticket TASK-9
$BIN/hodei-mh sup-status --socket "$SOCK"
$BIN/hodei-mh sup-events --socket "$SOCK" --jsonl
$BIN/hodei-mh attach-terminal --socket "$SOCK"   # read-only event tail
```

The refusals are per-fact, with exit `1` (captured against this host's
binary):

```
refusing to start: --live and --rehearsal are exclusive (a rehearsal measures nothing; a live run executes)
refusing the live supervisor: --server is required with --live        # or --image, --project-dir, --tracker-dir, --managed-branch
refusing the live supervisor: ANTHROPIC_AUTH_TOKEN is empty: the live runtime takes its credentials from the environment only
refusing the live supervisor: the flag --permission has the value 'bogus': expected allow-once or deny
refusing the live supervisor: the flag --agent has the value 'codex': expected claude-code or qoder
```

(One wording note: `USAGE` lists the values as `allow-once|deny`; the
refusal message prefixes the flag with "the flag", a cosmetic difference
from the phrasing used in the reference doc — the behaviour is the same.)

## 3. Credentials travel ONLY in the environment

This is a hard invariant, not a convention
(`crates/hodei-mh-profiles/src/credentials.rs`):

```rust
/// The ONLY authorized delivery channel. Adding another one is a design
/// change, not a convenience.
pub enum CredentialChannel {
    ProcessEnvOnly,
}
```

- Credentials reach the sandbox **only** through the process environment
  of the one process that needs them. They are never flags or argv, never
  written to files, build commands, logs, state, snapshots or handoffs.
  `SECRET_KEYS` (`ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_API_KEY`,
  `GATEWAY_AUTH_TOKEN`, `REGISTRY_PASSWORD`) are scrubbed to `[redacted]`
  in any observable output, together with value shapes that look like
  secrets (`sk-ant-…`, `ghp_…`, credentials embedded in URLs).
- The live supervisor therefore **refuses to boot** when
  `ANTHROPIC_AUTH_TOKEN` is empty (see 2.4) — the credential must already
  live in the environment of the process you start.
- The lifecycle API's bearer secret is the same rule:
  `HODEI_MH_CONTROL_TOKEN` in the environment of the server and of every
  authorized client (`crates/opensandbox/src/control.rs`); the server
  refuses to start without it.
- Supervisor logs and event payloads are redacted **before** they are
  recorded or sent on the socket, at any depth
  ([supervisor-protocol.md §5.1](supervisor-protocol.md#51-events-keep-their-structure)).

So the correct way to provide a credential is, e.g., to have your secret
manager place it in the shell that starts the supervisor:

```bash
read -rs ANTHROPIC_AUTH_TOKEN && export ANTHROPIC_AUTH_TOKEN
```

and never:

```bash
$BIN/hodei-mh supervisor --live --token 'sk-ant-…'   # no such flag exists
```

## 4. Where to go next

| Question | Document |
|---|---|
| What exactly does each flag refuse? | [cli-reference.md](cli-reference.md) |
| How is the socket authenticated, bounded, versioned? | [supervisor-protocol.md](supervisor-protocol.md) |
| How are profiles pinned and egress enforced? | [profiles.md](profiles.md) |
| Which environment variables exist and at what precedence? | [configuration.md](configuration.md) |
