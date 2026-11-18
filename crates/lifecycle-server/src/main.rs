//! hodei-mh-lifecycle-server entry point: config from environment, Docker
//! connection, HTTP serve.

use std::collections::BTreeMap;

use lifecycle_server::auth::{CONTROL_TOKEN_ENV, ControlToken};
use lifecycle_server::compose::DockerHardening;
use lifecycle_server::docker::DockerAdapter;
use lifecycle_server::http;
use lifecycle_server::policy::{
    DEFAULT_PUBLISH_ADDRESS, DEFAULT_SANDBOX_NETWORK, DEFAULT_SANDBOX_SUBNET,
    INTERNAL_EGRESS_PROXY, LimitPolicy, NetworkPolicy, publish_address,
};
use lifecycle_server::service::{LifecycleService, ServiceConfig, TrustConfig};

/// The environment variable that accepts the publication of the sandbox
/// ports on an address that is not the loopback.
const ALLOW_PUBLIC_PUBLISH_ENV: &str = "HODEI_MH_ALLOW_PUBLIC_PUBLISH";

/// The environment variable that names the network policy.
const NETWORK_POLICY_ENV: &str = "SANDBOX_NETWORK_POLICY";

/// The environment variable that names the network of the sandboxes.
const SANDBOX_NETWORK_ENV: &str = "SANDBOX_NETWORK";

/// The environment variable that names the subnet of that network.
const SANDBOX_SUBNET_ENV: &str = "SANDBOX_SUBNET";

/// The environment variable that names the address the ports are
/// published on.
const PUBLISH_ADDRESS_ENV: &str = "SANDBOX_PUBLISH_ADDRESS";

/// The environment variable that names the origin the endpoints announce.
const PUBLIC_ORIGIN_ENV: &str = "HODEI_MH_PUBLIC_ORIGIN";

/// Read a limit from the environment. A malformed value is a refusal,
/// not a default: a limit the operator wrote and the server ignored is
/// a limit the operator does not have.
fn limit_from_env(name: &str, default: i64) -> Result<i64, String> {
    match std::env::var(name) {
        Ok(raw) => raw
            .parse::<i64>()
            .map_err(|_| format!("the limit {name}={raw:?} is not a number")),
        Err(_) => Ok(default),
    }
}

/// The trust boundary this server enforces, read from the environment.
///
/// Every value that cannot be honoured stops the server BEFORE it
/// serves anything: an insecure configuration must not run "a little".
fn trust_from_env() -> Result<TrustConfig, String> {
    let control_token = ControlToken::from_env().map_err(|e| e.to_string())?;
    let policy_name =
        std::env::var(NETWORK_POLICY_ENV).unwrap_or_else(|_| INTERNAL_EGRESS_PROXY.to_string());
    let network_name =
        std::env::var(SANDBOX_NETWORK_ENV).unwrap_or_else(|_| DEFAULT_SANDBOX_NETWORK.to_string());
    let subnet =
        std::env::var(SANDBOX_SUBNET_ENV).unwrap_or_else(|_| DEFAULT_SANDBOX_SUBNET.to_string());
    let network =
        NetworkPolicy::parse(&policy_name, &network_name, &subnet).map_err(|e| e.message)?;
    let public_publish_accepted = std::env::var(ALLOW_PUBLIC_PUBLISH_ENV)
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let publish_address = publish_address(
        &std::env::var(PUBLISH_ADDRESS_ENV).unwrap_or_else(|_| DEFAULT_PUBLISH_ADDRESS.to_string()),
        public_publish_accepted,
    )
    .map_err(|e| e.message)?;
    let defaults = LimitPolicy::default();
    let limits = LimitPolicy {
        memory_bytes: limit_from_env("SANDBOX_MEMORY_LIMIT_BYTES", defaults.memory_bytes)?,
        nano_cpus: limit_from_env("SANDBOX_CPU_LIMIT_NANOCPUS", defaults.nano_cpus)?,
        pids_limit: limit_from_env("SANDBOX_PIDS_LIMIT", defaults.pids_limit)?,
    };
    Ok(TrustConfig {
        control_token,
        publish_address,
        public_origin: std::env::var(PUBLIC_ORIGIN_ENV)
            .unwrap_or_else(|_| DEFAULT_PUBLISH_ADDRESS.to_string()),
        network,
        limits,
    })
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let host = std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let execd_image =
        std::env::var("EXECD_IMAGE").unwrap_or_else(|_| "opensandbox/execd:v1.1.0".to_string());
    let max_timeout: i64 = std::env::var("MAX_SANDBOX_TIMEOUT_SECONDS")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(86_400);

    // The boundary is decided BEFORE anything is opened: a server that
    // cannot authenticate its clients, or that cannot enforce its
    // network policy, does not serve at all.
    let trust = match trust_from_env() {
        Ok(trust) => trust,
        Err(reason) => {
            eprintln!("the lifecycle server refuses to start: {reason}");
            eprintln!(
                "the control credential travels in the environment of the processes that need it: \
                 set {CONTROL_TOKEN_ENV} in the server and in every client"
            );
            std::process::exit(1);
        }
    };

    let docker = bollard::Docker::connect_with_unix(
        "/var/run/docker.sock",
        120,
        bollard::API_DEFAULT_VERSION,
    )
    .unwrap_or_else(|e| {
        eprintln!("cannot connect to Docker daemon: {e}");
        std::process::exit(1);
    });

    let adapter = DockerAdapter::connect(docker, execd_image);
    let mut server_env = BTreeMap::new();
    if let Ok(extra) = std::env::var("SANDBOX_ENV") {
        // Format: KEY=VALUE,KEY2=VALUE2
        for pair in extra.split(',') {
            if let Some((k, v)) = pair.split_once('=') {
                server_env.insert(k.to_string(), v.to_string());
            }
        }
    }
    let config = ServiceConfig {
        hardening: DockerHardening::default(),
        server_env,
        execd_run_as_init: true,
        max_timeout_seconds: max_timeout,
        // The global environment budget lives HERE, in the create path:
        // a client-side ledger only bounds the clients that know about it.
        max_concurrent_environments: std::env::var("MAX_CONCURRENT_ENVIRONMENTS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok()),
        trust,
    };
    let service = std::sync::Arc::new(LifecycleService::new(adapter, config));

    // TTL reaper: the sweep interval bounds how long an expired sandbox can
    // outlive its contract (spec TASK-13: TTL is real, not decorative).
    let sweep_secs: u64 = std::env::var("EXPIRY_SWEEP_SECONDS")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(5);
    let reaper = service.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(sweep_secs));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let removed = reaper.expire_due_sandboxes().await;
            if !removed.is_empty() {
                tracing::info!(
                    "expiry sweep released {} sandbox(es): {:?}",
                    removed.len(),
                    removed
                );
            }
        }
    });

    let app = http::router(service);
    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| {
            eprintln!("cannot bind {addr}: {e}");
            std::process::exit(1);
        });
    tracing::info!("hodei-mh-lifecycle-server listening on http://{addr}");
    axum::serve(listener, app).await.unwrap_or_else(|e| {
        eprintln!("server error: {e}");
        std::process::exit(1);
    });
}
