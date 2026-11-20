# Overview — what this is, what it is for, and how to start

This document is for a reader with **zero context**. It answers "what is
this, what is it for, how does it work, how do I start" before it shows
you a single command line. Every guarantee here is anchored to a document
or a code path that already states it; nothing is promised that the
product does not do.

## 1. The one-paragraph answer

**hodei-meta-harness** (**hodei-mh**) is a local-first harness that puts
AI programming agents to work on your real task board — inside disposable,
network-restricted Docker sandboxes — and promotes **only verified results**
back into your repository. If you want an analogy: it is a **foreman who
hires AI programmers and gives each one a locked, throwaway workshop**.
The agent writes code inside its workshop (a sandbox); when it finishes,
the foreman inspects the piece (the candidate) in a *second*, clean
workshop of known shape; and only a piece that passes inspection is
placed on your shelf (the managed branch). The workshops are then burned
down. Nothing the agent did can touch your working tree, and nothing
unverified can touch your branch.

## 2. The words this document cannot avoid

Defined once, used everywhere:

- **Sandbox**: a temporary Docker container that the agent (or the
  verifier) works inside. It is created for one job and destroyed after
  it — your host's files, network and credentials are not reachable from
  it except through explicitly granted, redacted channels.
- **ACP**: the Agent Client Protocol, version 1 — the JSON-RPC language
  an agent speaks with its client (`initialize`, `session/new`,
  `session/prompt`, permission requests). hodei-mh is the client; any
  ACP v1 agent can be the worker.
- **Candidate**: the proposed change a run produced, carried as a Git
  bundle with a declared revision and content identity, awaiting
  verification. It is data, not a live workspace: it cannot execute
  anything until a verifier materializes it in a clean sandbox.
- **Managed branch**: the branch on your host repository that only
  verified results are promoted onto (e.g. `integration`). It is the
  product's sole write target on your side.
- **Objective loop** (a.k.a. goal loop): one full turn of the system —
  select a ticket, work it, verify the candidate, publish the result.

## 3. How one turn works

The loop has four beats, and each beat runs where it is safe to run:

1. **Select** — the supervisor picks eligible work from the real
   Backlog.md tracker (or accepts a custom objective you gave it).
2. **Work** — an ACP agent session runs *inside a sandbox* over the
   `execd` data plane. Whatever it does — builds, edits, network
   attempts — stays inside the container.
3. **Verify** — the candidate is materialized **from its Git bundle
   inside a throwaway sandbox** (never your working tree), its identity
   is checked *before* any check runs, and only then are the policy
   commands executed there. See `crates/hodei-mh-verify/src/runner.rs`
   (module docs) and [profiles.md](profiles.md).
4. **Publish** — only a verified candidate is promoted to the managed
   branch, and the ticket is closed through the official tracker CLI.
   The promotion intent is journaled *before* any effect, so a crash
   resumes the same operation instead of applying it twice
   (`crates/hodei-mh-promote/src/flow.rs`, module docs).

## 4. The three guarantees

The product's value reduces to three promises. Each is enforced by code
and documented where stated — they are not aspirations.

### 4.1 Isolation: a locked workshop with a monitored door

Builds and agent work run inside disposable sandboxes on a pinned image,
with per-sandbox isolated egress through a restricted proxy. There is
**no native fallback**: if the sandbox, the pinned image or the egress
path is unavailable, the gate says `REFUSE` and the build simply does
not happen (`yolo_fallback: false`).
Where it is stated: [profiles.md](profiles.md) (prerequisites, the trust
boundary, the doctor gate) and the refusal output in
[quickstart.md §2.3](quickstart.md#23-watch-the-gates-doctor-and-profile-run---profile-rust).

### 4.2 Verification: the piece is weighed in a clean room, identity first

The candidate is never trusted because the agent produced it. The clean
runner materializes it from the Git bundle inside a throwaway sandbox
and refuses to run a single check until the transport matches its
declared digest, the checkout's head matches the declared revision, and
the checkout's content matches the declared content. A wrong candidate
is rejected **before the checks**, so it can never produce evidence; a
timeout or lost session is recorded as ABSENT, never as a PASS
(`crates/hodei-mh-verify/src/runner.rs`, module docs). Only the evidence
bound to a verified candidate can authorize promotion
([supervisor-protocol.md](supervisor-protocol.md) for how that evidence
reaches the controller).

### 4.3 Honest memory: what happened is recorded, what is unknown is said

Three mechanisms keep the record truthful:

- **A durable event journal.** Every action and outcome is a structured
  event with provenance and credential redaction applied *before* it is
  recorded; `sup-events --jsonl` replays exactly what the socket
  carries, follows the pagination cursor, and **announces a hole in the
  retained history instead of hiding it**
  ([cli-reference.md, `sup-events`](cli-reference.md#sup-events);
  [supervisor-protocol.md §5](supervisor-protocol.md#5-the-wire-contract-protocol-version-2)).
- **One controller, by two locks.** A socket path admits exactly one
  supervisor: it holds two exclusive kernel locks (socket path and
  project identity), so a second controller is *refused*, not silently
  forked — there is never a split memory of the run
  ([supervisor-protocol.md §2](supervisor-protocol.md#2-identity-two-exclusive-locks-no-name-probe)).
- **Budget, refused with a reason.** The lifecycle server reserves its
  global environment budget atomically before any effect and answers an
  over-budget create with `429 SANDBOX::BUDGET_EXHAUSTED` and the
  instruction that the caller must **WAIT** — no silent admission, no
  double sandbox ([profiles.md §Global environment
  budget](profiles.md#global-environment-budget);
  [configuration.md](configuration.md) for `MAX_CONCURRENT_ENVIRONMENTS`,
  `SANDBOX_TOTAL_MEMORY_BYTES`, `SANDBOX_TOTAL_NANOCPUS`).

The same honesty extends to uncertainty itself: a live run's durable
checkpoint reports a pending effect as *uncertain* and refuses the next
attempt rather than duplicating an effect
(`crates/hodei-mh/src/live/checkpoint.rs`, module docs;
[cli-reference.md, `supervisor` refusals](cli-reference.md#supervisor)).

## 5. One sentence per piece

| Piece | The role in the analogy |
|---|---|
| `hodei-mh-lifecycle-server` | **The concierge** — creates, monitors and destroys the sandboxes on your Docker host, on authority of one bearer secret and under the global budget. |
| `supervisor` (the `hodei-mh` CLI's control surface) | **The foreman** — the single controller of one project: it starts runs, records every event, and is the only thing that talks to your board and branch. |
| `execd` | **The hands inside** — the Go agent living in each sandbox that actually executes commands, reached over a WebSocket pipe with an access token that is scoped to that one sandbox. |
| `doctor` + the profiles | **The gatekeeper** — read-only checks that measure the image pin, the egress path and the API before anything runs, and refuse (never degrade) when a guarantee is missing. |

## 6. How to start: the three-step ladder

Each rung is real and each costs more than the last. Do them in order —
the exact commands and their real outputs live in
[quickstart.md](quickstart.md); they are named here, not reinvented.

**Rung 1 — the rehearsal (no Docker, no network, no model).**
Drive a complete objective loop against the deterministic rehearsal
runtime: `hodei-mh supervisor --rehearsal ...`, then `sup-action start`,
`sup-status`, `sup-events --jsonl`. It never closes a ticket and never
moves a ref, and every event says so. This is the fastest way to see the
whole shape of the loop. Full walkthrough:
[quickstart.md §1](quickstart.md#1-the-rehearsal-route-no-docker-no-network-no-model).

**Rung 2 — one real sandbox (a build, still no agent).**
`hodei-mh doctor --profile rust` reads the gate; `hodei-mh profile-run
--profile rust --project .` runs one build plan inside the pinned
sandbox and writes a single JSON record of what it measured — or exactly
what is missing and did **not** happen. Prerequisites and command lines:
[quickstart.md §2.2–2.3](quickstart.md#22-start-the-lifecycle-server);
gate semantics: [profiles.md](profiles.md).

**Rung 3 — the live loop (board + managed branch + real agent).**
`hodei-mh supervisor --live` with the five required flags wires the real
Backlog.md tracker, the real sandboxes and the real managed branch, and
the agent's credential comes from the environment only. Then you drive
and watch the same way you did in the rehearsal. Configuration, flags and
refusals:
[quickstart.md §2.4](quickstart.md#24-start-the-live-controller-supervisor---live)
and [cli-reference.md](cli-reference.md#supervisor).

A cardinal rule across all three rungs: **credentials travel only in the
process environment** — never as flags, never in files, logs or records
([quickstart.md §3](quickstart.md#3-credentials-travel-only-in-the-environment)).

## 7. Where to read next

| You want | Go to |
|---|---|
| Run the loop once, with real outputs | [quickstart.md](quickstart.md) |
| The exact command surface: every flag, refusal, exit code | [cli-reference.md](cli-reference.md) |
| Which environment variable affects which process | [configuration.md](configuration.md) |
| How the sandbox gate measures pinning, egress and budget | [profiles.md](profiles.md) |
| The socket contract: identity, bounds, wire version, cursor | [supervisor-protocol.md](supervisor-protocol.md) |
| Install, checksums, troubleshooting recipes | the [top-level README](../README.md) |
