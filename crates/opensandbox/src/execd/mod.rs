pub mod api;
pub mod client;
pub mod deadline;
pub mod endpoint;
/// Test double of the execd surface, exported so dependent crates can
/// drive their transports offline without a daemon.
pub mod fake;
pub mod frames;
pub mod oneshot;
pub mod pipe;

#[cfg(test)]
mod measurement;
