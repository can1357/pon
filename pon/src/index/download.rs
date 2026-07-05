//! Artifact acquisition for package indexes.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// Hash verification mode for an artifact download.
#[derive(Debug)]
pub enum HashPolicy<'a> {
    /// Verify against index-provided hashes when a sha256 digest is present.
    ///
    /// If the index provided no sha256 digest, the artifact is accepted.
    Index(&'a BTreeMap<String, String>),
    /// Require the artifact to match one of the provided `algo:hex` hashes.
    ///
    /// Only `sha256` is supported. Unsupported algorithms are rejected before
    /// any hash comparison is attempted.
    Required(&'a [String]),
}

/// Materialize `url` as a local artifact and verify it with `policy`.
///
/// `file://` URLs are verified in place and returned without copying. HTTP(S)
/// URLs are streamed synchronously into `<cache_dir>/wheels/<sha256(url)>/`,
/// first as `<filename>.part` and then atomically renamed to `filename` after
/// the downloaded bytes satisfy the requested hash policy.
pub fn download_artifact(
    cache_dir: &Path,
    url: &str,
    filename: &str,
    policy: &HashPolicy<'_>,
) -> Result<PathBuf> {
    if url.starts_with("file://") {
        return verify_file_url(url, filename, policy);
    }

    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(Error::Index(format!("unsupported artifact URL `{url}`")));
    }

    let artifact_path = cached_artifact_path(cache_dir, url, filename)?;
    if artifact_path.is_file() {
        let actual = sha256_file(&artifact_path)?;
        match check_digest(&actual, policy)? {
            DigestCheck::Pass => return Ok(artifact_path),
            DigestCheck::Mismatch { .. } => remove_file_if_exists(&artifact_path)?,
        }
    }

    let parent = artifact_path.parent().ok_or_else(|| {
        Error::Index(format!(
            "could not determine cache directory for artifact `{filename}`"
        ))
    })?;
    fs::create_dir_all(parent)?;

    let part_path = parent.join(format!("{filename}.part"));
    let actual = download_to_part(url, &part_path)?;

    match check_digest(&actual, policy) {
        Ok(DigestCheck::Pass) => {
            if let Err(error) = fs::rename(&part_path, &artifact_path) {
                let _ = fs::remove_file(&part_path);
                return Err(Error::from(error));
            }
            Ok(artifact_path)
        }
        Ok(DigestCheck::Mismatch { expected }) => {
            let _ = fs::remove_file(&part_path);
            Err(hash_mismatch_error(filename, &expected, &actual))
        }
        Err(error) => {
            let _ = fs::remove_file(&part_path);
            Err(error)
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum DigestCheck {
    Pass,
    Mismatch { expected: String },
}

fn verify_file_url(url: &str, filename: &str, policy: &HashPolicy<'_>) -> Result<PathBuf> {
    let path = file_url_path(url)?;
    let actual = sha256_file(&path)?;
    match check_digest(&actual, policy)? {
        DigestCheck::Pass => Ok(path),
        DigestCheck::Mismatch { expected } => Err(hash_mismatch_error(filename, &expected, &actual)),
    }
}

fn cached_artifact_path(cache_dir: &Path, url: &str, filename: &str) -> Result<PathBuf> {
    if Path::new(filename).file_name().and_then(|name| name.to_str()) != Some(filename) {
        return Err(Error::Index(format!(
            "artifact filename `{filename}` is not a plain file name"
        )));
    }

    Ok(cache_dir
        .join("wheels")
        .join(sha256_hex(url.as_bytes()))
        .join(filename))
}

fn download_to_part(url: &str, part_path: &Path) -> Result<String> {
    remove_file_if_exists(part_path)?;

    let result = (|| {
        let mut response = ureq::get(url)
            .call()
            .map_err(|error| Error::Index(format!("failed to download artifact `{url}`: {error}")))?;
        let mut reader = response.body_mut().as_reader();
        let mut writer = File::create(part_path)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];

        loop {
            let len = reader.read(&mut buffer).map_err(|error| {
                Error::Index(format!(
                    "failed to read artifact response `{url}`: {error}"
                ))
            })?;
            if len == 0 {
                break;
            }
            writer.write_all(&buffer[..len])?;
            hasher.update(&buffer[..len]);
        }

        Ok(finalize_hex(hasher))
    })();

    if result.is_err() {
        let _ = fs::remove_file(part_path);
    }

    result
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];

    loop {
        let len = file.read(&mut buffer)?;
        if len == 0 {
            break;
        }
        hasher.update(&buffer[..len]);
    }

    Ok(finalize_hex(hasher))
}

fn check_digest(actual: &str, policy: &HashPolicy<'_>) -> Result<DigestCheck> {
    let expected = expected_hashes(policy)?;
    let Some(expected) = expected else {
        return Ok(DigestCheck::Pass);
    };

    if expected
        .sha256
        .iter()
        .any(|hash| hash.eq_ignore_ascii_case(actual))
    {
        Ok(DigestCheck::Pass)
    } else {
        Ok(DigestCheck::Mismatch {
            expected: expected.display,
        })
    }
}

#[derive(Debug, Eq, PartialEq)]
struct ExpectedHashes {
    sha256: Vec<String>,
    display: String,
}

fn expected_hashes(policy: &HashPolicy<'_>) -> Result<Option<ExpectedHashes>> {
    match policy {
        HashPolicy::Index(hashes) => {
            let sha256 = hashes
                .iter()
                .filter(|(algorithm, _)| algorithm.eq_ignore_ascii_case("sha256"))
                .map(|(_, hash)| hash.to_owned())
                .collect::<Vec<_>>();
            if sha256.is_empty() {
                Ok(None)
            } else {
                Ok(Some(ExpectedHashes {
                    display: display_hashes(&sha256),
                    sha256,
                }))
            }
        }
        HashPolicy::Required(required) => {
            let mut sha256 = Vec::with_capacity(required.len());
            for entry in *required {
                let (algorithm, hash) = entry
                    .split_once(':')
                    .unwrap_or((entry.as_str(), ""));
                if !algorithm.eq_ignore_ascii_case("sha256") {
                    return Err(Error::Index(format!(
                        "unsupported hash algorithm `{algorithm}`; only sha256 is supported"
                    )));
                }
                sha256.push(hash.to_owned());
            }

            Ok(Some(ExpectedHashes {
                display: display_hashes(&sha256),
                sha256,
            }))
        }
    }
}

fn display_hashes(hashes: &[String]) -> String {
    if hashes.is_empty() {
        return "<none>".to_owned();
    }

    hashes
        .iter()
        .map(|hash| format!("sha256:{hash}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn file_url_path(url: &str) -> Result<PathBuf> {
    let without_scheme = url.strip_prefix("file://").ok_or_else(|| {
        Error::Index(format!("artifact URL `{url}` is not a file URL"))
    })?;
    let without_fragment = without_scheme.split_once('#').map_or(without_scheme, |(path, _)| path);
    let without_query = without_fragment
        .split_once('?')
        .map_or(without_fragment, |(path, _)| path);
    let path = without_query
        .strip_prefix("localhost/")
        .map(|path| format!("/{path}"))
        .unwrap_or_else(|| without_query.to_owned());
    let decoded = percent_decode(&path, url)?;

    #[cfg(windows)]
    {
        if decoded.as_bytes().get(0) == Some(&b'/')
            && decoded.as_bytes().get(2) == Some(&b':')
        {
            return Ok(PathBuf::from(&decoded[1..]));
        }
    }

    Ok(PathBuf::from(decoded))
}

fn percent_decode(input: &str, url: &str) -> Result<String> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }

        if index + 2 >= bytes.len() {
            return Err(invalid_percent_escape(url));
        }
        let high = hex_value(bytes[index + 1]).ok_or_else(|| invalid_percent_escape(url))?;
        let low = hex_value(bytes[index + 2]).ok_or_else(|| invalid_percent_escape(url))?;
        decoded.push((high << 4) | low);
        index += 3;
    }

    String::from_utf8(decoded)
        .map_err(|_| Error::Index(format!("file URL path is not valid UTF-8 in `{url}`")))
}

fn invalid_percent_escape(url: &str) -> Error {
    Error::Index(format!("invalid percent escape in file URL `{url}`"))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn hash_mismatch_error(filename: &str, expected: &str, actual: &str) -> Error {
    Error::Index(format!(
        "hash mismatch for `{filename}`: expected {expected}, got sha256:{actual}"
    ))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex_bytes(&digest[..])
}

fn finalize_hex(hasher: Sha256) -> String {
    let digest = hasher.finalize();
    hex_bytes(&digest[..])
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::from(error)),
    }
}
