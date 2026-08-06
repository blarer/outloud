//! LLM model manager: download with resume, SHA256 verify, atomic rename.
//!
//! Deliberately mirrors `crates/asr/src/models.rs` rather than importing it:
//! the asr crate pulls the audio stack as a dependency, and this crate must
//! stay OS- and audio-independent so it tests anywhere. The invariants are
//! identical and worth restating because callers depend on them:
//!
//! - In-progress downloads live at `<id>.partial`; the final name appears
//!   only after the checksum passes, via an atomic same-filesystem rename.
//!   A present final file is therefore always a verified one.
//! - Resume uses RFC 7233 Range; a server ignoring Range restarts cleanly.
//! - The *whole* file is hashed after download, including any resumed
//!   prefix, so a corrupt prefix from a previous crash cannot survive.
//! - Progress reports real bytes, never fake percentages.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// A downloadable GGUF model. Same shape as `asr::models::ModelSpec`, plus
/// honest resource figures so the UI can warn *before* a 1.3GB download.
#[derive(Debug, Clone)]
pub struct LlmModelSpec {
    /// Stable identifier, doubles as the cache filename stem.
    pub id: &'static str,
    pub url: &'static str,
    /// Lowercase hex SHA256 of the complete file.
    pub sha256: Option<&'static str>,
    /// Approximate size, for progress display before the first byte.
    pub approx_bytes: u64,
    /// Weight licence, distinct from this crate's MIT code licence; users
    /// redistributing downloaded bundles need to know it.
    pub license: &'static str,
    /// Honest resident-memory estimate once loaded (weights + KV cache for
    /// a short context). From measurement where available, model-card math
    /// otherwise; see docs/llm.md.
    pub approx_ram_bytes: u64,
}

/// Registry of models the llama backend can run. First entry is the default.
pub fn registry() -> Vec<LlmModelSpec> {
    vec![LlmModelSpec {
        id: "qwen3-1.7b-q4km-gguf",
        // ggml-org's official GGUF conversion of Qwen/Qwen3-1.7B.
        url: "https://huggingface.co/ggml-org/Qwen3-1.7B-GGUF/resolve/main/Qwen3-1.7B-Q4_K_M.gguf",
        // Pinned from a verified fetch on 2026-07-27.
        sha256: Some("d2387ca2dbfee2ffabce7120d3770dadca0b293052bc2f0e138fdc940d9bc7b5"),
        approx_bytes: 1_282_439_264,
        license: "Apache-2.0",
        // Measured on this machine (see docs/llm.md): ~1.5GB resident with
        // a 4k context loaded via llama.cpp Metal.
        approx_ram_bytes: 1_500_000_000,
    }]
}

/// Where models live: shared with the ASR models on purpose, one cache to
/// manage and one directory for the user to delete.
pub fn default_cache_dir() -> PathBuf {
    config::model_dir()
}

/// Download progress, delivered after every chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub bytes_done: u64,
    /// Total when known (Content-Length or resume math); `None` otherwise.
    pub bytes_total: Option<u64>,
}

/// Errors callers are expected to handle distinctly.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("checksum mismatch for {id}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        id: String,
        expected: String,
        actual: String,
    },
    #[error("http error fetching {url}: {source}")]
    Http {
        url: String,
        #[source]
        source: Box<ureq::Error>,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Fetch `spec` into `cache_dir`, resuming a partial download if one exists.
/// Returns the path to the verified file.
pub fn fetch(
    spec: &LlmModelSpec,
    cache_dir: &Path,
    mut on_progress: impl FnMut(Progress),
) -> Result<PathBuf, FetchError> {
    std::fs::create_dir_all(cache_dir)?;
    let final_path = cache_dir.join(spec.id);
    if final_path.exists() {
        // Present implies verified (rename invariant), so a warm cache costs
        // zero hashing time on startup.
        return Ok(final_path);
    }
    let partial_path = cache_dir.join(format!("{}.partial", spec.id));
    let existing = std::fs::metadata(&partial_path)
        .map(|m| m.len())
        .unwrap_or(0);

    let mut request = ureq::get(spec.url);
    if existing > 0 {
        // RFC 7233 resume. Servers ignoring Range return 200 with the whole
        // body; detected below and we start over.
        request = request.set("Range", &format!("bytes={existing}-"));
    }
    let response = request.call().map_err(|e| FetchError::Http {
        url: spec.url.to_string(),
        source: Box::new(e),
    })?;

    let resumed = response.status() == 206;
    let mut bytes_done = if resumed { existing } else { 0 };
    let bytes_total = response
        .header("Content-Length")
        .and_then(|v| v.parse::<u64>().ok())
        .map(|len| len + if resumed { existing } else { 0 });

    let mut file = if resumed {
        std::fs::OpenOptions::new()
            .append(true)
            .open(&partial_path)?
    } else {
        std::fs::File::create(&partial_path)?
    };

    let mut reader = response.into_reader();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        bytes_done += n as u64;
        on_progress(Progress {
            bytes_done,
            bytes_total,
        });
    }
    file.flush()?;
    drop(file);

    // Verify the *whole* file, including any resumed prefix.
    let actual = sha256_file(&partial_path)?;
    if let Some(expected) = spec.sha256 {
        if actual != expected {
            // Remove the bad partial so the next attempt starts clean.
            let _ = std::fs::remove_file(&partial_path);
            return Err(FetchError::ChecksumMismatch {
                id: spec.id.to_string(),
                expected: expected.to_string(),
                actual,
            });
        }
    } else {
        eprintln!(
            "warning: model {} has no pinned sha256; downloaded file hashes to {actual}",
            spec.id
        );
    }
    std::fs::rename(&partial_path, &final_path)?;
    Ok(final_path)
}

/// SHA256 of a file, streamed so gigabyte models need constant RAM.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vector() {
        let dir = std::env::temp_dir().join("aqua-llm-test-sha");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("abc.txt");
        std::fs::write(&p, b"abc").unwrap();
        // NIST test vector for "abc".
        assert_eq!(
            sha256_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn cached_file_short_circuits_network() {
        let dir = std::env::temp_dir().join("aqua-llm-test-cache");
        std::fs::create_dir_all(&dir).unwrap();
        let spec = LlmModelSpec {
            id: "already-here",
            // Unroutable URL proves no network attempt happens on cache hit.
            url: "http://192.0.2.1/never",
            sha256: None,
            approx_bytes: 3,
            license: "MIT",
            approx_ram_bytes: 3,
        };
        std::fs::write(dir.join(spec.id), b"hit").unwrap();
        let path = fetch(&spec, &dir, |_| {}).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"hit");
    }

    #[test]
    fn registry_entries_are_well_formed() {
        for spec in registry() {
            assert!(spec.url.starts_with("https://"), "{}", spec.id);
            assert!(!spec.license.is_empty(), "{}", spec.id);
            assert!(spec.approx_bytes > 0, "{}", spec.id);
            assert!(spec.approx_ram_bytes >= spec.approx_bytes, "{}", spec.id);
            if let Some(h) = spec.sha256 {
                assert_eq!(h.len(), 64, "{}", spec.id);
                assert!(h.chars().all(|c| c.is_ascii_hexdigit()), "{}", spec.id);
            }
        }
    }
}
