# hodei-meta-harness

Hodei (short: **hodei-mh**) is a lightweight, local-first, sandbox-first meta-harness
driven by persistent goals. Agents run inside disposable OpenSandbox environments,
speak ACP v1, and promote verified Git results back to the host.

## Install

The v0.1.0 release ships **standalone** binaries and an installable bundle:

| Asset | SHA-256 |
|---|---|
| `hodei-mh-v0.1.0-linux-x86_64` | `d1bcfb4e146fe9c1479184b8eda95b6c249bd64a62e499f09d0bfd0de68ca9b4` |
| `hodei-mh-lifecycle-server-v0.1.0-linux-x86_64` | `9f7f699be012332699c73f8a263ee40693f75dd2a47675ff153b18104bee1a09` |
| `hodei-mh-v0.1.0-linux-x86_64.tar.gz` | `d9c40b63ad41a5bced18739b4fb2729309a1e669d0f689c9cf38855542561c26` |

Requirements: Docker (the sandbox backend), a host that can pull the pinned images,
and the restricted-egress proxy for sandboxes (see
[docs/profiles.md](docs/profiles.md)).

### 1. Download

```bash
REPO=Rubentxu/hodei-meta-harness
TAG=v0.1.0
BASE=https://github.com/$REPO/releases/download/$TAG

# the installable bundle plus the manifest of digests
curl -L -O "$BASE/hodei-mh-v0.1.0-linux-x86_64.tar.gz"
curl -L -O "$BASE/SHA256SUMS"

# or the binaries on their own
curl -L -O "$BASE/hodei-mh-v0.1.0-linux-x86_64"
curl -L -O "$BASE/hodei-mh-lifecycle-server-v0.1.0-linux-x86_64"
chmod +x hodei-mh-v0.1.0-linux-x86_64 hodei-mh-lifecycle-server-v0.1.0-linux-x86_64
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
hodei-mh-v0.1.0-linux-x86_64: OK
hodei-mh-lifecycle-server-v0.1.0-linux-x86_64: OK
hodei-mh-v0.1.0-linux-x86_64.tar.gz: OK
```

If you only took the bundle, check it against the digest published in the release
notes:

```bash
echo "d9c40b63ad41a5bced18739b4fb2729309a1e669d0f689c9cf38855542561c26  hodei-mh-v0.1.0-linux-x86_64.tar.gz" | sha256sum -c -
```

### 3. Install outside the repository

The bundle is self-contained: `bin/`, `docs/profiles.md`, `install.sh`,
`INSTALL.md` and `SHA256SUMS`, nothing else. Unpack it anywhere and run the
installer with the prefix you want — the repository is not needed at runtime:

```bash
mkdir hodei-mh && tar xzf hodei-mh-v0.1.0-linux-x86_64.tar.gz -C hodei-mh
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
