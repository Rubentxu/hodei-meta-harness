//! hodei-mh-lifecycle-server entry point: config from environment, Docker
//! connection, HTTP serve.

use std::collections::BTreeMap;

use lifecycle_server::compose::DockerHardening;
use lifecycle_server::docker::DockerAdapter;
use lifecycle_server::http;
use lifecycle_server::service::{LifecycleService, ServiceConfig};

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
    };
    let service = std::sync::Arc::new(LifecycleService::new(adapter, config));

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
