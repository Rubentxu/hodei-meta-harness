//! Reproducible language profiles (TASK-16): pinned base images with
//! digests, a non-destructive doctor, an availability gate that NEVER
//! falls back to running natively (YOLO) without the sandbox, per-project
//! caches that cannot cross-contaminate, and credential delivery through
//! the authorized minimal channel (process env only).

pub mod build_plan;
pub mod credentials;
pub mod doctor;
pub mod gate;
pub mod profile;
