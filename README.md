# hodei-meta-harness

Hodei (short: **hodei-mh**) is a lightweight, local-first, sandbox-first meta-harness
driven by persistent goals. Agents run inside disposable OpenSandbox environments,
speak ACP v1, and promote verified Git results back to the host.

## Install

The v0.1.1 release ships **standalone** binaries and an installable bundle:

| Asset | SHA-256 |
|---|---|
| `hodei-mh-v0.1.1-linux-x86_64` | `9e07bd433984120ed022f116c67184746d814359c160b15ebb157b4466da1426` |
| `hodei-mh-lifecycle-server-v0.1.1-linux-x86_64` | `d70dc5fb990592ecccfe183be5acd01a9d552179b665e6313b70abe3d20bcec7` |
| `hodei-mh-v0.1.1-linux-x86_64.tar.gz` | `5980983671a9726e0ed89a1c8c421e9d50798053595c5a4a6bcabbd89267a486` |

Requirements: Docker (the sandbox backend), a host that can pull the pinned images,
and the restricted-egress proxy for sandboxes (see
[docs/profiles.md](docs/profiles.md); the repository ships one at
`scripts/egress-proxy.py`). The E2E probes that drive the real tracker also
need the official Backlog.md CLI (`npm install -g backlog.md`).

### 1. Download

```bash
REPO=Rubentxu/hodei-meta-harness
TAG=v0.1.1
BASE=https://github.com/$REPO/releases/download/$TAG

# the installable bundle plus the manifest of digests
curl -L -O "$BASE/hodei-mh-v0.1.1-linux-x86_64.tar.gz"
curl -L -O "$BASE/SHA256SUMS"

# or the binaries on their own
curl -L -O "$BASE/hodei-mh-v0.1.1-linux-x86_64"
curl -L -O "$BASE/hodei-mh-lifecycle-server-v0.1.1-linux-x86_64"
chmod +x hodei-mh-v0.1.1-linux-x86_64 hodei-mh-lifecycle-server-v0.1.1-linux-x86_64
```

`curl` writes what the server sends, and the server sends no executable bit: the
binaries taken one by one need `chmod +x` before they can run. The bundle needs
nothing — its binaries already carry the bit, and `install.sh` copies them as they
are.

### 2. Verify the checksums

`SHA256SUMS` carries the digests above. With every published asset in the same
directory:

```bash
LC_ALL=C sha256sum -c SHA256SUMS
```

```
hodei-mh-v0.1.1-linux-x86_64: OK
hodei-mh-lifecycle-server-v0.1.1-linux-x86_64: OK
hodei-mh-v0.1.1-linux-x86_64.tar.gz: OK
```

If you only took the bundle, check it against the digest published in the release
notes:

```bash
echo "5980983671a9726e0ed89a1c8c421e9d50798053595c5a4a6bcabbd89267a486  hodei-mh-v0.1.1-linux-x86_64.tar.gz" | sha256sum -c -
```

### 3. Install outside the repository

The bundle is self-contained: `bin/`, `docs/profiles.md`, `install.sh`,
`INSTALL.md` and `SHA256SUMS`, nothing else. Unpack it anywhere and run the
installer with the prefix you want — the repository is not needed at runtime:

```bash
mkdir hodei-mh && tar xzf hodei-mh-v0.1.1-linux-x86_64.tar.gz -C hodei-mh
cd hodei-mh
LC_ALL=C sha256sum -c SHA256SUMS    # the bundle's own digests, offline
sh install.sh                        # into ~/.local/bin
sh install.sh /opt/hodei-mh          # or into any prefix you can write to
```

```
installed to /opt/hodei-mh/bin (hodei-mh, hodei-mh-lifecycle-server)
```

Unpack the bundle into a directory of its own: it ships a `SHA256SUMS` of its
own, and extracting it next to the downloaded manifest would replace the one
that certifies the download. The bundle's manifest covers the bytes **inside**
it, so the two checks together seal the whole path: the download against the
published digest, and the unpacked files against the bundle's.

The binaries land in `<prefix>/bin` and the documentation in
`<prefix>/share/hodei-mh/docs`. Nothing else is required: no runtime, no shared
library, no plugin.

## Why the binaries need no system libraries

The release binaries are **statically linked**: the kernel loads the image
directly, with no dynamic loader in between. They carry no `PT_INTERP` segment and
no `DT_NEEDED` entry, so they need **no glibc, no libgcc and no other system
library** from your distribution — only the Linux kernel (3.2 or newer). That is
what makes them usable on a distro whose glibc is *older* than the one they were
built against, which is the usual way a binary like this stops working.

You can check that claim yourself, on the file you downloaded:

```bash
file hodei-mh
# ELF 64-bit LSB pie executable, x86-64, ..., static-pie linked, for GNU/Linux 3.2.0

ldd hodei-mh
#	statically linked

readelf -l hodei-mh | grep -c INTERP
# 0        (no dynamic loader)

readelf -d hodei-mh | grep -c NEEDED
# 0        (no shared library required)
```

The same binaries were run in an image built `FROM scratch` — no libc, no loader,
no shell — and answered `hodei-mh 0.1.0`. `hodei-mh package` refuses to build a
bundle from binaries that need a dynamic loader, and its manifest reports the
measured fact instead of asserting it.

## First run

```bash
PORT=8081 hodei-mh-lifecycle-server &              # the control plane
hodei-mh doctor --profile rust                    # read-only checks + gate
hodei-mh profile-run --profile rust --project .   # a real build in a sandbox
hodei-mh supervisor --project hodei-mh &          # the control socket lives
hodei-mh sup-status && hodei-mh tui               # in the private runtime dir
```

The control surface — where the socket lives, who may talk to it and what
every exchange is bounded by — is documented in
[docs/supervisor-protocol.md](docs/supervisor-protocol.md).

## Build from source

```bash
cargo build --release -p hodei-mh -p hodei-mh-lifecycle-server
# -> target/x86_64-unknown-linux-gnu/release
```

The build is standalone by default (`.cargo/config.toml` links statically), and the
workspace is **pinned to `x86_64-unknown-linux-gnu`**: the product runs on Linux
sandboxes, so every cargo command targets that triple.

`hodei-mh package` assembles the installable bundle from the **release** profile
(a debug build is never packaged), names it after the platform the binaries were
compiled for, and **refuses** to package binaries that carry a dynamic loader.

## What it guarantees

- Builds run inside disposable sandboxes with the PINNED image; there is no native
  execution fallback (`yolo_fallback: false`).
- The effective CPU/memory limits are read from inside the container, and the
  sandbox's own egress is measured from inside it.
- Credentials only travel in the environment of the process that needs them: never
  on disk, in the repo, in logs or in records.
- The lifecycle server enforces a global budget of concurrent environments
  (`MAX_CONCURRENT_ENVIRONMENTS`), refusing over-budget creates with
  `429 SANDBOX::BUDGET_EXHAUSTED`. The budget is reserved atomically before any
  effect, so simultaneous creates cannot overshoot it, and a create carrying a
  `creationKey` never produces two sandboxes (`docs/profiles.md`, "Global
  environment budget").
- The coordinator of a project keeps its claims and its effect intents in a SQLite
  state file under the user's state directory (`$XDG_STATE_HOME/hodei-mh/<project>/`,
  or `~/.local/state/hodei-mh/<project>/`), never inside the workspace. A claim is
  committed before it is granted: a write that fails grants nothing.

Everything above is demonstrated by the repository's E2E probes and by the tests of
the crate that owns each guarantee, not asserted.

## Troubleshooting

Every refusal below is the product's real behaviour: the signals quoted here
are the strings the code prints. A refusal is never a silent degradation —
the answer says what is missing and nothing else happens.

### The gate refuses: no restricted egress

**Signal.** `hodei-mh doctor` prints

```
[MISS] restricted-egress: egress proxy NOT reachable — builds needing the network would refuse to run
gate: REFUSE — no native (YOLO) fallback exists
  missing: restricted egress path (proxy)
```

and exits `1`; `profile-run` writes a record with `"gate": "REFUSE"` and
`"missing": ["restricted egress path (proxy)"]`.

**Cause.** The reachability check is a TCP connect to the proxy
(`HODEI_MH_PROXY_CHECK_URL`, falling back to `HODEI_MH_PROXY_URL`, default
`http://hodei-proxy:8888`), and it did not answer. The gate has no native
fallback: without the restricted egress path the build simply does not
happen.

**Fix.** Start the shipped proxy (`scripts/egress-proxy.py`) or point
`--proxy-url` at the trusted one you already run. For the probe lane,
`scripts/probes.sh --preflight` fails with an explicit reason when the
operator variables are unset or the proxy container is not running — set
`HODEI_MH_EGRESS_PROXY_CONTAINER` and `HODEI_MH_PROXY_CHECK_URL`. The
lifecycle server itself refuses to start without
`HODEI_MH_EGRESS_PROXY_CONTAINER`: `HODEI_MH_EGRESS_PROXY_CONTAINER must
identify the trusted proxy container`.

### The profile image is not in the local daemon

**Signal.**

```
[MISS] pinned-image-local: <ref> not present locally — the docker daemon must pull it first (the profile never runs without it)
```

(or, for a user image, `user image <ref> not present locally — the docker
daemon must pull it first`), followed by
`missing: pinned image (pull required; the daemon fetches it, the profile
never runs without it)`.

**Cause.** The doctor is strictly read-only: it inspects the daemon but
never pulls, so an absent pinned image is a hard stop, not something the
run fixes on the way. A digest mismatch is the sibling failure — the check
reports `pinned <digest>; local <digest> (MISMATCH)` and the gate refuses
with `local image digest does not match the pinned digest`; a foreign
image is never trusted.

**Fix.** `docker pull` the exact reference the message names (the full
`name@sha256:...` form), then re-run the doctor. It turns to
`gate: PROCEED` only when the local digest matches the pin.

### `429 SANDBOX::BUDGET_EXHAUSTED` on a sandbox create

**Signal.** The lifecycle server answers the create with HTTP `429` and the
body `detail.code` `SANDBOX::BUDGET_EXHAUSTED`; the message names the
exhausted dimension, e.g. `global environment budget exhausted: N of N
slots in use; no environment is created and the caller must WAIT (no
silent admission)`. In `profile-run` the create is retried once with the
same request tag and the second refusal is reported as
`sandbox create retry returned 429 Too Many Requests`, exit `1`.

**Cause.** The server enforces a global budget in the create path, reserved
atomically before any effect — so the refusal created nothing, and
simultaneous creates cannot overshoot. The slots budget comes from
`MAX_CONCURRENT_ENVIRONMENTS` (unset means no limit); the aggregate memory
and CPU budgets from `SANDBOX_TOTAL_MEMORY_BYTES` and
`SANDBOX_TOTAL_NANOCPUS`.

**Fix.** Wait for an environment to be released, or raise
`MAX_CONCURRENT_ENVIRONMENTS` in the lifecycle server's environment and
restart it. See [docs/configuration.md](docs/configuration.md) for every
variable and [docs/cli-reference.md](docs/cli-reference.md) for the client
behaviour around the retry.

### The credential does not reach the sandbox agent

**Signal.** The live runtime refuses at boot:
`ANTHROPIC_AUTH_TOKEN is empty: the live runtime takes its credentials
from the environment only`. A networked `profile-run` with no credentials
instead reports the limitation it carries:
`[note] subscription-limitations: none: no credentials configured;
networked builds will fail to fetch dependencies`.

**Cause.** `CredentialChannel::ProcessEnvOnly` is the only authorized
delivery channel: the token travels solely in the environment of the one
sandbox process that needs it. In the live loop it is handed over a single
stdin-line handshake that a staged wrapper `export`s before `exec`ing the
adapter — argv is never a credential channel, so the session command, the
process table and every log stay secret-free by construction. An empty or
whitespace value in the host's environment is a hard error
(`MissingCredentials`), never a silent unauthenticated run.

**Fix.** `export ANTHROPIC_AUTH_TOKEN` in the environment of the host
process that starts the supervisor or the live loop before launching it
(the variables are listed in
[docs/configuration.md](docs/configuration.md)). Never put a token in a
command line, a file or the repo — records redact any that leaks anyway
(`[redacted]`).

### `hodei-mh package` refuses your binaries

**Signal.**

```
refusing to package non-standalone binaries: <path>: NeedsDynamicLoader, ... — build them statically (cargo build --release with -C target-feature=+crt-static) or the bundle would need the host's glibc
```

**Cause.** The linkage is read from the ELF bytes, never assumed: a binary
carrying a `PT_INTERP` segment needs the host's dynamic loader and its
system libraries, which would make the bundle unusable on an older distro
than the build host. Related refusals: `refusing to package a debug build`
(only the release profile ships), an architecture mismatch
(`refusing to package <path>: it is a <arch> binary, but the artifact
would be named <os>-<arch>`), and a missing product document
(`refusing to package without the product's documentation`).

**Fix.** Rebuild statically: `cargo build --release` already applies
`-C target-feature=+crt-static` through `.cargo/config.toml`; a hand-built
command must carry the flag itself. Verify before repackaging:
`readelf -l <bin> | grep -c INTERP` must print `0`.

### More documentation

- [docs/cli-reference.md](docs/cli-reference.md) — every subcommand, flag, refusal and exit code.
- [docs/configuration.md](docs/configuration.md) — the environment variables each lane reads, with their code anchors.
- [docs/quickstart.md](docs/quickstart.md) — a narrated walkthrough of one full turn of the objective loop.

## Status

The task board lives in `backlog/` (Backlog.md is the single source of truth).
`docs/profiles.md` documents the profiles, the doctor, the budget and the
installation contract in detail.

The audit of v0.1.0 became the v0.2 consolidation plan and lives on the same
board: thirteen tasks with their acceptance criteria, their code references and
their technical dependencies (TASK-24..TASK-36). The order is the one the
dependencies allow — the guarantees first, and the autonomous execution on top
of them.
