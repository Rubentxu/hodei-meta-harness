//! Supervisor surface for hodei-meta-harness (TASK-15): one control socket,
//! many clients. The TUI and the CLI are BOTH plain clients of the same
//! contract — they never boot a second controller. The pure engine decides;
//! the socket server and clients are thin adapters.

pub mod client;
pub mod engine;
pub mod lock;
pub mod paths;
pub mod protocol;
pub mod provenance;
pub mod render;
pub mod server;
