//! The lifecycle client of an execd: ONE HTTP client, ONE endpoint, and
//! the readiness the current sandbox generation has already shown.
//!
//! The client is what makes the transport cheap: the connections are
//! reused between the calls of a sandbox, and the readiness is observed
//! ONCE per generation instead of once per command. The readiness is
//! tied to the generation of the sandbox: a sandbox that was replaced
//! behind the same address is a different generation, and the readiness
//! observed for the previous one does not hold for it.

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use super::api::{
    self, CreateSessionRequest, CreateSessionResponse, ExecdApiError, SessionStatusResponse,
};
use super::endpoint::ExecdEndpoint;
use super::pipe::AttachOptions;

/// The generation of a sandbox: the readiness observed for one sandbox
/// does not hold for the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SandboxGeneration(u64);

impl SandboxGeneration {
    /// The generation of a sandbox that has just been created.
    pub fn first() -> Self {
        SandboxGeneration(1)
    }

    /// The generation that follows this one.
    fn next(self) -> Self {
        SandboxGeneration(self.0 + 1)
    }
}

/// What the client knows about the sandbox it talks to.
#[derive(Debug)]
struct ClientState {
    /// The generation of the sandbox behind the endpoint.
    generation: SandboxGeneration,
    /// The generation the readiness was observed for. `None` when the
    /// readiness is unknown.
    readiness: Option<SandboxGeneration>,
}

/// A client of the lifecycle of one execd.
#[derive(Debug)]
pub struct ExecdClient {
    endpoint: ExecdEndpoint,
    /// The shared HTTP client. `None` when its construction failed (the
    /// TLS backend is the usual reason): the client exists, but every
    /// call says the transport is unavailable instead of panicking.
    http: Option<reqwest::Client>,
    state: Mutex<ClientState>,
}

impl ExecdClient {
    /// A client of the execd behind the endpoint.
    ///
    /// The construction is total: an HTTP client that cannot be built
    /// leaves a client that answers every call with
    /// [`ExecdApiError::ClientUnavailable`], never a panic.
    pub fn new(endpoint: ExecdEndpoint) -> Self {
        Self {
            endpoint,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .ok(),
            state: Mutex::new(ClientState {
                generation: SandboxGeneration::first(),
                readiness: None,
            }),
        }
    }

    /// The execd this client talks to.
    pub fn endpoint(&self) -> &ExecdEndpoint {
        &self.endpoint
    }

    /// The WebSocket address of a session of this execd.
    pub fn ws_url(&self, session_id: &str) -> String {
        self.endpoint.ws_url(session_id)
    }

    /// The options that attach to a session of this execd.
    pub fn attach_options(&self) -> AttachOptions {
        self.endpoint.attach_options()
    }

    /// The HTTP client of the execd calls, or the explicit reason it is
    /// unavailable.
    fn http(&self) -> Result<&reqwest::Client, ExecdApiError> {
        self.http.as_ref().ok_or_else(|| {
            ExecdApiError::ClientUnavailable(
                "the HTTP client of the execd could not be built".to_string(),
            )
        })
    }

    /// The state of the client. A poisoned lock is RECOVERED: the
    /// generation and the readiness it carries are worth more than the
    /// panic that poisoned them.
    fn state(&self) -> MutexGuard<'_, ClientState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The generation of the sandbox behind the endpoint.
    pub fn generation(&self) -> SandboxGeneration {
        self.state().generation
    }

    /// Whether the readiness of the current generation is already known.
    pub fn readiness_is_current(&self) -> bool {
        let state = self.state();
        state.readiness == Some(state.generation)
    }

    /// The readiness of the current generation was observed.
    pub fn mark_ready(&self) {
        let mut state = self.state();
        state.readiness = Some(state.generation);
    }

    /// The readiness is not known anymore: the execd may be a different
    /// server now (a lost session says so).
    pub fn invalidate_readiness(&self) {
        let mut state = self.state();
        state.readiness = None;
    }

    /// The sandbox behind the endpoint was replaced: the readiness of
    /// the previous generation does not hold for this one.
    pub fn advance_generation(&self) {
        let mut state = self.state();
        state.generation = state.generation.next();
        state.readiness = None;
    }

    /// Create a session running `command`.
    pub async fn create_session(
        &self,
        req: CreateSessionRequest<'_>,
        timeout: Duration,
    ) -> Result<CreateSessionResponse, ExecdApiError> {
        api::create_session_with_client(self.http()?, &self.endpoint, req, timeout).await
    }

    /// The state of a session and how much output it produced.
    pub async fn session_status(
        &self,
        session_id: &str,
        timeout: Duration,
    ) -> Result<SessionStatusResponse, ExecdApiError> {
        api::session_status_with_client(self.http()?, &self.endpoint, session_id, timeout).await
    }

    /// How many output bytes the execd says the session produced.
    pub async fn session_output_offset(&self, session_id: &str, timeout: Duration) -> Option<i64> {
        let http = self.http().ok()?;
        api::session_output_offset_with_client(http, &self.endpoint, session_id, timeout).await
    }

    /// Release a session and its process group.
    pub async fn delete_session(
        &self,
        session_id: &str,
        timeout: Duration,
    ) -> Result<(), ExecdApiError> {
        api::delete_session_with_client(self.http()?, &self.endpoint, session_id, timeout).await
    }

    /// Upload a file into the sandbox.
    pub async fn upload_file(
        &self,
        bytes: Vec<u8>,
        remote_path: &str,
        timeout: Duration,
    ) -> Result<(), ExecdApiError> {
        api::upload_file_with_client(self.http()?, &self.endpoint, bytes, remote_path, timeout)
            .await
    }

    /// Download a file from the sandbox.
    pub async fn download_file(
        &self,
        path: &str,
        timeout: Duration,
    ) -> Result<Vec<u8>, ExecdApiError> {
        api::download_file_with_client(self.http()?, &self.endpoint, path, timeout).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_readiness_is_not_known_before_it_is_observed() {
        let client = ExecdClient::new(ExecdEndpoint::new("http://127.0.0.1:1"));
        assert!(!client.readiness_is_current());
    }

    #[test]
    fn the_observed_readiness_belongs_to_the_current_generation() {
        let client = ExecdClient::new(ExecdEndpoint::new("http://127.0.0.1:1"));
        client.mark_ready();
        assert!(client.readiness_is_current());
    }

    #[test]
    fn a_lost_session_invalidates_the_readiness() {
        let client = ExecdClient::new(ExecdEndpoint::new("http://127.0.0.1:1"));
        client.mark_ready();
        client.invalidate_readiness();
        assert!(!client.readiness_is_current());
    }

    #[test]
    fn the_readiness_of_a_previous_generation_is_not_reused() {
        let client = ExecdClient::new(ExecdEndpoint::new("http://127.0.0.1:1"));
        client.mark_ready();
        let generation = client.generation();
        client.advance_generation();
        assert!(!client.readiness_is_current());
        assert_ne!(client.generation(), generation);
    }

    #[test]
    fn a_poisoned_state_is_recovered_instead_of_panicking() {
        // A panic while the state was held must not disable the client:
        // the generation and the readiness are recovered, not re-panicked
        // on.
        let client =
            std::sync::Arc::new(ExecdClient::new(ExecdEndpoint::new("http://127.0.0.1:1")));
        let poisoned = client.clone();
        let _ = std::thread::spawn(move || {
            let _held = poisoned.state();
            panic!("the owner of the state died");
        })
        .join();
        client.mark_ready();
        assert!(client.readiness_is_current());
        client.invalidate_readiness();
        assert!(!client.readiness_is_current());
        client.advance_generation();
    }

    #[tokio::test]
    async fn a_client_without_its_transport_answers_with_an_explicit_error() {
        // The construction of the HTTP client is fallible; a client that
        // could not build it says so instead of panicking or pretending
        // a command ran.
        let client = ExecdClient {
            endpoint: ExecdEndpoint::new("http://127.0.0.1:1"),
            http: None,
            state: Mutex::new(ClientState {
                generation: SandboxGeneration::first(),
                readiness: None,
            }),
        };
        let error = client
            .create_session(
                CreateSessionRequest {
                    cwd: None,
                    command: Some("true"),
                },
                Duration::from_millis(100),
            )
            .await
            .expect_err("the transport is unavailable");
        assert!(
            matches!(error, ExecdApiError::ClientUnavailable(_)),
            "{error}"
        );
        assert_eq!(
            client
                .session_output_offset("session", Duration::from_millis(100))
                .await,
            None
        );
    }
}
