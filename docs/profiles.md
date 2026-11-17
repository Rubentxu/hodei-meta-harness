# hodei-meta-harness: language profiles (Rust / JVM)

Reproducible build profiles for hodei-meta-harness. Everything runs inside an
OpenSandbox sandbox; there is NO native (host) execution fallback. If the
sandbox, the pinned image or the restricted egress path is unavailable, the
tool informs you and refuses.

## Prerequisites (clean host)

1. Docker (the chosen backend): `docker --version` (any recent version with
   `docker inspect`).
2. A Rust toolchain to build the harness itself: cargo ≥ 1.96.
3. Restricted egress proxy for sandboxes (the only authorized network path;
   sandboxes have NO direct internet):
   `python3 /tmp/hodei-connect-proxy.py` — binds `172.17.0.1:8888`, accepts
   CONNECT only from the docker subnet.
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

One supervisor per machine/project. The CLI and the TUI are clients of the
same socket (`sup-status`, `sup-action start|pause|resume|stop|renew`, `tui`).

## Doctor (strictly non-destructive)

```bash
./target/release/hodei-mh doctor --profile rust
./target/release/hodei-mh doctor --profile jvm
```

Checks (all read-only): profile pinned, local image digest vs pinned digest,
restricted-egress reachability, lifecycle API readiness, subscription
limitations. NEVER creates, starts or removes anything.

## Shipped profiles (fixed versions)

| profile | image | digest (sha256) | toolchain |
| --- | --- | --- | --- |
| rust | `rust:1-bookworm` | `sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea` | cargo 1.100.0 |
| jvm | `opensandbox/code-interpreter:v1.1.0` | `sha256:133a3c1720dd52291a019740c2987e7164ea6de79e23d8198798e58950ae2e6e` | javac/java 21.0.11, Maven 3.9.2 |

User images are accepted with `--image URI`; an unpinned user image is
reported as such (reproducibility then depends on the registry tag).

## Run a profile build

```bash
./target/release/hodei-mh profile-run --profile rust --project /path/to/project --cpu 1 --memory 1Gi
./target/release/hodei-mh profile-run --profile jvm  --project /path/to/project --cpu 1 --memory 1Gi
```

Behavior:

- Gates through the doctor; if anything required is missing the run REFUSES
  (exit 1) and prints the missing pieces. `yolo_fallback: false` — there is
  no native execution path.
- Creates a sandbox with the PINNED image and the REQUESTED limits, then
  reports the OBSERVED limits from inside (cgroups).
- Caches live in `/workspace/.cache/hodei-mh/<profile>/<hash-of-project-path>`
  inside the sandbox: the token is a sha256 of the project's absolute path,
  so two projects can never share cache state.
- Build commands carry no credentials; the egress proxy is the only network
  grant.
- The final record includes the image digest, the exact commands and their
  outcomes (this is the artifact you archive per build).
