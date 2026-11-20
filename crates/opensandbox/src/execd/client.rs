//! The lifecycle client of an execd: ONE HTTP client, ONE endpoint, and
//! the readiness the current sandbox generation has already shown.
//!
//! The client is what makes the transport cheap: the connections are
//! reused between the calls of a sandbox, and the readiness is observed
//! ONCE per generation instead of once per command. The readiness is
//! tied to the generation of the sandbox: a sandbox that was replaced
//! behind the same address is a different generation, and the readiness
//! observed for the previous one does not hold for it.

use std::sync::Mutex;
use std::time::Duration;

use super::api::{
    self, CreateSessionRequest, CreateSessionResponse, ExecdApiError, SessionStatusResponse,
};
use super::endpoint::ExecdEndpoint;
use super::pipe::AttachOptions;
use crate::cleanup::CleanupRegistry;

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
    http: reqwest::Client,
    state: Mutex<ClientState>,
    registry: CleanupRegistry,
}

impl ExecdClient {
    /// A client of the execd behind the endpoint, with its own ledger of
    /// residues.
    pub fn new(endpoint: ExecdEndpoint) -> Self {
        Self::with_registry(endpoint, CleanupRegistry::new())
    }

    /// A client that registers the residues it cannot release in the
    /// given ledger: one ledger for everything the operation owns.
    pub fn with_registry(endpoint: ExecdEndpoint, registry: CleanupRegistry) -> Self {
        Self {
            endpoint,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("static HTTP client configuration is valid"),
            state: Mutex::new(ClientState {
                generation: SandboxGeneration::first(),
                readiness: None,
            }),
            registry,
        }
    }

    /// The execd this client talks to.
    pub fn endpoint(&self) -> &ExecdEndpoint {
        &self.endpoint
    }

    /// The ledger of the residues this client could not release.
    pub fn cleanup_registry(&self) -> &CleanupRegistry {
        &self.registry
    }

    /// The WebSocket address of a session of this execd.
    pub fn ws_url(&self, session_id: &str) -> String {
        self.endpoint.ws_url(session_id)
    }

    /// The options that attach to a session of this execd.
    pub fn attach_options(&self) -> AttachOptions {
        self.endpoint.attach_options()
    }

    /// The generation of the sandbox behind the endpoint.
    pub fn generation(&self) -> SandboxGeneration {
        self.state
            .lock()
            .expect("the client state is readable")
            .generation
    }

    /// Whether the readiness of the current generation is already known.
    pub fn readiness_is_current(&self) -> bool {
        let state = self.state.lock().expect("the client state is readable");
        state.readiness == Some(state.generation)
    }

    /// The readiness of the current generation was observed.
    pub fn mark_ready(&self) {
        let mut state = self.state.lock().expect("the client state is writable");
        state.readiness = Some(state.generation);
    }

    /// The readiness is not known anymore: the execd may be a different
    /// server now (a lost session says so).
    pub fn invalidate_readiness(&self) {
        let mut state = self.state.lock().expect("the client state is writable");
        state.readiness = None;
    }

    /// The sandbox behind the endpoint was replaced: the readiness of
    /// the previous generation does not hold for this one.
    pub fn advance_generation(&self) {
        let mut state = self.state.lock().expect("the client state is writable");
        state.generation = state.generation.next();
        state.readiness = None;
    }

    /// Create a session running `command`.
    pub async fn create_session(
        &self,
        req: CreateSessionRequest<'_>,
        timeout: Duration,
    ) -> Result<CreateSessionResponse, ExecdApiError> {
        api::create_session_with_client(&self.http, &self.endpoint, req, timeout).await
    }

    /// The state of a session and how much output it produced.
    pub async fn session_status(
        &self,
        session_id: &str,
        timeout: Duration,
    ) -> Result<SessionStatusResponse, ExecdApiError> {
        api::session_status_with_client(&self.http, &self.endpoint, session_id, timeout).await
    }

    /// How many output bytes the execd says the session produced.
    pub async fn session_output_offset(&self, session_id: &str, timeout: Duration) -> Option<i64> {
        api::session_output_offset_with_client(&self.http, &self.endpoint, session_id, timeout)
            .await
    }

    /// Release a session and its process group.
    pub async fn delete_session(
        &self,
        session_id: &str,
        timeout: Duration,
    ) -> Result<(), ExecdApiError> {
        api::delete_session_with_client(&self.http, &self.endpoint, session_id, timeout).await
    }

    /// Upload a file into the sandbox.
    pub async fn upload_file(
        &self,
        bytes: Vec<u8>,
        remote_path: &str,
        timeout: Duration,
    ) -> Result<(), ExecdApiError> {
        api::upload_file_with_client(&self.http, &self.endpoint, bytes, remote_path, timeout).await
    }

    /// Download a file from the sandbox.
    pub async fn download_file(
        &self,
        path: &str,
        timeout: Duration,
    ) -> Result<Vec<u8>, ExecdApiError> {
        api::download_file_with_client(&self.http, &self.endpoint, path, timeout).await
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
}
