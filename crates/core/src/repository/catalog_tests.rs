use super::*;
use serde_json::json;

#[test]
fn tree_hashes_distinguish_lfs_sha256_from_git_blob_sha1() {
    for (lfs, expected_algorithm, expected_length) in [
        (false, HashAlgorithm::GitSha1, 40),
        (true, HashAlgorithm::Sha256, 64),
    ] {
        let mut value = json!({"type":"file","path":"weights.bin","size":123,"oid":"A".repeat(40)});
        if lfs {
            value["lfs"] = json!({"oid":"B".repeat(64),"size":123});
        }
        let raw: TreeEntry = serde_json::from_value(value).unwrap();
        let entry = raw.into_entry().unwrap().unwrap();
        let hash = entry.content_hash.unwrap();
        assert_eq!(hash.algorithm, expected_algorithm);
        assert_eq!(
            hash.hex,
            if lfs { "b" } else { "a" }.repeat(expected_length)
        );
        assert_eq!(entry.size.get(), 123);
    }
    for value in [
        json!({"type":"file","path":"a","size":1,"oid":"invalid"}),
        json!({"type":"file","path":"a","size":1,"lfs":{"oid":"b".repeat(64),"size":2}}),
        json!({"type":"file","path":"a","size":1,"lfs":{"oid":"g".repeat(64),"size":1}}),
    ] {
        assert!(matches!(
            serde_json::from_value::<TreeEntry>(value)
                .unwrap()
                .into_entry(),
            Err(RepositoryError::InvalidResponse)
        ));
    }
    for value in [
        json!({"type":"file","path":"../escape","size":1}),
        json!({"type":"file","path":"a","size":-1}),
        json!({"type":"symlink","path":"a","size":1}),
    ] {
        assert!(serde_json::from_value::<TreeEntry>(value).is_err());
    }
}

#[test]
fn link_parser_preserves_encoded_next_with_multiple_relations_and_rejects_ambiguity() {
    let current = Url::parse("https://example.test/api/models/org/repo/tree/commit").unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        LINK,
        "<?cursor=a,b%23>; rel=\"next last\", </previous>; rel=\"prev\""
            .parse()
            .unwrap(),
    );
    assert_eq!(
        next_link(&headers, &current).unwrap().unwrap().query(),
        Some("cursor=a,b%23")
    );
    for value in [
        "<https://other.test/page>; rel=next",
        "<http://example.test/page>; rel=next",
    ] {
        headers.insert(LINK, value.parse().unwrap());
        assert!(matches!(
            next_link(&headers, &current),
            Err(RepositoryError::UnsupportedEndpoint)
        ));
    }
    headers.insert(LINK, "</one>; rel=next, </two>; rel=next".parse().unwrap());
    assert!(matches!(
        next_link(&headers, &current),
        Err(RepositoryError::InvalidResponse)
    ));
}
