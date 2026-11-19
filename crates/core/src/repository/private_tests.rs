use super::*;

#[test]
fn resolve_path_encodes_special_file_segments_and_pins_commit() {
    let source = RepositorySpec {
        request: RepositoryRequest {
            endpoint: Endpoint::parse("https://example.test", false).unwrap(),
            kind: RepoKind::Model,
            repo_id: RepoId::parse("org/model").unwrap(),
            requested_revision: Revision::parse("feature/branch").unwrap(),
        },
        resolved_commit: CommitId::parse(&"a".repeat(40)).unwrap(),
    };
    let entry = CatalogEntry {
        path: RepoPath::parse("special/space # 中文.bin").unwrap(),
        size: Bytes::new(1024),
        content_hash: None,
    };
    let url = resolve_url(&source, &entry).unwrap();
    assert_eq!(
        url.as_str(),
        format!(
            "https://example.test/org/model/resolve/{}/special/space%20%23%20%E4%B8%AD%E6%96%87.bin",
            "a".repeat(40)
        )
    );
    assert_eq!(
        metadata_url(&source.request).unwrap().path(),
        "/api/models/org/model/revision/feature%2Fbranch"
    );
}
