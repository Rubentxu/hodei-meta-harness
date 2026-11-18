//! The admission, the creation keys and the release of environments,
//! observed against a SIMULATED daemon.
//!
//! The daemon listens on a unix socket and answers only what the paths under
//! test need. It can ACCEPT a request and never answer it, which is what lets
//! a test force two creations to overlap: the first one stays in flight with
//! its slot reserved, and the second one is decided while it is.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use super::*;
use crate::model::CreateSandboxRequest;

/// What the simulated daemon does with the first call of a creation: the
/// look-up of the image.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ImageLookup {
    /// The request is accepted and never answered: the creation stays in
    /// flight for as long as the test wants.
    Hang,
    /// The daemon refuses it.
    Fail,
}

struct SimContainer {
    id: String,
    sandbox_id: String,
    labels: HashMap<String, String>,
    memory: i64,
    nano_cpus: i64,
}

struct Daemon {
    image_lookup: Mutex<ImageLookup>,
    containers: Mutex<Vec<SimContainer>>,
    delete_fails: AtomicBool,
    deletes: AtomicUsize,
    /// How many image look-ups reached the daemon: the creations that got
    /// past the admission.
    lookups: tokio::sync::watch::Sender<usize>,
}

impl Daemon {
    fn lookups(&self) -> usize {
        *self.lookups.borrow()
    }

    /// Wait until `count` creations reached the daemon.
    async fn wait_for_lookups(&self, count: usize) {
        let mut seen = self.lookups.subscribe();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            seen.wait_for(|lookups| *lookups >= count),
        )
        .await
        .expect("the creations reach the daemon")
        .expect("the daemon is alive");
    }

    fn add_container(&self, sandbox_id: &str, extra: &[(&str, &str)], memory: i64, cpus: i64) {
        let mut labels = HashMap::from([(compose::LABEL_ID.to_string(), sandbox_id.to_string())]);
        for (key, value) in extra {
            labels.insert((*key).to_string(), (*value).to_string());
        }
        self.containers.lock().unwrap().push(SimContainer {
            id: format!("{:0>64}", sandbox_id.replace('-', "")),
            sandbox_id: sandbox_id.to_string(),
            labels,
            memory,
            nano_cpus: cpus,
        });
    }

    fn clear_containers(&self) {
        self.containers.lock().unwrap().clear();
    }
}

fn json_response(status: StatusCode, body: Value) -> Response {
    (status, axum::Json(body)).into_response()
}

async fn answer(State(daemon): State<Arc<Daemon>>, request: Request<Body>) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    // The client may prefix the version of the API (`/v1.52/...`).
    let path = match path.strip_prefix('/').and_then(|rest| rest.split_once('/')) {
        Some((version, rest))
            if version.starts_with('v')
                && version[1..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit()) =>
        {
            format!("/{rest}")
        }
        _ => path,
    };
    match (method, path.as_str()) {
        (Method::GET, "/images/json") => {
            daemon.lookups.send_modify(|lookups| *lookups += 1);
            let mode = *daemon.image_lookup.lock().unwrap();
            match mode {
                ImageLookup::Hang => std::future::pending::<Response>().await,
                ImageLookup::Fail => json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({"message": "the daemon refuses the look-up"}),
                ),
            }
        }
        (Method::GET, "/containers/json") => {
            let list: Vec<Value> = daemon
                .containers
                .lock()
                .unwrap()
                .iter()
                .map(|c| json!({"Id": c.id, "Labels": c.labels, "State": "running"}))
                .collect();
            json_response(StatusCode::OK, Value::Array(list))
        }
        (Method::GET, path) if path.starts_with("/containers/") && path.ends_with("/json") => {
            let name = &path["/containers/".len()..path.len() - "/json".len()];
            let found = daemon
                .containers
                .lock()
                .unwrap()
                .iter()
                .find(|c| c.id == name || compose::container_name(&c.sandbox_id) == name)
                .map(|c| {
                    json!({
                        "Id": c.id,
                        "Image": "sha256:image",
                        "Config": {"Labels": c.labels, "Cmd": ["/bin/sh"]},
                        "State": {"Status": "running"},
                        "HostConfig": {"Memory": c.memory, "NanoCpus": c.nano_cpus},
                    })
                });
            match found {
                Some(body) => json_response(StatusCode::OK, body),
                None => json_response(
                    StatusCode::NOT_FOUND,
                    json!({"message": "no such container"}),
                ),
            }
        }
        (Method::DELETE, path) if path.starts_with("/containers/") => {
            daemon.deletes.fetch_add(1, Ordering::SeqCst);
            if daemon.delete_fails.load(Ordering::SeqCst) {
                return json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({"message": "the daemon refuses the removal"}),
                );
            }
            let name = &path["/containers/".len()..];
            daemon.containers.lock().unwrap().retain(|c| c.id != name);
            StatusCode::NO_CONTENT.into_response()
        }
        (Method::GET, path) if path.starts_with("/networks/") => {
            json_response(StatusCode::NOT_FOUND, json!({"message": "no such network"}))
        }
        _ => json_response(StatusCode::NOT_FOUND, json!({"message": "not simulated"})),
    }
}

/// A simulated daemon and a service that talks to it.
struct Harness {
    service: Arc<LifecycleService>,
    daemon: Arc<Daemon>,
    socket: std::path::PathBuf,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
    }
}

fn harness(configure: impl FnOnce(&mut ServiceConfig)) -> Harness {
    let (lookups, _seen) = tokio::sync::watch::channel(0usize);
    let daemon = Arc::new(Daemon {
        image_lookup: Mutex::new(ImageLookup::Hang),
        containers: Mutex::new(Vec::new()),
        delete_fails: AtomicBool::new(false),
        deletes: AtomicUsize::new(0),
        lookups,
    });
    let socket = std::env::temp_dir().join(format!("hodei-sim-{}.sock", uuid::Uuid::new_v4()));
    let listener = tokio::net::UnixListener::bind(&socket).expect("the simulated socket");
    let app = Router::new().fallback(answer).with_state(daemon.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let docker = bollard::Docker::connect_with_socket(
        socket.to_str().expect("socket path"),
        30,
        bollard::API_DEFAULT_VERSION,
    )
    .expect("the client is built");
    let mut config = ServiceConfig::default();
    config.trust.proxy_container = Some("trusted-proxy".to_string());
    config.release_attempts = 3;
    config.release_delay = std::time::Duration::from_millis(1);
    configure(&mut config);
    Harness {
        service: Arc::new(LifecycleService::new(
            DockerAdapter::connect(docker, "execd:local"),
            config,
        )),
        daemon,
        socket,
    }
}

fn request(entrypoint: &str, key: Option<&str>) -> CreateSandboxRequest {
    let mut body = json!({
        "image": {"uri": "rust:1-bookworm"},
        "entrypoint": [entrypoint],
    });
    if let Some(key) = key {
        body["creationKey"] = json!(key);
    }
    serde_json::from_value(body).expect("a valid request")
}

/// Start a creation in the background.
fn spawn_create(
    harness: &Harness,
    req: CreateSandboxRequest,
) -> tokio::task::JoinHandle<Result<CreateSandboxResponse, ServiceError>> {
    let service = harness.service.clone();
    tokio::spawn(async move { service.create_sandbox(req).await })
}

/// Abandon a creation: its future is dropped, as it is when a client gives up.
async fn abandon(task: tokio::task::JoinHandle<Result<CreateSandboxResponse, ServiceError>>) {
    task.abort();
    let joined = task.await;
    assert!(joined.is_err_and(|error| error.is_cancelled()));
}

fn occupancy(harness: &Harness) -> (usize, usize) {
    let authority = lock_admission(&harness.service.admission);
    (authority.environments(), authority.creations_in_flight())
}

fn code(result: Result<CreateSandboxResponse, ServiceError>) -> &'static str {
    result.expect_err("the creation is refused").code()
}

// --- The admission is decided before any effect -------------------------

#[tokio::test]
async fn with_capacity_one_two_simultaneous_creations_admit_only_one() {
    let h = harness(|config| config.max_concurrent_environments = Some(1));

    // The first creation is accepted by the daemon and never answered: it is
    // IN FLIGHT, its environment does not exist, and its slot is taken.
    let first = spawn_create(&h, request("/bin/first", None));
    h.daemon.wait_for_lookups(1).await;
    assert_eq!(occupancy(&h), (1, 1));

    // The second one finds the host full. It is refused before it touches the
    // daemon: no environment, no network, no image look-up.
    let second = h.service.create_sandbox(request("/bin/second", None)).await;
    assert_eq!(code(second), error_codes::BUDGET_EXHAUSTED);
    assert_eq!(h.daemon.lookups(), 1, "the refused creation had no effect");
    assert_eq!(occupancy(&h), (1, 1), "the refusal took nothing");

    // The first one is abandoned: its reservation is released with it.
    abandon(first).await;
    assert_eq!(occupancy(&h), (0, 0));

    // The slot is free again: the next creation is ADMITTED, which is seen
    // because it reaches the daemon.
    let third = spawn_create(&h, request("/bin/third", None));
    h.daemon.wait_for_lookups(2).await;
    assert_eq!(occupancy(&h), (1, 1));
    abandon(third).await;
}

#[tokio::test]
async fn a_burst_of_creations_admits_exactly_the_capacity() {
    const CAPACITY: usize = 3;
    const BURST: usize = 12;
    let h = harness(|config| config.max_concurrent_environments = Some(CAPACITY));

    let tasks: Vec<_> = (0..BURST)
        .map(|n| spawn_create(&h, request(&format!("/bin/burst-{n}"), None)))
        .collect();
    // The admitted creations hang in the daemon; the others answer at once.
    h.daemon.wait_for_lookups(CAPACITY).await;
    let mut refused = 0;
    let mut admitted = Vec::new();
    for task in tasks {
        if task.is_finished() {
            let result = task.await.expect("the task ran");
            assert_eq!(code(result), error_codes::BUDGET_EXHAUSTED);
            refused += 1;
        } else {
            admitted.push(task);
        }
    }
    // A refusal is immediate, so every refused creation has answered by the
    // time the admitted ones are all in the daemon.
    tokio::task::yield_now().await;
    assert_eq!(h.daemon.lookups(), CAPACITY, "nothing above the capacity");
    assert_eq!(occupancy(&h), (CAPACITY, CAPACITY));
    assert!(admitted.len() >= CAPACITY, "the capacity was admitted");
    assert_eq!(refused + admitted.len(), BURST);
    for task in admitted {
        abandon(task).await;
    }
}

#[tokio::test]
async fn a_failed_creation_gives_its_slot_back() {
    let h = harness(|config| config.max_concurrent_environments = Some(1));
    *h.daemon.image_lookup.lock().unwrap() = ImageLookup::Fail;

    for attempt in 1..=3 {
        let result = h.service.create_sandbox(request("/bin/fails", None)).await;
        assert_ne!(
            code(result),
            error_codes::BUDGET_EXHAUSTED,
            "attempt {attempt}: the slot of the failed creation is free"
        );
        assert_eq!(occupancy(&h), (0, 0), "attempt {attempt}");
        assert_eq!(h.daemon.lookups(), attempt, "each attempt was admitted");
    }
}

#[tokio::test]
async fn a_memory_budget_refuses_what_does_not_fit_with_the_effective_limit() {
    // The effective memory of the environment is what is counted: the policy
    // default, when the request asks for nothing.
    let h = harness(|config| config.max_total_memory_bytes = Some(1));
    let result = h.service.create_sandbox(request("/bin/big", None)).await;
    assert_eq!(code(result), error_codes::BUDGET_EXHAUSTED);
    assert_eq!(h.daemon.lookups(), 0);
}

// --- A restart reconciles with what the daemon reports ------------------

#[tokio::test]
async fn a_restarted_server_counts_the_environments_it_left_behind() {
    let h = harness(|config| config.max_concurrent_environments = Some(1));
    *h.daemon.image_lookup.lock().unwrap() = ImageLookup::Fail;
    let survivor = uuid::Uuid::new_v4().to_string();
    h.daemon
        .add_container(&survivor, &[], 512 << 20, 1_000_000_000);

    // A server that has not reconciled yet knows nothing about it.
    h.service.reconcile().await.expect("the reconciliation");
    assert_eq!(occupancy(&h), (1, 0));
    let refused = h.service.create_sandbox(request("/bin/new", None)).await;
    assert_eq!(code(refused), error_codes::BUDGET_EXHAUSTED);
    assert_eq!(h.daemon.lookups(), 0, "the host is full: no effect");

    // The environment is removed by somebody else: it stops counting.
    h.daemon.clear_containers();
    h.service.reconcile().await.expect("the reconciliation");
    assert_eq!(occupancy(&h), (0, 0));
    let admitted = h.service.create_sandbox(request("/bin/new", None)).await;
    assert_ne!(code(admitted), error_codes::BUDGET_EXHAUSTED);
    assert_eq!(h.daemon.lookups(), 1, "the slot was free again");
}

#[tokio::test]
async fn a_reconciliation_does_not_take_the_slot_of_a_creation_in_flight() {
    let h = harness(|config| config.max_concurrent_environments = Some(1));
    let first = spawn_create(&h, request("/bin/first", None));
    h.daemon.wait_for_lookups(1).await;

    // The daemon does not know the creation yet, and the reconciliation sees
    // an empty host: the slot of the creation must survive it.
    h.service.reconcile().await.expect("the reconciliation");
    assert_eq!(occupancy(&h), (1, 1));
    let second = h.service.create_sandbox(request("/bin/second", None)).await;
    assert_eq!(code(second), error_codes::BUDGET_EXHAUSTED);
    abandon(first).await;
}

#[tokio::test]
async fn the_limits_a_container_really_runs_under_are_the_ones_that_count() {
    let h = harness(|config| config.max_total_memory_bytes = Some(1 << 30));
    let survivor = uuid::Uuid::new_v4().to_string();
    // The environment runs under 1 GiB: the whole budget.
    h.daemon
        .add_container(&survivor, &[], 1 << 30, 1_000_000_000);
    h.service.reconcile().await.expect("the reconciliation");
    let refused = h.service.create_sandbox(request("/bin/new", None)).await;
    assert_eq!(code(refused), error_codes::BUDGET_EXHAUSTED);
    assert_eq!(h.daemon.lookups(), 0);
}

// --- The creation key makes a retry one environment ---------------------

#[tokio::test]
async fn the_same_key_with_the_same_body_is_the_environment_that_already_exists() {
    let h = harness(|_| {});
    let sandbox_id = uuid::Uuid::new_v4().to_string();
    let req = request("/bin/work", Some("creation-1"));
    let fingerprint = compose::creation_fingerprint(&req);
    h.daemon.add_container(
        &sandbox_id,
        &[
            (compose::LABEL_CREATION_KEY, "creation-1"),
            (compose::LABEL_CREATION_FINGERPRINT, fingerprint.as_str()),
        ],
        512 << 20,
        1_000_000_000,
    );
    // The key survives a restart: it is read back from the labels.
    h.service.reconcile().await.expect("the reconciliation");

    let first = h.service.create_sandbox(req).await.expect("answered");
    let second = h
        .service
        .create_sandbox(request("/bin/work", Some("creation-1")))
        .await
        .expect("answered");
    assert_eq!(first.id, sandbox_id);
    assert_eq!(second.id, sandbox_id, "a retry is the same environment");
    assert_eq!(h.daemon.lookups(), 0, "no second environment was created");
}

#[tokio::test]
async fn the_same_key_with_another_body_is_a_conflict_and_touches_nothing() {
    let h = harness(|_| {});
    let sandbox_id = uuid::Uuid::new_v4().to_string();
    let original = request("/bin/work", Some("creation-1"));
    h.daemon.add_container(
        &sandbox_id,
        &[
            (compose::LABEL_CREATION_KEY, "creation-1"),
            (
                compose::LABEL_CREATION_FINGERPRINT,
                compose::creation_fingerprint(&original).as_str(),
            ),
        ],
        512 << 20,
        1_000_000_000,
    );
    h.service.reconcile().await.expect("the reconciliation");

    let other = h
        .service
        .create_sandbox(request("/bin/something-else", Some("creation-1")))
        .await;
    assert_eq!(code(other), error_codes::CREATION_CONFLICT);
    assert_eq!(h.daemon.lookups(), 0);
    assert_eq!(h.daemon.deletes.load(Ordering::SeqCst), 0);
    assert_eq!(h.daemon.containers.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn two_requests_with_the_same_key_are_one_creation() {
    let h = harness(|_| {});
    let first = spawn_create(&h, request("/bin/work", Some("creation-2")));
    h.daemon.wait_for_lookups(1).await;

    // The second request waits for the first instead of creating another
    // environment; a body that differs is refused at once.
    let conflict = h
        .service
        .create_sandbox(request("/bin/different", Some("creation-2")))
        .await;
    assert_eq!(code(conflict), error_codes::CREATION_CONFLICT);
    let second = spawn_create(&h, request("/bin/work", Some("creation-2")));
    tokio::task::yield_now().await;
    assert_eq!(h.daemon.lookups(), 1, "the second request created nothing");

    // The owner is abandoned: the key is free and the waiting request takes
    // it over. It is the ONLY one that goes to the daemon.
    abandon(first).await;
    h.daemon.wait_for_lookups(2).await;
    assert_eq!(h.daemon.lookups(), 2);
    abandon(second).await;
}

#[tokio::test]
async fn an_abandoned_creation_frees_its_key() {
    let h = harness(|_| {});
    let first = spawn_create(&h, request("/bin/work", Some("creation-3")));
    h.daemon.wait_for_lookups(1).await;
    abandon(first).await;
    assert!(
        lock_creations(&h.service.creations).is_empty(),
        "the key of an abandoned creation is free"
    );
}

#[tokio::test]
async fn a_failed_creation_frees_its_key_for_a_retry() {
    let h = harness(|_| {});
    *h.daemon.image_lookup.lock().unwrap() = ImageLookup::Fail;
    for attempt in 1..=2 {
        let result = h
            .service
            .create_sandbox(request("/bin/work", Some("creation-4")))
            .await;
        assert_ne!(code(result), error_codes::CREATION_IN_PROGRESS);
        assert_eq!(h.daemon.lookups(), attempt, "the retry goes to the daemon");
    }
}

#[tokio::test]
async fn an_empty_creation_key_is_refused() {
    let h = harness(|_| {});
    let result = h
        .service
        .create_sandbox(request("/bin/work", Some("   ")))
        .await;
    assert_eq!(code(result), error_codes::INVALID_PARAMETER);
    assert_eq!(h.daemon.lookups(), 0);
}

// --- The release is honest ----------------------------------------------

#[tokio::test]
async fn a_failed_removal_is_retried_a_bounded_number_of_times_and_is_not_a_release() {
    let h = harness(|config| config.max_concurrent_environments = Some(1));
    let sandbox_id = uuid::Uuid::new_v4().to_string();
    h.daemon
        .add_container(&sandbox_id, &[], 512 << 20, 1_000_000_000);
    h.service.reconcile().await.expect("the reconciliation");
    h.daemon.delete_fails.store(true, Ordering::SeqCst);

    let failure = h
        .service
        .delete_sandbox(&sandbox_id)
        .await
        .expect_err("the daemon refuses the removal");
    assert_ne!(failure.code(), error_codes::SANDBOX_NOT_FOUND);
    assert_eq!(
        h.daemon.deletes.load(Ordering::SeqCst),
        3,
        "the removal is attempted, and the attempts are bounded"
    );
    // The environment is still there, and it still counts: a failure is not
    // reported as a release.
    assert_eq!(h.daemon.containers.lock().unwrap().len(), 1);
    assert_eq!(occupancy(&h), (1, 0));
    let refused = h.service.create_sandbox(request("/bin/new", None)).await;
    assert_eq!(code(refused), error_codes::BUDGET_EXHAUSTED);

    // The daemon recovers: the retry releases the environment and the slot.
    h.daemon.delete_fails.store(false, Ordering::SeqCst);
    *h.daemon.image_lookup.lock().unwrap() = ImageLookup::Fail;
    h.service
        .delete_sandbox(&sandbox_id)
        .await
        .expect("the release succeeds");
    assert_eq!(occupancy(&h), (0, 0));
    assert!(h.daemon.containers.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_environment_that_is_not_ours_is_never_removed() {
    let h = harness(|_| {});
    // A container that answers to the name of a sandbox but whose identity
    // label belongs to another one.
    let wanted = uuid::Uuid::new_v4().to_string();
    let foreign = uuid::Uuid::new_v4().to_string();
    h.daemon
        .add_container(&foreign, &[], 512 << 20, 1_000_000_000);
    h.daemon
        .containers
        .lock()
        .unwrap()
        .last_mut()
        .expect("the container")
        .sandbox_id = wanted.clone();
    h.daemon
        .containers
        .lock()
        .unwrap()
        .last_mut()
        .expect("the container")
        .labels
        .insert(compose::LABEL_ID.to_string(), foreign);

    let refused = h
        .service
        .delete_sandbox(&wanted)
        .await
        .expect_err("the identity does not match");
    assert_eq!(refused.code(), error_codes::SANDBOX_NOT_FOUND);
    assert_eq!(h.daemon.deletes.load(Ordering::SeqCst), 0);
    assert_eq!(h.daemon.containers.lock().unwrap().len(), 1);
}

// --- The claim registry, without any daemon -----------------------------

#[tokio::test]
async fn the_claim_of_a_key_has_one_owner_at_a_time() {
    let h = harness(|_| {});
    let service = &h.service;
    let mut owner = match service.claim_creation_key("k", "fingerprint-a") {
        CreationClaim::Owned(ticket) => ticket,
        _ => panic!("the first claim owns the creation"),
    };
    assert!(matches!(
        service.claim_creation_key("k", "fingerprint-a"),
        CreationClaim::InFlight(_)
    ));
    assert!(matches!(
        service.claim_creation_key("k", "fingerprint-b"),
        CreationClaim::Conflict
    ));
    owner.finish(CreationOutcome::Succeeded("sbx-1".to_string()));
    match service.claim_creation_key("k", "fingerprint-a") {
        CreationClaim::Existing(id) => assert_eq!(id, "sbx-1"),
        _ => panic!("the creation that succeeded is the answer"),
    }
    assert!(matches!(
        service.claim_creation_key("k", "fingerprint-b"),
        CreationClaim::Conflict
    ));
}

#[tokio::test]
async fn a_ticket_that_goes_away_unfinished_releases_the_key() {
    let h = harness(|_| {});
    let service = &h.service;
    let CreationClaim::Owned(ticket) = service.claim_creation_key("k", "fingerprint-a") else {
        panic!("the first claim owns the creation");
    };
    let CreationClaim::InFlight(mut waiting) = service.claim_creation_key("k", "fingerprint-a")
    else {
        panic!("the second claim waits");
    };
    drop(ticket);
    // The one that waited is told that the creation did not happen.
    assert_eq!(
        await_creation_outcome(&mut waiting).await,
        Some(CreationOutcome::Failed)
    );
    // A claim with ANOTHER body is not a conflict any more: the key is free.
    assert!(matches!(
        service.claim_creation_key("k", "fingerprint-b"),
        CreationClaim::Owned(_)
    ));
}
