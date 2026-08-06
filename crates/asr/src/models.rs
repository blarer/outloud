//! Model manager: download with resume, SHA256 verify, cache.
//!
//! Model weights are the one thing this codebase cannot vendor: they are
//! hundreds of MB, some carry non-MIT licences (Parakeet weights are
//! CC-BY-4.0), and users on macOS 26 may never need any of them (Apple's
//! backend is zero-install). So models are fetched on demand into
//! `~/.outloud/models`, verified by SHA256, and re-download resumes from
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
            // Fetched twice over independent transports, GitHub raw and the
            // jsdelivr CDN mirror of the same tag, and hashed: identical
            // bytes and identical length both times. GitHub publishes only
            // git blob SHA1s, so unlike the Hugging Face models there is no
            // upstream SHA256 to compare against; two transports is the
            // strongest evidence available for this one.
            sha256: Some("2623a2953f6ff3d2c1e61740c6cdb7168133479b267dfef114a4a3cc5bdd788f"),
            approx_bytes: 2_327_524,
            license: "MIT",
        },
        ModelSpec {
            id: "whisper-base.en",
            url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin",
            // Pinned against the hash Hugging Face publishes for the file
            // (LFS object id, which is the SHA256 of the content:
            // `POST /api/models/ggerganov/whisper.cpp/paths-info/main`
            // with `{"paths":["ggml-base.en.bin"]}`), cross-checked against
            // a local `sha256sum` of a fetched copy. Both agree, and both
            // agree on the size below, so an unpinned fetch is no longer
            // the only thing standing between a rehosted file and the
            // user's microphone.
            sha256: Some("a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002"),
            approx_bytes: 147_964_211,
            license: "MIT",
        },
        ModelSpec {
            id: "parakeet-tdt-0.6b-v2-onnx",
            // istupakov's ONNX export of nvidia/parakeet-tdt-0.6b-v2, the
            // artifact sherpa-onnx consumes.
            url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v2-onnx/resolve/main/encoder-model.int8.onnx",
            // From Hugging Face's published hash for the file (the LFS
            // object id is the SHA256 of the content), which is why this
            // could be pinned without downloading 652MB. Note the tradeoff:
            // this pins the file as `main` served it, and a repo that moves
            // its main branch will now fail loudly instead of silently
            // swapping the model under us. That is the intended behaviour.
            sha256: Some("3e0581fda6ab843888b51e56d7ee78b6d5bc3237ec113af1f732d1d5286aa155"),
            approx_bytes: 652_184_014,
            license: "CC-BY-4.0",
        },
    ]
}

/// Where models live. Overridable for tests via `base`.
///
/// Delegated to `config::paths` so the recognizer, the LLM cache and the
/// doctor cannot drift apart about the directory, and so the rename from
/// `~/.aqua-oss` has exactly one migration.
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
        // Present implies verified *if this process put it there*. Files
        // arriving any other way break that: the documented
        // `curl -o ~/.outloud/models/whisper-base.en ...` shortcut, a
        // hand-copied model, and every file cached before its hash was
        // pinned. Those are exactly the paths an attacker or a bad mirror
        // would use, so a pinned model with no verification marker is
        // hashed once, here, rather than trusted forever.
        return verify_cached(spec, &final_path).map(|()| final_path);
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
    mark_verified(&final_path);
    Ok(final_path)
}

/// Path of the marker recording that `<model>` was checksum-verified.
///
/// A sidecar rather than a field in a database: it lives and dies with the
/// model file, survives nothing being written atomically anywhere else, and
/// a user who deletes it only costs themselves one re-hash.
fn verified_marker(model_path: &Path) -> PathBuf {
    let mut name = model_path.file_name().unwrap_or_default().to_os_string();
    name.push(".verified");
    model_path.with_file_name(name)
}

fn mark_verified(model_path: &Path) {
    // Best effort: a missing marker costs a re-hash on next launch, which is
    // cheap, while failing the fetch over an unwritable sidecar would deny
    // the user a model they successfully downloaded and verified.
    let _ = std::fs::write(verified_marker(model_path), b"sha256-ok\n");
}

/// Ensure a cached file matches its pinned hash, hashing it at most once.
///
/// Unpinned models are accepted as before: there is nothing to check them
/// against, and refusing to run would only punish users for a checksum the
/// project has not yet recorded.
///
/// A mismatch deletes the file. Leaving it would mean the next launch hits
/// the same error, and the honest state after "this is not the model you
/// asked for" is no model at all.
pub fn verify_cached(spec: &ModelSpec, model_path: &Path) -> Result<(), FetchError> {
    let Some(expected) = spec.sha256 else {
        return Ok(());
    };
    let marker = verified_marker(model_path);
    if marker.exists() {
        return Ok(());
    }
    let actual = sha256_file(model_path)?;
    if actual != expected {
        let _ = std::fs::remove_file(model_path);
        return Err(FetchError::ChecksumMismatch {
            id: spec.id.to_string(),
            expected: expected.to_string(),
            actual,
        });
    }
    mark_verified(model_path);
    Ok(())
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
        let dir = std::env::temp_dir().join("outloud-asr-test-sha");
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
        let dir = std::env::temp_dir().join("outloud-asr-test-cache");
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

    /// A file that arrived outside `fetch` (documented `curl` shortcut,
    /// hand copy, pre-pin cache) must not be trusted just because it is
    /// present under the right name.
    #[test]
    fn a_cached_file_that_fails_its_pin_is_rejected_and_removed() {
        let dir = std::env::temp_dir().join("outloud-asr-test-tampered");
        std::fs::create_dir_all(&dir).unwrap();
        let spec = ModelSpec {
            id: "pinned-model",
            // Unroutable: a rejection must not fall back to downloading.
            url: "http://192.0.2.1/never",
            // sha256 of "abc", while the file below contains something else.
            sha256: Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
            approx_bytes: 3,
            license: "MIT",
        };
        let path = dir.join(spec.id);
        std::fs::write(&path, b"not abc").unwrap();

        let err = fetch(&spec, &dir, |_| {}).unwrap_err();
        assert!(
            matches!(err, FetchError::ChecksumMismatch { .. }),
            "got {err}"
        );
        assert!(!path.exists(), "a file that failed its pin must not linger");
    }

    /// The happy path costs one hash, then none: a marker records the
    /// verdict so launching the app does not re-hash 148MB every time.
    #[test]
    fn a_matching_cached_file_is_verified_once_then_marked() {
        let dir = std::env::temp_dir().join("outloud-asr-test-verified");
        std::fs::create_dir_all(&dir).unwrap();
        let spec = ModelSpec {
            id: "good-model",
            url: "http://192.0.2.1/never",
            sha256: Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
            approx_bytes: 3,
            license: "MIT",
        };
        let path = dir.join(spec.id);
        std::fs::write(&path, b"abc").unwrap();
        let marker = dir.join("good-model.verified");
        let _ = std::fs::remove_file(&marker);

        assert_eq!(fetch(&spec, &dir, |_| {}).unwrap(), path);
        assert!(marker.exists(), "verification verdict was not recorded");

        // With the marker present the content is not read again, which is
        // what makes the check affordable at every launch.
        std::fs::write(&path, b"changed after verification").unwrap();
        assert_eq!(fetch(&spec, &dir, |_| {}).unwrap(), path);
    }

    /// Every registry entry must be pinned. An unpinned model downloads
    /// behind a warning nobody reads, which is exactly how whisper-base.en,
    /// silero-vad and parakeet stayed unverified for months while the
    /// download code looked complete.
    #[test]
    fn registry_entries_are_well_formed() {
        for spec in registry() {
            assert!(spec.sha256.is_some(), "{} has no pinned sha256", spec.id);
            assert!(spec.url.starts_with("https://"), "{}", spec.id);
            assert!(!spec.license.is_empty(), "{}", spec.id);
            assert!(spec.approx_bytes > 0, "{}", spec.id);
        }
    }
}
