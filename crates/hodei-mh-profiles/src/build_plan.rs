//! Build plan (pure): the exact commands a profile runs for a project,
//! with a per-project cache directory derived from the project's absolute
//! path — two different projects can NEVER share cache state.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::profile::ProfileSpec;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BuildPlan {
    pub profile_name: String,
    pub project_path_in_sandbox: String,
    /// Cache directory INSIDE the sandbox, unique per project path.
    pub cache_dir: String,
    pub commands: Vec<String>,
}

/// Deterministic per-project cache token: sha256 over the absolute project
/// path. Same project → same cache; different projects → different cache,
/// so one project's build state can never leak into another's.
pub fn project_cache_token(absolute_project_path: &str) -> String {
    let digest = Sha256::digest(absolute_project_path.as_bytes());
    format!("{:x}", digest)[..16].to_string()
}

/// Plan the build for a project with the given profile. `in_sandbox_root`
/// is where the project is mounted/copied inside the sandbox.
pub fn plan(spec: &ProfileSpec, absolute_project_path: &str, in_sandbox_root: &str) -> BuildPlan {
    let token = project_cache_token(absolute_project_path);
    let cache_dir = format!("/workspace/.cache/hodei-mh/{}/{}", spec.name, token);
    let commands = match spec.name.as_str() {
        "rust" => vec![
            format!(
                "CARGO_HOME={cache_dir}/cargo CARGO_TARGET_DIR={cache_dir}/target cargo build --manifest-path {root}/Cargo.toml",
                cache_dir = cache_dir,
                root = in_sandbox_root
            ),
            format!(
                "CARGO_HOME={cache_dir}/cargo CARGO_TARGET_DIR={cache_dir}/target cargo run --quiet --manifest-path {root}/Cargo.toml",
                cache_dir = cache_dir,
                root = in_sandbox_root
            ),
        ],
        "jvm" => vec![
            format!(
                "javac -d {cache_dir}/classes $(find {root}/src -name '*.java')",
                cache_dir = cache_dir,
                root = in_sandbox_root
            ),
            format!("java -cp {cache_dir}/classes Main", cache_dir = cache_dir),
        ],
        other => vec![format!(
            "echo \"profile '{other}' has no canned build plan; run your own commands\""
        )],
    };
    BuildPlan {
        profile_name: spec.name.clone(),
        project_path_in_sandbox: in_sandbox_root.to_string(),
        cache_dir,
        commands,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{ImagePin, jvm_profile, rust_profile};

    #[test]
    fn caches_are_isolated_per_project_never_shared() {
        let spec = rust_profile(ImagePin::pinned("rust:1-bookworm", "sha256:r"));
        let plan_a = plan(&spec, "/home/dev/project-alpha", "/workspace/project-alpha");
        let plan_b = plan(&spec, "/home/dev/project-beta", "/workspace/project-beta");
        assert_ne!(plan_a.cache_dir, plan_b.cache_dir);
        // Deterministic for the same project.
        let plan_a_again = plan(&spec, "/home/dev/project-alpha", "/workspace/project-alpha");
        assert_eq!(plan_a.cache_dir, plan_a_again.cache_dir);
        // Both commands of a plan consistently use ITS cache dir.
        for command in &plan_a.commands {
            assert!(command.contains(&plan_a.cache_dir));
            assert!(!command.contains(&plan_b.cache_dir));
        }
    }

    #[test]
    fn rust_and_jvm_profiles_plan_their_own_toolchain() {
        let rust = rust_profile(ImagePin::pinned("rust:1-bookworm", "sha256:r"));
        let jvm = jvm_profile(ImagePin::pinned("code-interpreter", "sha256:j"));
        let plan_rust = plan(&rust, "/p/a", "/workspace/a");
        let plan_jvm = plan(&jvm, "/p/b", "/workspace/b");
        assert!(plan_rust.commands[0].contains("cargo build"));
        assert!(plan_jvm.commands[0].contains("javac"));
        assert!(plan_jvm.commands[1].contains("java -cp"));
    }
}
