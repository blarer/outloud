//! Model manager: download with resume, SHA256 verify, cache.
//!
//! Model weights are the one thing this codebase cannot vendor: they are
//! hundreds of MB, some carry non-MIT licences (Parakeet weights are
//! CC-BY-4.0), and users on macOS 26 may never need any of them (Apple's
//! backend is zero-install). So models are fetched on demand into
//! `~/.aqua-oss/models`, verified by SHA256, and re-download resumes from
//! where it stopped, because a 600MB fetch dying at 95% on hotel Wi-Fi and
//! restarting from zero is how apps earn one-star reviews.
//!
//! Progress is reported honestly: bytes done out of total when the server
//! sends a length, bytes-done-only when it does not. No fake percentages.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// A downloadable model file. Checksums pin the exact artifact so a rehosted
/// or tampered file fails loudly instead of degrading accuracy silently.
#[derive(Debug, Clone)]
pub struct ModelSpec {
    /// Stable identifier, doubles as the cache filename stem.
    pub id: &'static str,
    pub url: &'static str,
    /// Lowercase hex SHA256 of the complete file. `None` means "not yet
    /// pinned": the download completes with a warning and prints the hash
    /// so it can be pinned in a follow-up commit.
    pub sha256: Option<&'static str>,
    /// Approximate size, for progress display before the first byte.
    pub approx_bytes: u64,
    /// Weight licence, surfaced in the UI because it is *not* the app's
    /// MIT licence and users redistributing bundles need to know.
    pub license: &'static str,
}

/// The registry of every model any backend might request.
pub fn registry() -> Vec<ModelSpec> {
    vec![
        ModelSpec {
            id: "silero-vad",
            url: "https://github.com/snakers4/silero-vad/raw/v5.1.2/src/silero_vad/data/silero_vad.onnx",
            sha256: None, // pin after first verified fetch
            approx_bytes: 2_300_000,
            license: "MIT",
        },
        ModelSpec {
            id: "whisper-base.en",
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin",
            sha256: None,
            approx_bytes: 148_000_000,
            license: "MIT",
        },
        ModelSpec {
            id: "parakeet-tdt-0.6b-v2-onnx",
            // istupakov's ONNX export of nvidia/parakeet-tdt-0.6b-v2, the
            // artifact sherpa-onnx consumes.
            url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v2-onnx/resolve/main/encoder-model.int8.onnx",
            sha256: None,
            approx_bytes: 652_000_000,
            license: "CC-BY-4.0",
        },
    ]
}

/// Where models live. Overridable for tests via `base`.
pub fn default_cache_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".aqua-oss").join("models")
}

/// Download progress, delivered after every chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub bytes_done: u64,
    /// Total when known (Content-Length or resume math); `None` otherwise.
    pub bytes_total: Option<u64>,
}

/// Errors that callers are expected to handle distinctly.
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

/// Fetch `spec` into `cache_dir`, resuming a partial download if one
/// exists. Returns the path to the verified file.
///
/// Layout: the in-progress file is `<id>.partial`; only after the checksum
/// passes is it renamed to `<id>`. The rename is atomic on the same
/// filesystem, so a present final file is always a verified one.
pub fn fetch(
    spec: &ModelSpec,
    cache_dir: &Path,
    mut on_progress: impl FnMut(Progress),
) -> Result<PathBuf, FetchError> {
    std::fs::create_dir_all(cache_dir)?;
    let final_path = cache_dir.join(spec.id);
    if final_path.exists() {
        // Present implies verified (see rename invariant above), so opening
        // the app offline with a warm cache costs zero hashing time.
        return Ok(final_path);
    }
    let partial_path = cache_dir.join(format!("{}.partial", spec.id));
    let existing = std::fs::metadata(&partial_path)
        .map(|m| m.len())
        .unwrap_or(0);

    let mut request = ureq::get(spec.url);
    if existing > 0 {
        // RFC 7233 resume. Servers that ignore Range return 200 with the
        // whole body; we detect that below and start over.
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

    // Verify the *whole* file, including any resumed prefix; a corrupt
    // prefix from a previous crash must not survive.
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
        // Not pinned yet: succeed, but leave a breadcrumb for pinning.
        eprintln!(
            "warning: model {} has no pinned sha256; downloaded file hashes to {actual}",
            spec.id
        );
    }
    std::fs::rename(&partial_path, &final_path)?;
    Ok(final_path)
}

/// SHA256 of a file, streamed so 600MB models do not need 600MB of RAM.
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
        let dir = std::env::temp_dir().join("aqua-asr-test-sha");
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
        let dir = std::env::temp_dir().join("aqua-asr-test-cache");
        std::fs::create_dir_all(&dir).unwrap();
        let spec = ModelSpec {
            id: "already-here",
            // Unroutable URL proves no network attempt happens on cache hit.
            url: "http://192.0.2.1/never",
            sha256: None,
            approx_bytes: 3,
            license: "MIT",
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
        }
    }
}
