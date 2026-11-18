//! SHA-256 of bytes and files: the seal of the bundle.
//!
//! The digests are a FACT read from the bytes, never a name taken on
//! trust: a release that ships its own checksums lets the consumer
//! verify what it installed without trusting the network.

use sha2::{Digest, Sha256};

/// The SHA-256 of the bytes, in lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex(&hasher.finalize())
}

/// The SHA-256 of a file, or `None` when it cannot be read: an
/// unreadable file has no digest, and saying otherwise would be a
/// claim without a fact behind it.
pub fn sha256_file(path: &std::path::Path) -> Option<String> {
    std::fs::read(path).ok().map(|bytes| sha256_hex(&bytes))
}

/// The `sha256sum` line of a file: `<digest>  <name>`.
///
/// The format is the one `sha256sum -c` reads, so the manifest the
/// bundle ships is verifiable with the standard tool.
pub fn checksum_line(digest: &str, name: &str) -> String {
    format!("{digest}  {name}")
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit((byte >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((byte & 0x0f) as u32, 16).unwrap_or('0'));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_digest_of_the_empty_input_is_the_known_one() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn the_digest_of_a_known_input_is_the_known_one() {
        // The published vector: if this drifts, every released digest
        // is worthless.
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_file_that_cannot_be_read_has_no_digest() {
        assert_eq!(sha256_file(std::path::Path::new("/nonexistent/file")), None);
    }

    #[test]
    fn the_checksum_line_is_the_one_sha256sum_reads() {
        let line = checksum_line("abc123", "bin/hodei-mh");
        assert_eq!(line, "abc123  bin/hodei-mh");
    }
}
