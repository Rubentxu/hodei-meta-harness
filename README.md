# hodei-meta-harness

Hodei (short: **hodei-mh**) is a lightweight, local-first, sandbox-first meta-harness
driven by persistent goals. Agents run inside disposable OpenSandbox environments,
speak ACP v1, and promote verified Git results back to the host.

## Install

Grab the release asset and unpack it:

```bash
curl -L -O https://github.com/Rubentxu/hodei-meta-harness/releases/download/v0.1.0/hodei-mh-v0.1.0-linux-x86_64.tar.gz
tar xzf hodei-mh-v0.1.0-linux-x86_64.tar.gz
sh install.sh            # binaries into ~/.local/bin
```

The binaries are **standalone**: statically linked, with no dynamic loader, so they
need no glibc, no libgcc and no other system library from your distro. They run on
any Linux x86_64 (kernel 3.2+), whatever glibc it ships.

Requirements: Docker (the sandbox backend), a host that can pull the pinned
images, and the restricted-egress proxy for sandboxes (see
[docs/profiles.md](docs/profiles.md)).

## First run

```bash
PORT=8081 hodei-mh-lifecycle-server &              # the control plane
hodei-mh doctor --profile rust                    # read-only checks + gate
hodei-mh profile-run --profile rust --project .   # a real build in a sandbox
hodei-mh supervisor --socket /tmp/hodei-mh-supervisor.sock --project hodei-mh &
hodei-mh sup-status && hodei-mh tui               # observe and drive
```

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
  `429 SANDBOX::BUDGET_EXHAUSTED`.

Everything above is demonstrated by the repository's E2E probes, not asserted.

## Status

The task board lives in `backlog/` (Backlog.md is the single source of truth).
`docs/profiles.md` documents the profiles, the doctor, the budget and the
installation contract in detail.
