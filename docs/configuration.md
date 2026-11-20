# Configuration reference

This is the environment-variable reference for **hodei-meta-harness**
(**hodei-mh**). Every variable below was confirmed against the code that
actually reads it, and each row cites the repository path and the line that
consumes it (verified at commit `f59e43e`). Nothing here is invented: if a
name could not be confirmed in the source it is not listed.

"Default" is the value the code falls back to when the variable is unset.
Some variables are **required**: when they are missing the component refuses
to start rather than degrading — those rows say so and cite the refusal.

## How to read this reference — three lanes

hodei-mh separates its configuration by *who reads it*:

1. **Control plane** — the `hodei-mh-lifecycle-server` binary (the Docker
   lifecycle port). It reads its whole trust boundary from its own
   environment and refuses to serve on an insecure or incomplete config.
2. **Supervisor & host harness** — the `hodei-mh` CLI, the coordinator, the
   supervisor control surface, and the ACP/lifecycle HTTP clients. These are
   the host-side processes that drive goals and talk to the control plane.
3. **Probes lane** — the expensive validation scripts
   [`../scripts/probes.sh`](../scripts/probes.sh) and
   [`../scripts/egress-proxy.py`](../scripts/egress-proxy.py). Some are shell
   environment overrides; the proxy itself is configured with command-line
   flags, not environment variables, and that is stated where it applies.

Related documents: the command surface lives in
[cli-reference.md](./cli-reference.md); the profile/gate semantics live in
[profiles.md](./profiles.md). The narrative end-to-end guide
(`quickstart.md`) is still pending and is being written in parallel — it is
not linked here until it exists.

### Secrets are never shown in clear text

Three variables carry credentials: `HODEI_MH_CONTROL_TOKEN` (control-plane
authorization), `ANTHROPIC_AUTH_TOKEN` (ACP gateway token, the default
`--agent claude-code` runtime) and `QODER_PERSONAL_ACCESS_TOKEN` (the Qoder
runtime's personal access token). This document only ever names them and
shows how to **export** them with a placeholder —
never a real value. In the code these tokens travel **only in the
environment** of the process that needs them: they are never written to disk,
never logged, and never echoed in a response. See the redaction in
`crates/lifecycle-server/src/auth.rs:85-90` (`Debug` prints
`ControlToken(<redacted>)`). Export them like this, with your own secret, and
keep the values out of the repository:

```sh
export HODEI_MH_CONTROL_TOKEN="<a strong secret you keep out of the repo>"
export ANTHROPIC_AUTH_TOKEN="<your gateway token>"          # --agent claude-code (default)
export QODER_PERSONAL_ACCESS_TOKEN="<your Qoder access token>"  # --agent qoder
```

---

## 1. Control plane — `hodei-mh-lifecycle-server`

All rows here are read by the server binary. Unless noted otherwise the
anchor is `crates/lifecycle-server/src/main.rs`; policy constants live in
`crates/lifecycle-server/src/policy.rs` and the credential logic in
`crates/lifecycle-server/src/auth.rs`.

| Variable | Default | Effect | Anchor |
|---|---|---|---|
| `HOST` | `127.0.0.1` | HTTP bind address. A **non-loopback** value makes the server refuse to start, because remote HTTP would expose the control credential. | `main.rs:144`, check `145-150` |
| `PORT` | `8080` | HTTP port. A value that does not parse as `u16` falls back to `8080`. | `main.rs:151-154` |
| `EXECD_IMAGE` | `opensandbox/execd:v1.1.0` | The `execd` image injected into every sandbox (the sandbox data plane). | `main.rs:155-156` |
| `MAX_SANDBOX_TIMEOUT_SECONDS` | `86400` | Upper bound on a sandbox's requested TTL (seconds). Non-numeric falls back to the default. | `main.rs:157-160` |
| `SANDBOX_ENV` | unset | Extra environment injected into sandboxes, format `KEY=VALUE,KEY2=VALUE2`. Reserved prefixes are rejected downstream. | `main.rs:191-198` |
| `MAX_CONCURRENT_ENVIRONMENTS` | unset (no limit) | Global budget of concurrent environments, enforced **in the create path**: over-budget creates are refused. Parses as `usize`; non-numeric → no limit. | `main.rs:206-208` |
| `SANDBOX_TOTAL_MEMORY_BYTES` | unset (no limit) | Host-wide memory budget for the environments. An **optional** limit — but a malformed (non-numeric) value makes the server refuse to start rather than ignore it. | `main.rs:212-218`, helper `54-63` |
| `SANDBOX_TOTAL_NANOCPUS` | unset (no limit) | Host-wide CPU budget (effective nano-CPU accounting). Same malformed → refuse rule. | `main.rs:219-225`, helper `54-63` |
| `SANDBOX_MEMORY_LIMIT_BYTES` | `4294967296` (4 GiB) | Per-sandbox memory ceiling (`LimitPolicy`). A request may ask for less, never more. Malformed → refuse. | `main.rs:114`; default `policy.rs:172-179`; cap logic `policy.rs:188-227` |
| `SANDBOX_CPU_LIMIT_NANOCPUS` | `2000000000` (2 CPUs) | Per-sandbox CPU ceiling in nano-CPU. Malformed → refuse. | `main.rs:115`; `policy.rs:172-179` |
| `SANDBOX_PIDS_LIMIT` | `4096` | Per-sandbox PID ceiling. Malformed → refuse. | `main.rs:116`; `policy.rs:172-179` |
| `SANDBOX_NETWORK_POLICY` | `internal-egress-proxy` | Network policy name. The only supported value is `internal-egress-proxy`; an unknown policy is refused (no fallback to the bridge). | `main.rs:21`, read `72`; `policy.rs:16,47-62` |
| `SANDBOX_ADDRESS_POOL` | `172.16.0.0/12` | CIDR pool Docker allocates per-sandbox subnets from; must be a valid CIDR or the server refuses. | `main.rs:31,87-91`; `policy.rs:25` |
| `SANDBOX_PUBLISH_ADDRESS` | `127.0.0.1` | Address the sandbox ports are published on. This isolated relay requires loopback; a non-loopback value is refused. | `main.rs:35,95-102`; `policy.rs:29,234-254` |
| `HODEI_MH_ALLOW_PUBLIC_PUBLISH` | unset (`false`) | Acceptance switch (`1`/`true`) that lets `publish_address()` permit a non-loopback publish. Note: even when set, this control relay still refuses non-loopback publication at `main.rs:100-102`, so it does not change behavior for this server. | `main.rs:18,92-94`; `policy.rs:234-254` |
| `HODEI_MH_PUBLIC_ORIGIN` | `127.0.0.1` | Origin the endpoints announce. Must be `localhost` or a loopback IPv4 or the server refuses. | `main.rs:38,103-111` |
| `EXPIRY_SWEEP_SECONDS` | `5` | Interval of the TTL reaper sweep (how long an expired sandbox may outlive its contract). | `main.rs:244-247`, reaper `249-263` |
| `HODEI_MH_CONTROL_TOKEN` | **required** | Control-plane credential. The server reads it from its own environment; **absent or empty means it refuses to start**. Clients present it as an `Authorization: Bearer` header. See the "Secrets" note above. | `auth.rs:22,40-45`, errors `94-99`; start refusal `main.rs:70,165-175` |

**Variables that must stay unset:** `SANDBOX_NETWORK` (`main.rs:24,73-75`) and
`SANDBOX_SUBNET` (`main.rs:27,82-86`) are *unsupported* — networks and subnets
are created per sandbox by Docker, so the server **refuses to start** if either
is present. They are listed here only so you know their presence is fatal, not
configurable.

> Discrepancy note (recorded truthfully): the task brief described
> `MAX_CONCURRENT_ENVIRONMENTS` as a `const` in `crates/hodei-mh/src/main.rs`.
> It is not. The authoritative read is `std::env::var(...)` at
> `crates/lifecycle-server/src/main.rs:206` (a budget enforced in the server's
> create path). The `hodei-mh` crate only *sets* it when launching a server
> for a probe (`crates/hodei-mh/src/multiproject_probe.rs:171`).

---

## 2. Supervisor & host harness — `hodei-mh`, coordinator, supervisor, ACP clients

These are the host-side processes: the `hodei-mh` CLI, the durable goal
coordinator (`hodei-mh-coordinator`), the supervisor control surface
(`hodei-mh-supervisor`), and the HTTP/ACP clients (`opensandbox`). Paths
follow the XDG spec with a `hodei-mh` subdirectory.

| Variable | Default | Effect | Anchor |
|---|---|---|---|
| `HODEI_MH_CONTROL_TOKEN` | **required** | Client-side view of the same credential the server uses. The lifecycle/HTTP client reads it from the process environment and presents it as a Bearer token; absent → the request is unauthenticated. See the "Secrets" note above. | `crates/opensandbox/src/control.rs:14` (name), `40-43` (read), absent-reason `81` |
| `XDG_STATE_HOME` | `$HOME/.local/state`, else temp | Root for durable **coordinator** state: `$XDG_STATE_HOME/hodei-mh/<project>/coordinator.db`. Also the cleanup-residue journal `$XDG_STATE_HOME/hodei-mh/cleanup-journal.json`. | `crates/hodei-mh-coordinator/src/paths.rs:10` (name), `13-21`; journal `crates/opensandbox/src/cleanup.rs:83-91` |
| `XDG_RUNTIME_DIR` | `/run/user/<uid>/hodei-mh`, else a private per-user temp dir | Root for the **supervisor** control surface: the default `supervisor.sock` plus the project and socket locks. Created mode `0700`, files `0600` (private to the user). | `crates/hodei-mh-supervisor/src/paths.rs:22` (name), `28-41`, socket `44-46` |
| `XDG_CACHE_HOME` | `$HOME/.cache`, else temp | Root of the **host build cache**: `$XDG_CACHE_HOME/hodei-mh/build-cache` (used by `doctor`/profile-run). | `crates/hodei-mh/src/profile_run.rs:56-69` |
| `HODEI_MH_PROXY_URL` | `http://hodei-proxy:8888` (`DEFAULT_PROXY_URL`) | Egress-proxy URL used by `doctor` and `profile-run` (a `--proxy-url` flag overrides it). The probe lane exports it too (see lane 3). | `crates/hodei-mh/src/profile_run.rs:30,515,551`; live default `crates/hodei-mh/src/live/mod.rs:320-321` |
| `HODEI_MH_PROXY_CHECK_URL` | equals `proxy_url` | Host-reachable HTTP origin the doctor uses for its **egress-reachability check** (distinct from the in-network alias the sandbox uses). | `crates/hodei-mh/src/profile_run.rs:479-481` |
| `HODEI_MH_BUILD_CACHE` | unset (**cache off**) | Opt-in switch for the host build cache. Unset means disabled; the verifier never enables it. | `crates/hodei-mh/src/profile_run.rs:581,583-597` |
| `HODEI_MH_BUILD_CACHE_DIR` | unset (falls back to the `XDG_CACHE_HOME` root above) | Overrides the build-cache store root directly. | `crates/hodei-mh/src/profile_run.rs:582,583-590` |
| `PROBE_MODE` | unset (`""`) | A **label only**: it is echoed into the `"mode"` field of the `acp_probe` JSON report. It does not change behavior — the probe's real mode comes from the `--mode` flag (default `mock`). | `crates/hodei-mh/src/main.rs:298` (report), `--mode` `284` |
| `ANTHROPIC_AUTH_TOKEN` | **required for the live loop with `--agent claude-code`** (the default) | Gateway token for the real ACP agent. It is passed **only** as the agent process's environment (into the sandbox), never persisted. In the product live loop an empty value is a hard error (`MissingCredentials`). See the "Secrets" note above. | `crates/hodei-mh/src/live/agent.rs:220` (read), refusal `221-225`; the name is declared per runtime at `crates/hodei-mh/src/live/runtime.rs:116`; probe reads `main.rs:531`, `interact_probe.rs:32`, `autonomous_probe.rs:152`, `supervisor_probe.rs:650` |
| `QODER_PERSONAL_ACCESS_TOKEN` | **required for the live loop with `--agent qoder`** | Headless personal access token for the Qoder ACP runtime. Same channel as every credential here — the agent process's environment only (`CredentialChannel::ProcessEnvOnly`), never persisted, and an empty value is the same `MissingCredentials` refusal, whose message names this variable. | declared at `crates/hodei-mh/src/live/runtime.rs:139`, read through the runtime at `crates/hodei-mh/src/live/agent.rs:220`, refusal `221-225`; not read by the probe code (the probes drive the default runtime) |
| `ANTHROPIC_BASE_URL` | unset (SDK default) | Optional gateway base URL propagated to the agent the same way (never persisted). When empty, no override prefix is added. Only `claude-code` declares a base-URL variable; the Qoder runtime's endpoint is fixed by the vendor (`base_url_env: None`). | `crates/hodei-mh/src/live/agent.rs:226-229`; declared `runtime.rs:117`, Qoder `142`; `main.rs:533`, `interact_probe.rs:33`, `autonomous_probe.rs:153` |

### Supervisor behavior flags (not environment)

Some settings the brief groups with configuration are **CLI flags**, not
environment variables. They are listed here so the distinction is explicit
(the full flag surface lives in [cli-reference.md](./cli-reference.md)):

- **`--permission allow-once|deny`** — ACP permission mode for the live
  product loop. **Default `allow-once`**; any other value is refused
  (`--permission has the value '<x>': expected allow-once or deny`). The
  underscore form `allow_once` is the ACP option *kind* the agent answers
  with, not the flag value. Anchor:
  `crates/hodei-mh/src/live/mod.rs:305-314`; usage string
  `crates/hodei-mh/src/main.rs:76`; documented at
  `docs/cli-reference.md:209,244,269-271`.
- **`yolo_fallback`** — **not configurable.** It is a *reported guarantee*
  emitted as JSON by `doctor`, `profile-run` and the package manifest, and it
  is hard-coded `false`: there is no host (YOLO) execution fallback, so a
  refused gate never silently builds on the host. Do not look for an env
  variable to toggle it. Anchor:
  `crates/hodei-mh/src/profile_run.rs:613`,
  `crates/hodei-mh/src/package.rs:462`; discussed in
  `docs/profiles.md:307` and `docs/cli-reference.md:503`.

---

## 3. Probes lane — `scripts/probes.sh`, `scripts/egress-proxy.py`

### 3a. Shell environment overrides — `scripts/probes.sh`

These are read by the probe script itself (shell `${VAR:-default}` semantics,
so you can export them before running `scripts/probes.sh`). Note the port
default differs from the server's own default: the **lane** uses `8081`, the
**server binary** defaults to `8080`.

| Variable | Default | Effect | Anchor |
|---|---|---|---|
| `PORT` | `8081` | Port the lane runs its own control plane on (also settable with `--port`); the lane starts the server with `PORT="$PORT"`. | `scripts/probes.sh:34`, flag `67`, launch `206` |
| `OUT` | `/tmp/hodei-probes` | Directory for probe logs, reports and residue snapshots. | `scripts/probes.sh:37` |
| `PROXY_PORT` | `8888` | Egress-proxy port inside the sandbox networks; used to build `PROXY_URL` (`http://hodei-proxy:${PROXY_PORT}`). | `scripts/probes.sh:43-44` |
| `PROBE_IMAGES` | `rust:1-bookworm opensandbox/code-interpreter:v1.1.0 python:3.12-slim` | The images the lane makes sure are in the local Docker daemon before probing (the read-only doctor never pulls, so the lane pulls what is missing). | `scripts/probes.sh:57`, loop `106` |
| `BACKLOG_CLI_VERSION` | `1.55.3` | Version reported in the install hint. The lane requires the Backlog.md CLI to be present, not a particular build. | `scripts/probes.sh:48`, hint `87` |
| `HODEI_MH_EGRESS_PROXY_CONTAINER` | **required** | The trusted, already-running proxy container the server attaches to each sandbox network. The lane dies with a reason if it is unset or not running; the server also enforces it (see lane 1). | `scripts/probes.sh:124,126`; server `crates/lifecycle-server/src/main.rs:76-81` |
| `HODEI_MH_PROXY_CHECK_URL` | **required** | Host-reachable proxy HTTP origin; the lane checks it is a valid, reachable HTTP origin with an explicit port before probing. | `scripts/probes.sh:125,128-145` |
| `HODEI_MH_CONTROL_TOKEN` | **required** | Shared credential for the lane's server **and** its probe clients (used as the Bearer header in the residue check). The lane dies if unset. See the "Secrets" note above. | `scripts/probes.sh:204`, `280` |
| `HODEI_MH_PROXY_URL` | set **by the lane** | The lane *exports* this for the probe clients (the in-network proxy alias), overriding the binary default from lane 2. | `scripts/probes.sh:247` |

> `PROBE_MODE` is **not** set by this script. Its only consumer is the Rust
> `acp_probe` report (`crates/hodei-mh/src/main.rs:298`, see lane 2), where it
> is read purely as a label. The brief listed it; it is real but script-untouched.

### 3b. The egress proxy is configured with flags, not env — `scripts/egress-proxy.py`

`scripts/egress-proxy.py` is the operator-owned restricted HTTPS-CONNECT
proxy. It reads **no environment variables**; its knobs are command-line
flags with these defaults. It is documented here because the probe lane
depends on it (`AGENTS.md`: the profiles' gate refuses to build without a
restricted egress proxy, and does not fall back).

| Flag | Default | Effect | Anchor |
|---|---|---|---|
| `--bind` | `172.17.0.1` | Address the proxy binds to. | `scripts/egress-proxy.py:30`, parse `229` |
| `--port` | `8888` | Port the proxy listens on. | `scripts/egress-proxy.py:31`, parse `230` |
| `--allow-prefix` | `172.16.0.0/12` | Client networks allowed to open tunnels (CIDR or whole-octet IPv4 prefix, repeatable). Must be IPv4 and at least `/12`, else the proxy refuses to start (no open relay). | `scripts/egress-proxy.py:21,36,231,235`, guard `57-67` |
| `--allow-domain` | a fixed allow-set | Extra exact HTTPS destinations (repeatable, no wildcards). The built-in set includes `api.anthropic.com`, `crates.io`, `github.com`, `registry.npmjs.org`, and similar. | `scripts/egress-proxy.py:22-29,232,236`, validation `46-54,70-96` |

---

## Quick matrix: which lane reads what

| Variable | Control plane | Supervisor / host | Probes lane |
|---|:--:|:--:|:--:|
| `HOST`, `PORT` | ✅ (`PORT` default 8080) | — | `PORT` (default 8081) |
| `EXECD_IMAGE` | ✅ | — | — |
| `MAX_SANDBOX_TIMEOUT_SECONDS` | ✅ | — | — |
| `SANDBOX_ENV` | ✅ | — | — |
| `MAX_CONCURRENT_ENVIRONMENTS` | ✅ | — | set by `multiproject_probe` |
| `SANDBOX_TOTAL_*`, `SANDBOX_*_LIMIT*` | ✅ | — | — |
| `SANDBOX_NETWORK_POLICY`, `SANDBOX_ADDRESS_POOL`, `SANDBOX_PUBLISH_ADDRESS`, `HODEI_MH_PUBLIC_ORIGIN`, `HODEI_MH_ALLOW_PUBLIC_PUBLISH` | ✅ | — | — |
| `EXPIRY_SWEEP_SECONDS` | ✅ | — | — |
| `HODEI_MH_CONTROL_TOKEN` | ✅ (server) | ✅ (client) | ✅ (required) |
| `HODEI_MH_EGRESS_PROXY_CONTAINER` | ✅ (required) | — | ✅ (required) |
| `XDG_STATE_HOME` | — | ✅ | — |
| `XDG_RUNTIME_DIR` | — | ✅ | — |
| `XDG_CACHE_HOME`, `HODEI_MH_BUILD_CACHE*` | — | ✅ | — |
| `HODEI_MH_PROXY_URL`, `HODEI_MH_PROXY_CHECK_URL` | — | ✅ | ✅ (exports URL, requires CHECK URL) |
| `PROBE_MODE` | — | ✅ (label only) | — (not set here) |
| `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_BASE_URL` | — | ✅ | ✅ (probe code reads them) |
| `QODER_PERSONAL_ACCESS_TOKEN` | — | ✅ (`--agent qoder`) | — (probes drive the default runtime) |
| `--permission allow-once\|deny` | — | ✅ (CLI flag) | — |
| `yolo_fallback` | — | reported (always `false`) | — |
| `PROBE_IMAGES`, `OUT`, `PROXY_PORT`, `BACKLOG_CLI_VERSION` | — | — | ✅ |
| egress proxy `--bind/--port/--allow-prefix/--allow-domain` | — | — | ✅ (flags) |

## Related documentation

- [cli-reference.md](./cli-reference.md) — every product subcommand, its
  flags (including `--permission`), output shapes and exit codes.
- [profiles.md](./profiles.md) — profile images, the doctor checks and the
  egress/gate model that `HODEI_MH_PROXY_*` and `HODEI_MH_EGRESS_PROXY_CONTAINER`
  feed.
- `quickstart.md` — **pending**; the narrated goal-loop walkthrough is being
  written in parallel and is not linked until it exists.
