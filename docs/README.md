# User documentation

Navigable index of the user-facing documentation for
**hodei-meta-harness** (**hodei-mh**). The project overview, install and
troubleshooting recipes live in the [top-level README](../README.md);
this folder holds the detailed reference documents.

## The documents

| Document | Purpose | Who it serves |
|---|---|---|
| [overview.md](overview.md) | The orientation read for a zero-context visitor: what the product is, the words it cannot avoid, the three guarantees (isolation, verification, honest memory) with their anchors, one sentence per piece, and the three-rung ladder from rehearsal to the live loop. | First contact: you want to know what this is and how it works before running anything. |
| [quickstart.md](quickstart.md) | A narrated walkthrough of one full turn of the goal loop with the real product commands, on two routes: the rehearsal (no Docker, no network, no model) and the live loop, plus the credentials-only-in-environment invariant. | First contact: you want to run the loop end to end and see the real outputs. |
| [cli-reference.md](cli-reference.md) | One section per `hodei-mh` subcommand (`doctor`, `profile-run`, `supervisor`, `sup-status`, `sup-action`, `sup-events`, `attach-terminal`, `tui`, `package`, probes): every flag, output shape, exit code and refusal, taken from the source. | You need the exact command surface: what a flag does and what gets refused. |
| [configuration.md](configuration.md) | The environment-variable reference, organized by the three lanes that read them — control plane (`hodei-mh-lifecycle-server`), supervisor & host harness, and the probes lane — with a source anchor per row and the required/optional distinction. | You are setting up a host and need to know which variable affects which process. |
| [profiles.md](profiles.md) | The reproducible Rust/JVM build profiles: prerequisites, install and packaging of standalone binaries, the trust boundary, the read-only doctor gate, the pinned images, profile runs with the opt-in build cache, and the global environment budget. | You run profile builds and need to understand what the gate measures before it proceeds or refuses. |
| [supervisor-protocol.md](supervisor-protocol.md) | The contract of the supervisor control surface: socket location and permissions, identity by two exclusive locks, peer authentication, the per-dimension bounds, the version-2 wire contract with events and cursor, and the command output quota. | You drive (or script) the controller socket and need the guarantees the surface makes. |

## Suggested reading order

1. [overview.md](overview.md) — what this is and how it works, before any command.
2. [quickstart.md](quickstart.md) — run the loop once.
3. [cli-reference.md](cli-reference.md) — look up what you just ran.
4. [configuration.md](configuration.md) — make it yours (variables per lane).
5. [profiles.md](profiles.md) — understand the sandbox gates behind a build.
6. [supervisor-protocol.md](supervisor-protocol.md) — the wire contract, for
   clients and automation.

Back to the project home: [README.md](../README.md).
