//! Where the durable state of a project lives.
//!
//! The workspace is the user's tree: harness state never lands inside it.
//! Every project owns a directory under the state root, and the coordinator
//! state of that project is the only thing kept there.

use std::path::PathBuf;

/// The environment variable that overrides the state root.
const STATE_HOME_ENV: &str = "XDG_STATE_HOME";

/// The root that holds the state of every project.
pub fn state_root() -> PathBuf {
    if let Some(dir) = std::env::var_os(STATE_HOME_ENV).filter(|value| !value.is_empty()) {
        return PathBuf::from(dir).join("hodei-mh");
    }
    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(home).join(".local/state/hodei-mh");
    }
    std::env::temp_dir().join("hodei-mh-state")
}

/// The directory that holds the state of one project.
pub fn project_state_dir(project: &str) -> PathBuf {
    state_root().join(project_segment(project))
}

/// The durable coordinator state of one project.
pub fn project_state_db(project: &str) -> PathBuf {
    project_state_dir(project).join("coordinator.db")
}

/// A project name is ONE path segment: it can never escape the directory of
/// its project.
fn project_segment(project: &str) -> String {
    let segment: String = project
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if segment.is_empty() || segment.chars().all(|character| character == '.') {
        "unnamed".to_string()
    } else {
        segment
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_project_owns_its_own_state() {
        let alpha = project_state_dir("alpha");
        let beta = project_state_dir("beta");
        assert_ne!(alpha, beta);
        assert!(alpha.starts_with(state_root()));
        assert!(beta.starts_with(state_root()));
        assert_eq!(project_state_db("alpha").parent(), Some(alpha.as_path()));
    }

    #[test]
    fn a_project_name_stays_inside_its_own_directory() {
        let escaped = project_state_dir("../../etc");
        assert_eq!(
            escaped.parent(),
            Some(state_root().as_path()),
            "the state of a project is one level below the root"
        );
        assert!(
            !escaped
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .contains('/'),
            "the project name is one segment"
        );
        // A name that IS a directory reference does not become one.
        assert_eq!(
            project_state_dir("..").parent(),
            Some(state_root().as_path())
        );
    }
}
