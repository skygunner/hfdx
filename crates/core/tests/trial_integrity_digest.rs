use hfdx_core::integrity::{IntegrityResult, verify_file};
use hfdx_core::{ContentHash, HashAlgorithm};
use std::io::{self, Write};
use std::process::{Command, Stdio};
use tokio_util::sync::CancellationToken;

#[test]
fn known_vectors_match_for_empty_and_nonempty_files() {
    let cases: &[(&[u8], HashAlgorithm, &str)] = &[
        (
            b"",
            HashAlgorithm::Sha256,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            b"abc",
            HashAlgorithm::Sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            b"",
            HashAlgorithm::GitSha1,
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391",
        ),
        (
            b"hello\n",
            HashAlgorithm::GitSha1,
            "ce013625030ba8dba906f756967f9e9ca394464a",
        ),
    ];
    for &(content, algorithm, hex) in cases {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(content).unwrap();
        let expected = ContentHash {
            algorithm,
            hex: hex.into(),
        };
        assert_eq!(
            verify_file(
                file.path(),
                Some(&expected),
                &CancellationToken::new(),
                |_, _| {}
            )
            .unwrap(),
            IntegrityResult::Matched
        );
    }
}

#[test]
fn git_hash_object_independently_agrees_across_buffer_boundary() {
    let content = vec![b'x'; 65_537];
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&content).unwrap();
    let output = Command::new("git")
        .args(["hash-object", "--stdin"])
        .env("GIT_MASTER", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut child = output;
    child.stdin.take().unwrap().write_all(&content).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let expected = ContentHash {
        algorithm: HashAlgorithm::GitSha1,
        hex: String::from_utf8(output.stdout).unwrap().trim().to_owned(),
    };
    let mut progress = Vec::new();
    assert_eq!(
        verify_file(
            file.path(),
            Some(&expected),
            &CancellationToken::new(),
            |done, total| progress.push((done, total))
        )
        .unwrap(),
        IntegrityResult::Matched
    );
    assert_eq!(progress, [(0, 65_537), (65_536, 65_537), (65_537, 65_537)]);
}

#[test]
fn mismatch_returns_actual_digest_and_missing_hash_is_unavailable() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(b"abc").unwrap();
    let expected = ContentHash {
        algorithm: HashAlgorithm::Sha256,
        hex: "0".repeat(64),
    };
    assert_eq!(
        verify_file(
            file.path(),
            Some(&expected),
            &CancellationToken::new(),
            |_, _| {}
        )
        .unwrap(),
        IntegrityResult::Mismatch {
            actual: ContentHash {
                algorithm: HashAlgorithm::Sha256,
                hex: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into()
            },
        }
    );
    assert_eq!(
        verify_file(
            &file.path().join("missing"),
            None,
            &CancellationToken::new(),
            |_, _| {}
        )
        .unwrap(),
        IntegrityResult::Unavailable
    );
}

#[test]
fn malformed_hash_and_precancelled_request_are_rejected() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let expected = ContentHash {
        algorithm: HashAlgorithm::GitSha1,
        hex: "z".repeat(40),
    };
    assert_eq!(
        verify_file(
            file.path(),
            Some(&expected),
            &CancellationToken::new(),
            |_, _| {}
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::InvalidInput
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        verify_file(file.path(), Some(&expected), &cancel, |_, _| {})
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
}

#[test]
fn cancellation_from_progress_stops_reading_before_next_chunk() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&vec![b'x'; 65_537]).unwrap();
    let expected = ContentHash {
        algorithm: HashAlgorithm::Sha256,
        hex: "0".repeat(64),
    };
    let cancel = CancellationToken::new();
    let mut seen = Vec::new();
    let result = verify_file(file.path(), Some(&expected), &cancel, |done, total| {
        seen.push((done, total));
        if done > 0 {
            cancel.cancel();
        }
    });
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
    assert_eq!(seen, [(0, 65_537), (65_536, 65_537)]);
}
