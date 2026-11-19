use super::RepositoryClient;
use crate::{
    logging::{EventKind, HttpLogEvent, LogOutcome},
    repository::RepositoryError,
};
use reqwest::{
    Method, StatusCode,
    header::{HeaderMap, RANGE, RETRY_AFTER},
};
use std::time::{Duration, Instant, SystemTime};
use tokio_util::sync::CancellationToken;
use url::Url;

const MAX_METADATA_BYTES: usize = 16 * 1024 * 1024;

pub(super) struct RequestPlan {
    pub url: Url,
    pub method: Method,
    pub authenticated: bool,
    pub range: bool,
    pub body: bool,
}
pub(super) struct WireResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}
impl RepositoryClient {
    pub(super) async fn exchange(
        &self,
        plan: RequestPlan,
        cancel: &CancellationToken,
    ) -> Result<WireResponse, RepositoryError> {
        for attempt in 0..=3u32 {
            let started = Instant::now();
            let mut request = self.http.request(plan.method.clone(), plan.url.clone());
            if self.endpoint.is_modelscope() && plan.url.path().ends_with(".git/info/refs") {
                request = request.header(reqwest::header::USER_AGENT, "git/hfdx");
            }
            if plan.authenticated
                && plan.url.origin() == self.endpoint.url().origin()
                && let Some(token) = &self.token
            {
                request = if self.endpoint.is_modelscope() && plan.url.path().starts_with("/api/") {
                    request.header(
                        reqwest::header::COOKIE,
                        format!("m_session_id={}", token.expose()),
                    )
                } else {
                    request.bearer_auth(token.expose())
                };
            }
            if plan.range {
                request = request.header(RANGE, "bytes=0-0");
            }
            let result = tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(RepositoryError::Cancelled),
                result = async {
                    let mut response = request.send().await.map_err(|_| RepositoryError::Network)?;
                    let status = response.status();
                    let headers = response.headers().clone();
                    let body = if plan.body && status.is_success() {
                        if response.content_length().is_some_and(|size| size > MAX_METADATA_BYTES as u64) {
                            return Err(RepositoryError::InvalidResponse);
                        }
                        let mut body = Vec::new();
                        while let Some(chunk) = response.chunk().await.map_err(|_| RepositoryError::Network)? {
                            if body.len().saturating_add(chunk.len()) > MAX_METADATA_BYTES {
                                return Err(RepositoryError::InvalidResponse);
                            }
                            body.extend_from_slice(&chunk);
                        }
                        body
                    } else { Vec::new() };
                    Ok::<_, RepositoryError>(WireResponse { status, headers, body })
                } => result,
            };
            let (delay, outcome) = match result {
                Ok(wire) => {
                    if !(wire.status.is_server_error()
                        || wire.status == StatusCode::TOO_MANY_REQUESTS)
                        || attempt == 3
                    {
                        return Ok(wire);
                    }
                    (
                        retry_delay(&wire.headers, attempt),
                        if wire.status == StatusCode::TOO_MANY_REQUESTS {
                            LogOutcome::RateLimited
                        } else {
                            LogOutcome::Network
                        },
                    )
                }
                Err(error) => {
                    if attempt == 3 || !matches!(error, RepositoryError::Network) {
                        return Err(error);
                    }
                    (
                        Duration::from_millis(250 * 2u64.pow(attempt)),
                        LogOutcome::Network,
                    )
                }
            };
            if let Some(logger) = &self.logger {
                let _result = logger.record_http(HttpLogEvent {
                    event: EventKind::Retry,
                    outcome,
                    elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    page: None,
                    retry: Some(attempt + 1),
                });
            }
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(RepositoryError::Cancelled),
                () = tokio::time::sleep(delay) => {}
            }
        }
        Err(RepositoryError::Network)
    }
}
fn retry_delay(headers: &HeaderMap, attempt: u32) -> Duration {
    if let Some(value) = headers.get(RETRY_AFTER).and_then(|h| h.to_str().ok()) {
        if let Ok(seconds) = value.parse::<u64>() {
            return Duration::from_secs(seconds);
        }
        if let Ok(date) = httpdate::parse_http_date(value) {
            return date
                .duration_since(SystemTime::now())
                .unwrap_or(Duration::ZERO);
        }
    }
    Duration::from_millis(250 * 2u64.pow(attempt))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn oversized_metadata_is_rejected_with_and_without_content_length() {
        use crate::repository::{
            Endpoint, NetworkAccess, RepoId, RepoKind, RepositoryRequest, Revision,
        };
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for chunked in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 4096];
                assert!(stream.read(&mut request).await.unwrap() > 0);
                let mut body = format!("{{\"sha\":\"{}\"}}", "a".repeat(40)).into_bytes();
                body.resize(16 * 1024 * 1024 + 1, b' ');
                let headers = if chunked {
                    "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
                        .to_owned()
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                };
                stream.write_all(headers.as_bytes()).await.unwrap();
                if chunked {
                    stream
                        .write_all(format!("{:x}\r\n", body.len()).as_bytes())
                        .await
                        .unwrap();
                }
                // Rejection may close the socket before the oversized body is written.
                let _ = stream.write_all(&body).await;
                if chunked {
                    let _ = stream.write_all(b"\r\n0\r\n\r\n").await;
                }
            });
            let endpoint = Endpoint::parse(&format!("http://{address}"), true).unwrap();
            let client = RepositoryClient::new(&endpoint, NetworkAccess::default()).unwrap();
            let request = RepositoryRequest {
                endpoint,
                kind: RepoKind::Model,
                repo_id: RepoId::parse("org/model").unwrap(),
                requested_revision: Revision::parse("main").unwrap(),
            };
            let result = client.resolve(&request, &CancellationToken::new()).await;
            assert!(
                matches!(result, Err(RepositoryError::InvalidResponse)),
                "oversized metadata accepted; chunked={chunked}"
            );
            server.await.unwrap();
        }
    }
    #[test]
    fn retry_after_accepts_seconds_and_http_date() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, "17".parse().unwrap());
        assert_eq!(retry_delay(&headers, 0), Duration::from_secs(17));
        headers.insert(
            RETRY_AFTER,
            "Thu, 01 Jan 1970 00:00:00 GMT".parse().unwrap(),
        );
        assert_eq!(retry_delay(&headers, 0), Duration::ZERO);
        headers.insert(RETRY_AFTER, "invalid".parse().unwrap());
        assert_eq!(retry_delay(&headers, 2), Duration::from_secs(1));
    }
}
