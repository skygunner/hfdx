use super::RepositoryClient;
use crate::{
    logging::{EventKind, HttpLogEvent, LogOutcome},
    repository::*,
};
use reqwest::header::{HeaderMap, LINK};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use url::Url;

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;

#[derive(Deserialize)]
#[serde(tag = "type")]
enum TreeEntry {
    #[serde(rename = "directory")]
    Directory,
    #[serde(rename = "file")]
    File {
        path: RepoPath,
        size: Bytes,
        oid: Option<String>,
        lfs: Option<Lfs>,
    },
}
#[derive(Deserialize)]
struct Lfs {
    oid: String,
    size: Bytes,
}
impl TreeEntry {
    fn into_entry(self) -> Result<Option<CatalogEntry>, RepositoryError> {
        match self {
            Self::Directory => Ok(None),
            Self::File {
                path,
                size,
                oid,
                lfs,
            } => {
                let hash = match lfs {
                    Some(lfs) => {
                        if lfs.size != size {
                            return Err(RepositoryError::InvalidResponse);
                        }
                        Some((HashAlgorithm::Sha256, lfs.oid, 64))
                    }
                    None => oid.map(|oid| (HashAlgorithm::GitSha1, oid, 40)),
                };
                let content_hash = match hash {
                    Some((algorithm, hex, length)) => {
                        if hex.len() != length || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                            return Err(RepositoryError::InvalidResponse);
                        }
                        Some(ContentHash {
                            algorithm,
                            hex: hex.to_ascii_lowercase(),
                        })
                    }
                    None => None,
                };
                Ok(Some(CatalogEntry {
                    path,
                    size,
                    content_hash,
                }))
            }
        }
    }
}
impl RepositoryClient {
    pub async fn catalog(
        &self,
        request: &CatalogRequest,
        output: mpsc::Sender<CatalogPage>,
        cancel: &CancellationToken,
    ) -> Result<(), RepositoryError> {
        self.check_source(&request.source.request.endpoint)?;
        if request.source.request.endpoint.is_modelscope() {
            return self.modelscope_catalog(request, output, cancel).await;
        }
        let mut url = tree_url(&request.source)?;
        let mut visited = HashSet::new();
        let mut seen = HashMap::<RepoPath, CatalogEntry>::new();
        let mut page_number = 0u64;
        loop {
            if !visited.insert(url.clone()) {
                return Err(RepositoryError::InvalidResponse);
            }
            page_number = page_number.saturating_add(1);
            let started = Instant::now();
            let response = self.api_get(url.clone(), cancel).await;
            if let Some(logger) = &self.logger {
                let outcome = match &response {
                    Ok(_) => LogOutcome::Succeeded,
                    Err(RepositoryError::Cancelled) => LogOutcome::Cancelled,
                    Err(RepositoryError::Http { status: 429, .. }) => LogOutcome::RateLimited,
                    Err(_) => LogOutcome::Network,
                };
                let _result = logger.record_http(HttpLogEvent {
                    event: EventKind::Pagination,
                    outcome,
                    elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    page: Some(page_number),
                    retry: None,
                });
            }
            let (response_url, wire) = response?;
            let next = next_link(&wire.headers, &response_url)?;
            let raw: Vec<TreeEntry> =
                serde_json::from_slice(&wire.body).map_err(|_| RepositoryError::InvalidResponse)?;
            let mut entries = Vec::with_capacity(raw.len());
            for entry in raw {
                if let Some(entry) = entry.into_entry()? {
                    match seen.get(&entry.path) {
                        Some(previous) if previous != &entry => {
                            return Err(RepositoryError::InvalidResponse);
                        }
                        Some(_) => {}
                        None => {
                            seen.insert(entry.path.clone(), entry.clone());
                            entries.push(entry);
                        }
                    }
                }
            }
            let page = CatalogPage {
                catalog: request.id,
                entries,
                complete: next.is_none(),
            };
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(RepositoryError::Cancelled),
                result = output.send(page) => result.map_err(|_| RepositoryError::Cancelled)?,
            }
            match next {
                Some(next) => url = next,
                None => return Ok(()),
            }
        }
    }
}
fn tree_url(source: &RepositorySpec) -> Result<Url, RepositoryError> {
    let request = &source.request;
    let mut url = request.endpoint.url().clone();
    let kind = match request.kind {
        RepoKind::Model => "models",
        RepoKind::Dataset => "datasets",
    };
    url.path_segments_mut()
        .map_err(|_| RepositoryError::UnsupportedEndpoint)?
        .clear()
        .push("api")
        .push(kind)
        .extend(request.repo_id.as_str().split('/'))
        .push("tree")
        .push(source.resolved_commit.as_str());
    url.query_pairs_mut().append_pair("recursive", "true");
    Ok(url)
}
fn next_link(headers: &HeaderMap, current: &Url) -> Result<Option<Url>, RepositoryError> {
    let mut next = None;
    for header in headers.get_all(LINK) {
        let value = header
            .to_str()
            .map_err(|_| RepositoryError::InvalidResponse)?;
        let mut angle = false;
        let mut quoted = false;
        for item in value.split(|c| {
            match c {
                '<' if !quoted => angle = true,
                '>' if !quoted => angle = false,
                '"' if !angle => quoted = !quoted,
                _ => {}
            }
            c == ',' && !angle && !quoted
        }) {
            let (target, parameters) = item
                .trim()
                .strip_prefix('<')
                .and_then(|s| s.split_once('>'))
                .ok_or(RepositoryError::InvalidResponse)?;
            let is_next = parameters
                .split(';')
                .filter_map(|p| p.trim().split_once('='))
                .any(|(name, value)| {
                    name.eq_ignore_ascii_case("rel")
                        && value
                            .trim_matches('"')
                            .split_ascii_whitespace()
                            .any(|v| v == "next")
                });
            if is_next {
                let url = current
                    .join(target)
                    .map_err(|_| RepositoryError::InvalidResponse)?;
                if url.origin() != current.origin()
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.fragment().is_some()
                {
                    return Err(RepositoryError::UnsupportedEndpoint);
                }
                if next.replace(url).is_some() {
                    return Err(RepositoryError::InvalidResponse);
                }
            }
        }
        if angle || quoted {
            return Err(RepositoryError::InvalidResponse);
        }
    }
    Ok(next)
}

impl RepositoryClient {
    pub(crate) async fn file_modified_ms(
        &self,
        source: &RepositorySpec,
        path: &RepoPath,
        cancel: &CancellationToken,
    ) -> Result<Option<u64>, RepositoryError> {
        self.check_source(&source.request.endpoint)?;
        if source.request.endpoint.is_modelscope() {
            return self.modelscope_modified_ms(source, path, cancel).await;
        }
        let mut url = tree_url(source)?;
        if let Some((parent, _)) = path.as_str().rsplit_once('/') {
            url.path_segments_mut()
                .map_err(|_| RepositoryError::UnsupportedEndpoint)?
                .extend(parent.split('/'));
        }
        url.set_query(None);
        url.query_pairs_mut()
            .append_pair("recursive", "false")
            .append_pair("expand", "true");
        let mut visited = HashSet::new();
        loop {
            if !visited.insert(url.clone()) {
                return Err(RepositoryError::InvalidResponse);
            }
            let (response, wire) = self.api_get(url, cancel).await?;
            let entries: Vec<serde_json::Value> =
                serde_json::from_slice(&wire.body).map_err(|_| RepositoryError::InvalidResponse)?;
            if let Some(entry) = entries
                .iter()
                .find(|e| e["path"].as_str() == Some(path.as_str()))
            {
                return Ok(entry["lastCommit"]["date"].as_str().and_then(commit_millis));
            }
            match next_link(&wire.headers, &response)? {
                Some(next) => url = next,
                None => return Ok(None),
            }
        }
    }
}
fn commit_millis(text: &str) -> Option<u64> {
    let (date, clock) = text.strip_suffix('Z')?.split_once('T')?;
    let fields: Vec<i64> = date
        .split('-')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    let times: Vec<&str> = clock.split(':').collect();
    if fields.len() != 3 || times.len() != 3 {
        return None;
    }
    let (year, month, day) = (fields[0], fields[1], fields[2]);
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1970..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || day < 1
        || day > days[(month - 1) as usize]
    {
        return None;
    }
    let hour: i64 = times[0].parse().ok()?;
    let minute: i64 = times[1].parse().ok()?;
    let (seconds, fraction) = times[2].split_once('.').unwrap_or((times[2], ""));
    let second: i64 = seconds.parse().ok()?;
    if !(0..24).contains(&hour)
        || !(0..60).contains(&minute)
        || !(0..60).contains(&second)
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let adjusted = year - i64::from(month <= 2);
    let era = adjusted.div_euclid(400);
    let yoe = adjusted - era * 400;
    let shifted = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * shifted + 2) / 5 + day - 1;
    let elapsed = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468;
    let fraction = format!("{fraction:0<3}");
    let millis: u64 = fraction[..3].parse().ok()?;
    u64::try_from(elapsed * 86400 + hour * 3600 + minute * 60 + second)
        .ok()?
        .checked_mul(1000)?
        .checked_add(millis)
}
#[cfg(test)]
#[test]
fn file_commit_time_uses_utc_and_rejects_invalid_calendar_dates() {
    assert_eq!(commit_millis("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(
        commit_millis("2024-02-29T12:30:01.123456Z"),
        Some(1709209801123)
    );
    assert_eq!(commit_millis("2023-02-29T12:00:00Z"), None);
    assert_eq!(commit_millis("2024-01-01T24:00:00Z"), None);
    assert_eq!(commit_millis("2024-01-01T00:00:00+10:00"), None);
}
