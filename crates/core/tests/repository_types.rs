use hfdx_core::repository::*;
use serde::{Deserialize, Serialize};

fn defaults() -> RepositoryRequest {
    RepositoryRequest {
        endpoint: Endpoint::parse("https://mirror.example", false).unwrap(),
        kind: RepoKind::Dataset,
        repo_id: RepoId::parse("original/repo").unwrap(),
        requested_revision: Revision::parse("refs/pr/42").unwrap(),
    }
}

#[test]
fn endpoints_normalize_and_preserve_explicit_http_consent() {
    let endpoint = Endpoint::parse("HTTPS://EXAMPLE.COM:443/", false).unwrap();
    assert_eq!(endpoint.url().as_str(), "https://example.com/");
    assert!(Endpoint::parse("http://127.0.0.1:1234", false).is_err());
    let local = Endpoint::parse("http://127.0.0.1:1234", true).unwrap();
    let encoded = serde_json::to_value(&local).unwrap();
    assert_eq!(
        encoded,
        serde_json::json!({"url":"http://127.0.0.1:1234/", "allow_http":true})
    );
    assert_eq!(serde_json::from_value::<Endpoint>(encoded).unwrap(), local);
    for bad in [
        "",
        "ftp://example.com",
        "https://fake:secret@example.com",
        "https://@example.com",
        "https://example.com/path",
        "https://example.com/a/..",
        "https://example.com/%2e",
        "https://example.com/.",
        "https://example.com?token=secret",
        "https://example.com#fragment",
        " https://example.com",
        "https://example.com\n",
        "https:\\example.com",
    ] {
        assert!(Endpoint::parse(bad, true).is_err(), "accepted {bad:?}");
    }
    assert!(
        serde_json::from_value::<Endpoint>(
            serde_json::json!({"url":"http://example.com", "allow_http":false})
        )
        .is_err()
    );
}

#[test]
fn repository_inputs_bind_url_origin_and_preserve_revision() {
    let base = defaults();
    for (input, kind, id, endpoint) in [
        ("gpt2", RepoKind::Dataset, "gpt2", "https://mirror.example/"),
        (
            "org/model",
            RepoKind::Dataset,
            "org/model",
            "https://mirror.example/",
        ),
        (
            "https://hf.example/org/model/",
            RepoKind::Model,
            "org/model",
            "https://hf.example/",
        ),
        (
            "https://hf.example/gpt2",
            RepoKind::Model,
            "gpt2",
            "https://hf.example/",
        ),
        (
            "https://hf.example/datasets/org/data",
            RepoKind::Dataset,
            "org/data",
            "https://hf.example/",
        ),
        (
            "https://hf.example/datasets/data",
            RepoKind::Dataset,
            "data",
            "https://hf.example/",
        ),
        (
            "https://hf.example/%6Frg/model",
            RepoKind::Model,
            "org/model",
            "https://hf.example/",
        ),
    ] {
        let parsed = parse_repository(input, &base).unwrap();
        assert_eq!(
            (
                parsed.kind,
                parsed.repo_id.as_str(),
                parsed.endpoint.url().as_str()
            ),
            (kind, id, endpoint)
        );
        assert_eq!(parsed.requested_revision.as_str(), "refs/pr/42");
    }
    assert_eq!(base.repo_id.as_str(), "original/repo");
    let mut http = base.clone();
    assert!(parse_repository("http://localhost:1234/org/model", &http).is_err());
    http.endpoint = Endpoint::parse("http://localhost:1234", true).unwrap();
    assert_eq!(
        parse_repository("http://localhost:5678/org/model", &http)
            .unwrap()
            .endpoint
            .url()
            .port(),
        Some(5678)
    );
}

#[test]
fn repository_urls_reject_pages_ambiguity_and_hidden_input() {
    for input in [
        "",
        "org/repo/extra",
        "https://hf.example/org/repo/blob/main/file",
        "https://hf.example/org/repo/tree/main",
        "https://hf.example/org/repo/file/main",
        "https://hf.example/org/repo?x=1",
        "https://hf.example/org/repo#x",
        "https://fake:secret@hf.example/org/repo",
        "https://hf.example//org/repo",
        "https://hf.example/org/repo//",
        "https://hf.example/org/../repo",
        "https://hf.example/org/%2e%2e/repo",
        "https://hf.example/org%2frepo",
        "https://hf.example/org/%zz",
        "https://hf.example/org/%ff",
        "https://hf.example",
        "https://hf.example/datasets/",
    ] {
        assert!(
            parse_repository(input, &defaults()).is_err(),
            "accepted {input:?}"
        );
    }
}

#[test]
fn validated_text_values_reject_invalid_deserialization() {
    for valid in ["gpt2", "org/model-v1.0", "org_1/model"] {
        assert_eq!(RepoId::parse(valid).unwrap().as_str(), valid);
    }
    for bad in [
        "",
        "org/",
        "/repo",
        "a/b/c",
        "../repo",
        "a/..",
        "org/a b",
        "org/repo?x",
        "org/中文",
        "org\\repo",
        "org/a\0",
    ] {
        assert!(RepoId::parse(bad).is_err());
        assert!(serde_json::from_value::<RepoId>(serde_json::json!(bad)).is_err());
    }
    for valid in ["main", "v1.0", "refs/pr/42", "feature/中文"] {
        assert_eq!(Revision::parse(valid).unwrap().as_str(), valid);
    }
    for bad in [
        "", "/main", "main/", "a//b", "../main", "a..b", "a b", "a\\b", "a\n", "a.lock", "a@{b",
    ] {
        assert!(Revision::parse(bad).is_err());
        assert!(serde_json::from_value::<Revision>(serde_json::json!(bad)).is_err());
    }
    for length in [40, 64] {
        let commit = CommitId::parse(&"A".repeat(length)).unwrap();
        assert_eq!(commit.as_str(), "a".repeat(length));
        assert_eq!(
            serde_json::from_value::<CommitId>(serde_json::json!(commit.as_str())).unwrap(),
            commit
        );
    }
    for bad in [
        "main".to_owned(),
        "a".repeat(39),
        "g".repeat(40),
        "a".repeat(65),
    ] {
        assert!(CommitId::parse(&bad).is_err());
        assert!(serde_json::from_value::<CommitId>(serde_json::json!(bad)).is_err());
    }
}

#[test]
fn repository_paths_reject_escape_and_keep_literal_file_names() {
    for valid in [
        "config.json",
        "special/space # 中文.bin",
        "literal%2Fname",
        "a/.../b",
    ] {
        let path = RepoPath::parse(valid).unwrap();
        assert_eq!(path.as_str(), valid);
        assert_eq!(
            serde_json::from_value::<RepoPath>(serde_json::json!(valid)).unwrap(),
            path
        );
    }
    for bad in [
        "",
        "/root",
        "//server/share",
        "../file",
        "a/../b",
        "./a",
        "a/./b",
        "a//b",
        "a/",
        "C:/file",
        "c:file",
        "a/C:/file",
        "a\\b",
        "\\\\server\\share",
        "a\0b",
        "a\nb",
        "a\u{7f}b",
        "a\u{85}b",
    ] {
        assert!(
            matches!(RepoPath::parse(bad), Err(RepositoryError::UnsafePath)),
            "accepted {bad:?}"
        );
        assert!(serde_json::from_value::<RepoPath>(serde_json::json!(bad)).is_err());
    }
}

#[test]
fn secret_types_have_no_serde_implementation() {
    macro_rules! assert_not_serde {
        ($value:ty) => {{
            trait Ambiguous<A> {
                fn check() {}
            }
            impl<T: ?Sized> Ambiguous<()> for T {}
            impl<T: ?Sized + Serialize> Ambiguous<u8> for T {}
            impl<T: for<'de> Deserialize<'de>> Ambiguous<u16> for T {}
            let _ = <$value as Ambiguous<_>>::check;
        }};
    }
    assert_not_serde!(Token);
    assert_not_serde!(ProxySpec);
    assert_not_serde!(DownloadUrl);
    assert_not_serde!(NetworkAccess);
}

#[test]
fn repository_error_formatting_never_echoes_raw_input() {
    let error =
        RepositoryError::Input("hf_fake-token https://user:fake-pass@host?signature=secret".into());
    assert!(!format!("{error} {error:?}").contains("fake"));
    if let RepositoryError::Input(message) = error {
        assert!(message.contains("hf_fake-token"));
    }
    let endpoint = Endpoint::parse("https://hf.example", false).unwrap();
    let error = RepositoryError::Http {
        endpoint,
        status: 403,
    };
    assert!(error.to_string().contains("403"));
    assert!(error.to_string().contains("https://hf.example/"));
    for error in [
        RepositoryError::Network,
        RepositoryError::UnsupportedEndpoint,
        RepositoryError::UnsafePath,
        RepositoryError::InvalidResponse,
        RepositoryError::Cancelled,
    ] {
        assert!(!error.to_string().is_empty());
    }
}
