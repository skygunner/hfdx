use super::*;
use std::collections::{BTreeSet, HashSet};

fn defaults() -> RepositoryRequest {
    RepositoryRequest {
        endpoint: Endpoint::parse("https://mirror.example", false).unwrap(),
        kind: RepoKind::Dataset,
        repo_id: RepoId::parse("original/repo").unwrap(),
        requested_revision: Revision::parse("refs/pr/42").unwrap(),
    }
}

#[test]
fn url_assembly_encodes_revision_as_one_segment_and_files_by_segment() {
    let request = defaults();
    assert_eq!(
        super::metadata_url(&request).unwrap().as_str(),
        "https://mirror.example/api/datasets/original/repo/revision/refs%2Fpr%2F42"
    );
    let source = RepositorySpec {
        request,
        resolved_commit: CommitId::parse(&"a".repeat(40)).unwrap(),
    };
    let entry = CatalogEntry {
        path: RepoPath::parse("special/space # 中文%2F.bin").unwrap(),
        size: Bytes::new(1024),
        content_hash: None,
    };
    assert_eq!(
        super::resolve_url(&source, &entry).unwrap().as_str(),
        format!(
            "https://mirror.example/datasets/original/repo/resolve/{}/special/space%20%23%20%E4%B8%AD%E6%96%87%252F.bin",
            "a".repeat(40)
        )
    );
    let mut model = defaults();
    model.kind = RepoKind::Model;
    assert_eq!(
        super::metadata_url(&model).unwrap().path(),
        "/api/models/original/repo/revision/refs%2Fpr%2F42"
    );
}

#[test]
fn catalog_values_round_trip_and_ids_distinguish_generations() {
    let id = CatalogId(7);
    let entry_id = EntryId {
        catalog: id,
        index: 3,
    };
    let newer = EntryId {
        catalog: CatalogId(8),
        index: 3,
    };
    assert_ne!(entry_id, newer);
    assert!(entry_id < newer);
    assert_eq!(HashSet::from([entry_id, entry_id, newer]).len(), 2);
    assert_eq!(BTreeSet::from([newer, entry_id]).first(), Some(&entry_id));
    let source = RepositorySpec {
        request: defaults(),
        resolved_commit: CommitId::parse(&"a".repeat(40)).unwrap(),
    };
    let request = CatalogRequest { id, source };
    let encoded = serde_json::to_value(&request).unwrap();
    assert_eq!(
        serde_json::from_value::<CatalogRequest>(encoded).unwrap(),
        request
    );
    let page = CatalogPage {
        catalog: id,
        entries: vec![CatalogEntry {
            path: RepoPath::parse("empty.bin").unwrap(),
            size: Bytes::new(0),
            content_hash: Some(ContentHash {
                algorithm: HashAlgorithm::Sha256,
                hex: "a".repeat(64),
            }),
        }],
        complete: true,
    };
    assert_eq!(
        serde_json::from_value::<CatalogPage>(serde_json::to_value(&page).unwrap()).unwrap(),
        page
    );
    assert_eq!(Bytes::new(u64::MAX).get(), u64::MAX);
    assert!(serde_json::from_str::<Bytes>("-1").is_err());
    assert!(serde_json::from_str::<Bytes>("18446744073709551616").is_err());
    for (algorithm, length) in [(HashAlgorithm::Sha256, 64), (HashAlgorithm::GitSha1, 40)] {
        assert_eq!(
            serde_json::from_value::<ContentHash>(
                serde_json::json!({"algorithm":algorithm,"hex":"A".repeat(length)})
            )
            .unwrap()
            .hex,
            "a".repeat(length)
        );
        for hex in ["a".repeat(length - 1), "z".repeat(length)] {
            assert!(
                serde_json::from_value::<ContentHash>(
                    serde_json::json!({"algorithm":algorithm,"hex":hex})
                )
                .is_err()
            );
        }
    }
}

#[test]
fn secrets_stay_in_memory_and_safe_proxy_address_round_trips() {
    let proxy = ProxySpec::parse("https://fake-user:fake-pass@PROXY.example:8443/").unwrap();
    let address = proxy.address();
    assert_eq!(address.url().as_str(), "https://proxy.example:8443/");
    assert_eq!(proxy.authenticated_url().username(), "fake-user");
    assert_eq!(
        serde_json::to_value(&address).unwrap(),
        "https://proxy.example:8443/"
    );
    assert_eq!(
        serde_json::from_value::<ProxyAddress>(serde_json::to_value(&address).unwrap()).unwrap(),
        address
    );
    assert!(ProxyAddress::parse("http://fake-user:fake-pass@proxy.example").is_err());
    assert!(
        serde_json::from_str::<ProxyAddress>("\"http://fake-user:fake-pass@proxy.example\"")
            .is_err()
    );
    let access = NetworkAccess {
        token: Some(Token::parse("hf_fake-token").unwrap()),
        proxy: Some(proxy),
    };
    assert_eq!(access.token.as_ref().unwrap().expose(), "hf_fake-token");
    let download = DownloadUrl::from_url(
        url::Url::parse("https://cdn.example/file?signature=fake-signature").unwrap(),
    )
    .unwrap();
    assert_eq!(download.url().query(), Some("signature=fake-signature"));
    for debug in [
        format!("{access:?}"),
        format!("{download:?}"),
        download.to_string(),
    ] {
        for secret in ["fake-user", "fake-pass", "hf_fake-token", "fake-signature"] {
            assert!(!debug.contains(secret));
        }
    }
    for bad in ["", "a b", "abc\n", "abc\0"] {
        assert!(Token::parse(bad).is_err());
    }
    for bad in [
        "socks5://proxy.example",
        "http://proxy.example/path",
        "http://proxy.example?secret=x",
    ] {
        assert!(ProxySpec::parse(bad).is_err());
    }
    assert!(DownloadUrl::from_url(url::Url::parse("file:///tmp/file").unwrap()).is_err());
    assert!(
        DownloadUrl::from_url(url::Url::parse("https://user:fake@cdn.example/file").unwrap())
            .is_err()
    );
    assert!(NetworkAccess::default().token.is_none());
    assert!(NetworkAccess::default().proxy.is_none());
}
