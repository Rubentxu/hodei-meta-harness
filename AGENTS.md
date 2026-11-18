# hodei-meta-harness (hodei-mh) — Agent Guide

hodei-meta-harness (short hodei-mh) is a lightweight, local-first,
sandbox-first meta-harness driven by persistent goals. Agents (ACP v1) run
inside disposable OpenSandbox environments; verified Git results are
promoted back to the host.

## Product identity

- Name: **hodei-meta-harness**, short **hodei-mh**. `hodei` alone is the
  company prefix and is never used as a bare product or crate name.
- The legacy intake keys `GC-XX` stay as import provenance; native task IDs
  are `TASK-N`.

## Workflow authority and skills

**Backlog.md is the single tracking authority.** Tasks, plans, decisions,
states, dependencies and evidence live in Backlog.md and are managed only
through its official CLI (`backlog ...`). Never hand-edit task markdown.
Never create parallel tracking files (no ROADMAP.md, no ADR files, no
progress reports).

When working in this repository, agents should use these skills:

| Skill | When |
|---|---|
| `backlog-agile-workflow` | Always. Task lifecycle: read board, pick work, branch, implement, verify with evidence, close. Anti-stall: after two sterile iterations, change strategy and record it. |
| `pure-functional-design` | Before and during implementation. Functional core design: domain ADTs, invariants, effect separation, pure algorithm selection. |
| `pipelinek-local-ci` | Local CI gates (optional). The project's own gates are the validation lanes below. |
| `goldfish-review` | Independent review of designs/plans/theses with fresh-context subagents before merge of architecturally significant work. |

## Work cycle

Repeat without asking permission between steps:

1. **Read the backlog and pick the best-fitting ready task.** Read real
   state with the Backlog.md CLI from the repository root; choose a task
   that is ready, high value, and has no pending human decision.
2. **One branch per task.** Branch `task/task-<id>-<slug>` from `main`.
   Do not mix unrelated work into a task branch.
3. **Implement with TDD.** Failing test first, then code; plus an
   integration test only over the affected code, not the whole suite.
4. **Fix what you find.** Any problem or tech debt discovered during the
   task gets fixed inline when it fits, or registered as a task labeled
   `descubrimiento` when it does not.
5. **Integrate.** With verification green, merge to `main` via PR
   (implicit standing authorization) and clean up local and remote task
   branches. Pushing is never an excuse to skip checks: if something
   fails, stop and report instead of pushing.
6. **Update the backlog truthfully** at every transition (state, ACs,
   evidence, final summary), then go back to step 1.

Full validation gates (fmt, clippy `-D warnings`, full test suite) run
every 10 completed tasks; the lane that fits the change runs per task. The
lanes are defined once, in [Validation lanes](#validation-lanes).

## Code standard: functional paradigm (Rust like Haskell)

The implementation follows a functional paradigm: a pure domain core with
effects pushed to the edges.

1. **Pure core, effectful shell.** Domain logic (composition, validation,
   framing, protocol) lives in total functions with no I/O. I/O lives in
   thin adapters (`docker.rs`, `http.rs`, transport) that translate
   side effects into data.
2. **Model with ADTs.** Use enums and structs so illegal states are
   unrepresentable. Prefer closed enums over magic strings; parse, don't
   validate (convert raw input into typed values once, at the edge).
3. **Immutability by default.** No interior mutability in the domain core.
   Build new values instead of mutating; mutation is allowed only inside
   adapters that own isolated state.
4. **`Option`/`Result` discipline.** No `unwrap()`/`expect()` in production
   code paths (tests may). Errors are explicit types (`thiserror`), no
   stringly-typed failures. Functions that can fail say so in the type.
5. **Totality.** Match exhaustively; no panics as control flow. Prefer
   returning "not implemented / unsupported" as explicit values where the
   contract requires it (e.g. honest `501` responses).
6. **Iterator pipelines over manual loops** where they express intent
   better; keep helper functions small, named, and referentially
   transparent.
7. **TDD.** Unit tests sit next to the pure modules they verify and must
   run offline. Integration tests require real infrastructure (Docker,
   sandbox server) and are clearly separated.
8. **Tooling gates.** `cargo fmt`, `cargo clippy --all-targets -- -D
   warnings` and `cargo test` before every merge to `main`. The lanes below
   decide which of them run, and how often.

## Validation lanes

Three gates, with different costs. None of them is allowed to fail open:
a check that cannot run is a failure, not a skip.

| Gate | What it runs | Cost | When |
|---|---|---|---|
| **local** | `scripts/fast-check.sh`: `fmt`, `clippy` and the tests of the crate you touched **and the crates that depend on it** | seconds, no Docker | while you work, before every merge |
| **workspace** | the CI `fast` job: `fmt`, `clippy -D warnings`, the whole test suite and the doctests | minutes, no Docker | on every push and pull request |
| **integration** | `scripts/probes.sh`: the probes (`release`, `supervisor`, `profile`, `multiproject`, `verify`) against real Docker and the lifecycle server | minutes, starts sandboxes and runs real builds | on `main`, on demand, and locally before publishing |

```bash
scripts/fast-check.sh                                  # what changed vs the base
scripts/fast-check.sh crates/hodei-mh-verify/src/runner.rs
scripts/fast-check.sh --all                            # the whole workspace
scripts/probes.sh --rounds 3                           # the expensive lane
```

The merge gate is the **local** gate for the crates you touched; the
**workspace** gate completes it on every push, so a merge never depends on
the affected-crate selection being right. The **integration** lane adds the
probes when the change touches the sandbox, the ACP flow or the release.
The probes are expensive on purpose, and they are kept in their own lane so
the cheap gate never hides their cost.

Two rules about those probes:

- **A probe is only removed when its invariant is asserted in the productive
  path.** The probe is the evidence; the productive path is the product. If
  the invariant is not enforced by the code the consumers use, the probe
  stays.
- **Failure paths are tested with fakes, not with tokens and not with
  sleeps.** The ACP surface and the HTTP surface have offline fakes: the
  tests exercise the refusals without a provider, without a container and
  without waiting for anything. A test that sleeps to see whether something
  happens is a test that lies.

## Code quality and documentation (progressive application)

These rules apply **only to the code each task already touches**. They
never justify a global cleanup, a separate commit, or a dedicated task;
if something outside the scope violates them, leave it as is or register
it as a `descubrimiento`. A change that improves what is touched without
enlarging the diff is enough.

**Clean code**

- Self-descriptive English names that state intent; rename or extract
  before commenting.
- Small functions and methods with a single reason to change; extract
  when a block needs a comment to be understood.
- No duplication: a business rule or a name lives in exactly one place.
- No magic numbers or strings: named constants; no dead code, no
  commented-out code.
- Minimal nesting: guard clauses and early returns before cascading
  conditionals.
- Effects at the edges: pure logic separated from I/O; dependencies are
  received as parameters, never constructed deep inside.
- Errors say what failed, why, and how to fix it, with a specific cause;
  never a generic catch-all bucket and never a swallowed error.
- Every behavior change carries its test; the test describes observable
  behavior, not internal structure.

**Code documentation**

- Use the language-native format (rustdoc here) and only where it adds
  value.
- Public API: purpose, non-obvious parameters and returns, errors and
  refusals, effects, concurrency, compatibility. One line is enough when
  the name says almost everything.
- Implementation: comment only the *why* that code cannot express
  (surprising invariant, trust boundary, hard algorithm). Never narrate
  what the line already says.
- A comment that contradicts the code, or that tells history, steps or
  migrations, is removed or fixed when that code is touched.
- No task IDs, tickets, people or tracker references in code comments;
  that traceability lives in Backlog.md and Git. Domain identifiers
  (e.g. `RunId`), error codes, standards and technical links the reader
  needs are fine.
- User-facing and contract documentation is updated in the same task
  that changes the behavior it describes.

## Language conventions

- Code, identifiers, comments, docs, commit messages: English.
- Backlog task prose: Spanish (existing project convention).
- Commit messages reference the backlog task ID.

## Infrastructure notes

- Local control plane: `hodei-mh-lifecycle-server` (this repo's Rust port
  of the OpenSandbox Docker lifecycle subset). The Python
  `opensandbox-server` is not used at runtime by product code.
- Sandbox data plane: `execd` (Go, inside sandbox) reached over WebSocket
  pipe mode (`pty=0`): binary frames `0x00` stdin, `0x01` stdout,
  `0x02` stderr, `0x03` replay; JSON control frames
  (`stdin`/`signal`/`resize`/`ping`).
- No third-party CLI is used by product code paths: libraries, SDKs, or
  documented APIs only. CLIs are acceptable for interactive debugging and
  evidence gathering, never as a product dependency.
