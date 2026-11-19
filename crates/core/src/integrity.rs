//! Streaming verification against hashes from the saved repository catalog.

use crate::{ContentHash, HashAlgorithm};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use tokio_util::sync::CancellationToken;

/// Outcome of checking a file against its expected content hash.
#[derive(Debug, PartialEq, Eq)]
pub enum IntegrityResult {
    Matched,
    Mismatch { actual: ContentHash },
    Unavailable,
}

/// Hashes a file using a fixed 64 KiB buffer and compares it with trusted metadata.
///
/// This synchronous operation belongs on a joined blocking worker. Cancellation is
/// checked between reads; a filesystem read already in progress cannot be interrupted.
/// Missing metadata returns `Unavailable` without opening the file. The callback
/// receives processed and total bytes initially and after each chunk; it runs on the
/// caller's thread and should update a bounded, nonblocking progress sink.
///
/// # Errors
/// Returns an I/O error for invalid hash metadata, cancellation, read errors, or a
/// file whose length changes during the read.
pub fn verify_file(
    path: &Path,
    expected: Option<&ContentHash>,
    cancel: &CancellationToken,
    mut progress: impl FnMut(u64, u64),
) -> io::Result<IntegrityResult> {
    if cancel.is_cancelled() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "verification cancelled",
        ));
    }
    let Some(expected) = expected else {
        return Ok(IntegrityResult::Unavailable);
    };
    let hex_len = match expected.algorithm {
        HashAlgorithm::Sha256 => 64,
        HashAlgorithm::GitSha1 => 40,
    };
    if expected.hex.len() != hex_len || !expected.hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid expected hash",
        ));
    }

    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let mut sha256 = Sha256::new();
    let mut sha1 = Sha1::new();
    if expected.algorithm == HashAlgorithm::GitSha1 {
        sha1.update(format!("blob {size}\0").as_bytes());
    }
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    progress(total, size);
    loop {
        if cancel.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "verification cancelled",
            ));
        }
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(
                u64::try_from(read).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "file length overflow")
                })?,
            )
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "file length overflow"))?;
        match expected.algorithm {
            HashAlgorithm::Sha256 => sha256.update(&buffer[..read]),
            HashAlgorithm::GitSha1 => sha1.update(&buffer[..read]),
        }
        progress(total, size);
    }
    if cancel.is_cancelled() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "verification cancelled",
        ));
    }
    if total != size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file length changed during verification",
        ));
    }
    let hex = match expected.algorithm {
        HashAlgorithm::Sha256 => format!("{:x}", sha256.finalize()),
        HashAlgorithm::GitSha1 => format!("{:x}", sha1.finalize()),
    };
    if hex.eq_ignore_ascii_case(&expected.hex) {
        Ok(IntegrityResult::Matched)
    } else {
        Ok(IntegrityResult::Mismatch {
            actual: ContentHash {
                algorithm: expected.algorithm,
                hex,
            },
        })
    }
}
