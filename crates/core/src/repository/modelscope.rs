use super::RepositoryClient;
use crate::repository::*;
use serde::Deserialize;
use std::collections::{HashSet, VecDeque};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use url::Url;

#[cfg(test)]
#[allow(dead_code)] // Shared fixture also exposes HF-only convenience methods.
#[path = "../../tests/repository/fixture.rs"]
mod fixture;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Response {
    code: u16,
    success: bool,
    data: Option<Files>,
}
#[derive(Deserialize)]
struct Files {
    #[serde(rename = "Files")]
    files: Vec<File>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct File {
    path: RepoPath,
    #[serde(rename = "Type")]
    kind: String,
    size: Option<Bytes>,
    sha256: Option<String>,
    committed_date: Option<u64>,
}

fn files_url(source: &RepositorySpec, root: &str) -> Result<Url, RepositoryError> {
    let mut url = source.request.endpoint.url().clone();
    url.path_segments_mut()
        .map_err(|_| RepositoryError::UnsupportedEndpoint)?
        .clear()
        .extend(["api", "v1", "models"])
        .extend(source.request.repo_id.as_str().split('/'))
        .extend(["repo", "files"]);
    url.query_pairs_mut()
        .append_pair("Revision", source.resolved_commit.as_str())
        .append_pair("Recursive", "False")
        .append_pair("Root", root);
    Ok(url)
}

// Use Git's advertised refs to pin the repository, rather than a file's last-modified commit.
fn advertised_commit(mut bytes: &[u8], revision: &Revision) -> Result<CommitId, RepositoryError> {
    let branch = format!("refs/heads/{}", revision.as_str());
    let tag = format!("refs/tags/{}", revision.as_str());
    let peeled = format!("{tag}^{{}}");
    let mut branch_commit = None;
    let mut tag_commit = None;
    let mut peeled_commit = None;
    while !bytes.is_empty() {
        let length = bytes
            .get(..4)
            .and_then(|b| std::str::from_utf8(b).ok())
            .and_then(|s| usize::from_str_radix(s, 16).ok())
            .ok_or(RepositoryError::InvalidResponse)?;
        bytes = &bytes[4..];
        if length == 0 {
            continue;
        }
        let length = length
            .checked_sub(4)
            .ok_or(RepositoryError::InvalidResponse)?;
        let packet = bytes
            .get(..length)
            .ok_or(RepositoryError::InvalidResponse)?;
        bytes = &bytes[length..];
        let text = std::str::from_utf8(packet).map_err(|_| RepositoryError::InvalidResponse)?;
        let text = text.split('\0').next().unwrap_or("").trim_end_matches('\n');
        if let Some((sha, name)) = text.split_once(' ') {
            if name == branch {
                branch_commit = Some(CommitId::parse(sha)?);
            }
            if name == tag {
                tag_commit = Some(CommitId::parse(sha)?);
            }
            if name == peeled {
                peeled_commit = Some(CommitId::parse(sha)?);
            }
        }
    }
    branch_commit
        .or(peeled_commit)
        .or(tag_commit)
        .ok_or(RepositoryError::Input("Unknown ModelScope revision".into()))
}

impl RepositoryClient {
    pub(super) async fn modelscope_resolve(
        &self,
        request: &RepositoryRequest,
        cancel: &CancellationToken,
    ) -> Result<RepositorySpec, RepositoryError> {
        if request.kind != RepoKind::Model || !request.repo_id.as_str().contains('/') {
            return Err(RepositoryError::UnsupportedEndpoint);
        }
        let resolved_commit =
            if let Ok(commit) = CommitId::parse(request.requested_revision.as_str()) {
                commit
            } else {
                let mut url = request.endpoint.url().clone();
                let (owner, name) = request
                    .repo_id
                    .as_str()
                    .split_once('/')
                    .ok_or(RepositoryError::InvalidResponse)?;
                url.path_segments_mut()
                    .map_err(|_| RepositoryError::UnsupportedEndpoint)?
                    .clear()
                    .extend([owner, &format!("{name}.git"), "info", "refs"]);
                url.query_pairs_mut()
                    .append_pair("service", "git-upload-pack");
                let (_, wire) = self.api_get(url, cancel).await?;
                advertised_commit(&wire.body, &request.requested_revision)?
            };
        Ok(RepositorySpec {
            request: request.clone(),
            resolved_commit,
        })
    }

    async fn modelscope_files(
        &self,
        source: &RepositorySpec,
        root: &str,
        cancel: &CancellationToken,
    ) -> Result<Vec<File>, RepositoryError> {
        let response: Response = self.json(files_url(source, root)?, cancel).await?;
        if !response.success || response.code != 200 {
            return Err(if (400..=599).contains(&response.code) {
                RepositoryError::Http {
                    endpoint: self.endpoint.clone(),
                    status: response.code,
                }
            } else {
                RepositoryError::InvalidResponse
            });
        }
        let files = response.data.ok_or(RepositoryError::InvalidResponse)?.files;
        // The API caps each directory at 3000 entries and provides no pagination.
        if files.len() >= 3000 {
            return Err(RepositoryError::InvalidResponse);
        }
        let prefix = if root.is_empty() {
            String::new()
        } else {
            format!("{root}/")
        };
        if files.iter().any(|f| {
            !f.path
                .as_str()
                .strip_prefix(&prefix)
                .is_some_and(|p| !p.is_empty() && !p.contains('/'))
        }) {
            return Err(RepositoryError::InvalidResponse);
        }
        Ok(files)
    }

    pub(super) async fn modelscope_catalog(
        &self,
        request: &CatalogRequest,
        output: mpsc::Sender<CatalogPage>,
        cancel: &CancellationToken,
    ) -> Result<(), RepositoryError> {
        let mut pending = VecDeque::from([String::new()]);
        let mut seen = HashSet::new();
        while let Some(root) = pending.pop_front() {
            let mut entries = Vec::new();
            for file in self
                .modelscope_files(&request.source, &root, cancel)
                .await?
            {
                if !seen.insert(file.path.clone()) {
                    return Err(RepositoryError::InvalidResponse);
                }
                match file.kind.as_str() {
                    "tree" => pending.push_back(file.path.as_str().into()),
                    "blob" => {
                        let content_hash = file
                            .sha256
                            .filter(|s| !s.is_empty())
                            .map(|hex| {
                                serde_json::from_value(
                                    serde_json::json!({"algorithm":"sha256","hex":hex}),
                                )
                                .map_err(|_| RepositoryError::InvalidResponse)
                            })
                            .transpose()?;
                        entries.push(CatalogEntry {
                            path: file.path,
                            size: file.size.ok_or(RepositoryError::InvalidResponse)?,
                            content_hash,
                        });
                    }
                    _ => return Err(RepositoryError::InvalidResponse),
                }
            }
            let page = CatalogPage {
                catalog: request.id,
                entries,
                complete: pending.is_empty(),
            };
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(RepositoryError::Cancelled),
                result = output.send(page) => result.map_err(|_| RepositoryError::Cancelled)?,
            }
        }
        Ok(())
    }

    pub(super) async fn modelscope_modified_ms(
        &self,
        source: &RepositorySpec,
        path: &RepoPath,
        cancel: &CancellationToken,
    ) -> Result<Option<u64>, RepositoryError> {
        let root = path.as_str().rsplit_once('/').map_or("", |(root, _)| root);
        Ok(self
            .modelscope_files(source, root, cancel)
            .await?
            .into_iter()
            .find(|f| f.path == *path)
            .and_then(|f| f.committed_date)
            .and_then(|seconds| seconds.checked_mul(1000)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        aria2::{Aria2, Aria2Launch, DownloadJob, EngineState},
        storage::{self, DownloadOptions, ResumeAction, TaskStore},
    };

    fn client(fixture: &fixture::Fixture) -> (RepositoryClient, RepositoryRequest) {
        let port = Url::parse(fixture.origin()).unwrap().port().unwrap();
        let endpoint = Endpoint::parse(&format!("http://modelscope.ai:{port}"), true).unwrap();
        let request = RepositoryRequest {
            endpoint: endpoint.clone(),
            kind: RepoKind::Model,
            repo_id: RepoId::parse("org/model").unwrap(),
            requested_revision: Revision::parse("master").unwrap(),
        };
        let mut client = RepositoryClient::new(
            &endpoint,
            NetworkAccess {
                token: Some(Token::parse("fake-ms-token").unwrap()),
                proxy: None,
            },
        )
        .unwrap();
        client.http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .resolve("modelscope.ai", ([127, 0, 0, 1], port).into())
            .build()
            .unwrap();
        (client, request)
    }

    #[tokio::test]
    async fn modelscope_pins_commit_walks_directories_and_downloads_with_real_aria2() {
        let fixture = fixture::Fixture::start();
        let (client, request) = client(&fixture);
        let cancel = CancellationToken::new();
        let source = client.resolve(&request, &cancel).await.unwrap();
        assert_eq!(source.resolved_commit.as_str(), "a".repeat(40));
        let catalog = CatalogRequest {
            id: CatalogId(1),
            source: source.clone(),
        };
        let (tx, mut rx) = mpsc::channel(4);
        client.catalog(&catalog, tx, &cancel).await.unwrap();
        let mut entries = Vec::new();
        let mut complete = false;
        while let Some(page) = rx.recv().await {
            complete = page.complete;
            entries.extend(page.entries);
        }
        assert!(complete);
        assert_eq!(entries.len(), 4);
        let entry = entries
            .into_iter()
            .find(|e| e.path.as_str() == "special/space # 中文.bin")
            .unwrap();
        assert_eq!(
            entry.content_hash.as_ref().unwrap().algorithm,
            HashAlgorithm::Sha256
        );
        assert_eq!(
            client
                .file_modified_ms(&source, &entry.path, &cancel)
                .await
                .unwrap(),
            Some(1793491200000)
        );
        let url = client.download_url(&source, &entry, &cancel).await.unwrap();
        let directory = tempfile::tempdir().unwrap();
        let record = storage::new_record(
            source,
            vec![entry.clone()],
            DownloadOptions::new(1, 1).unwrap(),
        )
        .unwrap();
        let mut store = TaskStore::create(directory.path(), &record).await.unwrap();
        let mut engine = Aria2::launch(Aria2Launch {
            executable: "aria2c".into(),
        })
        .await
        .unwrap();
        engine.configure(&record.options).await.unwrap();
        let file = store.prepare(&entry, ResumeAction::Create).await.unwrap();
        let gid = engine
            .add(DownloadJob {
                url,
                file,
                proxy: None,
            })
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let status = engine.statuses(&[gid]).await.unwrap().remove(0);
                assert_ne!(status.state, EngineState::Failed, "{:?}", status.failure);
                if status.state == EngineState::Complete {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        engine.shutdown().await.unwrap();
        let bytes = std::fs::read(directory.path().join(entry.path.as_str())).unwrap();
        use sha2::{Digest, Sha256};
        assert_eq!(
            format!("{:x}", Sha256::digest(bytes)),
            entry.content_hash.unwrap().hex
        );
        let state = fixture.state().await;
        let events = state["events"].as_array().unwrap();
        assert!(events.iter().any(|e| {
            e["path"]
                .as_str()
                .is_some_and(|p| p.starts_with("/api/v1/"))
                && e["cookie_present"] == true
        }));
        assert!(
            events
                .iter()
                .filter(|e| e["role"] == "capture")
                .all(|e| e["cookie_present"] == false && e["auth_present"] == false)
        );
        fixture.stop().await;
    }

    #[tokio::test]
    async fn modelscope_api_error_inside_http_200_is_not_accepted_as_a_catalog() {
        let fixture = fixture::Fixture::start();
        let (client, request) = client(&fixture);
        let cancel = CancellationToken::new();
        let source = client.resolve(&request, &cancel).await.unwrap();
        fixture
            .http
            .post(format!("{}/__control", fixture.origin()))
            .json(&serde_json::json!({"api_status":429}))
            .send()
            .await
            .unwrap();
        let (tx, _rx) = mpsc::channel(2);
        let error = client
            .catalog(
                &CatalogRequest {
                    id: CatalogId(1),
                    source,
                },
                tx,
                &cancel,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, RepositoryError::Http { status: 429, .. }));
        assert!(!error.to_string().contains("fake-secret"));
        let response = fixture
            .http
            .post(format!("{}/__control", fixture.origin()))
            .json(&serde_json::json!({"api_status":200,"tree_mode":"unsafe"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let (tx, _rx) = mpsc::channel(2);
        let source = client.resolve(&request, &cancel).await.unwrap();
        assert!(matches!(
            client
                .catalog(
                    &CatalogRequest {
                        id: CatalogId(2),
                        source
                    },
                    tx,
                    &cancel
                )
                .await,
            Err(RepositoryError::InvalidResponse)
        ));
        fixture.stop().await;
    }

    #[test]
    fn git_refs_reject_truncation_and_prefer_peeled_tags() {
        fn packet(text: &str) -> String {
            format!("{:04x}{text}", text.len() + 4)
        }
        let refs = format!(
            "{}{}0000",
            packet(&format!("{} refs/tags/v1\n", "a".repeat(40))),
            packet(&format!("{} refs/tags/v1^{{}}\n", "b".repeat(40)))
        );
        assert_eq!(
            advertised_commit(refs.as_bytes(), &Revision::parse("v1").unwrap())
                .unwrap()
                .as_str(),
            "b".repeat(40)
        );
        for bytes in [b"0010bad".as_slice(), b"nope", b"0001", b"<html>"] {
            assert!(advertised_commit(bytes, &Revision::parse("master").unwrap()).is_err());
        }
    }
}
