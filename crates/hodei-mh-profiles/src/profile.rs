//! Profile definitions (pure): the product ships TWO supported profiles
//! (Rust, JVM) with FIXED versions — image URI + content digest. User
//! images are accepted, but an unpinned product profile is a refusal, not
//! a warning.

use serde::{Deserialize, Serialize};

/// An image reference. Product profiles MUST be pinned to a digest;
/// user-provided images may omit it (the doctor reports it as such).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImagePin {
    pub uri: String,
    pub digest: Option<String>,
}

impl ImagePin {
    pub fn pinned(uri: impl Into<String>, digest: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            digest: Some(digest.into()),
        }
    }

    pub fn user(uri: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            digest: None,
        }
    }

    /// Full reference with `@sha256:` when pinned — this is what goes into
    /// the sandbox create request.
    pub fn full_reference(&self) -> String {
        match &self.digest {
            Some(digest) => format!("{}@{digest}", self.uri),
            None => self.uri.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Toolchain {
    /// E.g. "cargo 1.96.0" or "javac 21.0.11" — the version the profile
    /// guarantees, independent of what the image happens to carry.
    pub primary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileSpec {
    pub name: String,
    pub image: ImagePin,
    pub toolchain: Toolchain,
}

/// The two profiles the product supports with pinned versions.
pub fn rust_profile(image: ImagePin) -> ProfileSpec {
    ProfileSpec {
        name: "rust".into(),
        image,
        toolchain: Toolchain {
            primary: "cargo 1.100.0 (rust:1-bookworm)".into(),
        },
    }
}

pub fn jvm_profile(image: ImagePin) -> ProfileSpec {
    ProfileSpec {
        name: "jvm".into(),
        image,
        toolchain: Toolchain {
            primary: "javac 21.0.11".into(),
        },
    }
}

/// A user-provided image: accepted as-is, with an explicit note that it is
/// NOT pinned by us.
pub fn user_profile(
    name: impl Into<String>,
    image: ImagePin,
    primary: impl Into<String>,
) -> (ProfileSpec, String) {
    let note = match image.digest {
        Some(_) => "user image pinned by digest".to_string(),
        None => "user image is NOT pinned to a digest: reproducibility depends on the registry tag"
            .to_string(),
    };
    (
        ProfileSpec {
            name: name.into(),
            image,
            toolchain: Toolchain {
                primary: primary.into(),
            },
        },
        note,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_reference_includes_digest_when_pinned() {
        let pin = ImagePin::pinned("rust:1-bookworm", "sha256:abc123");
        assert_eq!(pin.full_reference(), "rust:1-bookworm@sha256:abc123");
        let user = ImagePin::user("my-image:dev");
        assert_eq!(user.full_reference(), "my-image:dev");
    }

    #[test]
    fn user_images_without_digest_get_an_honest_note() {
        let (spec, note) = user_profile(
            "custom",
            ImagePin::user("registry/app:latest"),
            "custom tool",
        );
        assert!(note.contains("NOT pinned"));
        assert_eq!(spec.image.digest, None);
        let (spec_pinned, note_pinned) = user_profile(
            "custom2",
            ImagePin::pinned("registry/app:latest", "sha256:x"),
            "custom tool",
        );
        assert!(note_pinned.contains("pinned by digest"));
        assert!(spec_pinned.image.digest.is_some());
    }

    #[test]
    fn shipped_profiles_carry_fixed_versions() {
        let rust = rust_profile(ImagePin::pinned("rust:1-bookworm", "sha256:r"));
        let jvm = jvm_profile(ImagePin::pinned("opensandbox/code-interpreter", "sha256:j"));
        assert_eq!(rust.toolchain.primary, "cargo 1.100.0 (rust:1-bookworm)");
        assert_eq!(jvm.toolchain.primary, "javac 21.0.11");
        assert!(rust.image.digest.is_some() && jvm.image.digest.is_some());
    }
}
