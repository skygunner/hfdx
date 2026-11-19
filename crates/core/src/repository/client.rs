use super::*;
use crate::logging::SessionLogger;
use reqwest::{Client, Method, StatusCode, header::LOCATION};
use serde::Deserialize;
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

#[path = "catalog.rs"]
mod catalog;
#[path = "download.rs"]
mod download;
#[path = "modelscope.rs"]
mod modelscope;
#[path = "transport.rs"]
mod transport;
use transport::RequestPlan;

/// HTTP operations bound to one explicit source origin and its credentials.
pub struct RepositoryClient {
    endpoint: Endpoint,
    http: Client,
    token: Option<Token>,
    logger: Option<Arc<SessionLogger>>,
}
impl RepositoryClient {
    pub fn new(endpoint: &Endpoint, access: NetworkAccess) -> Result<Self, RepositoryError> {
        let mut builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(4);
        if let Some(proxy) = access.proxy {
            builder = builder.proxy(
                reqwest::Proxy::all(proxy.authenticated_url().as_str())
                    .map_err(|_| input_error())?,
            );
        }
        Ok(Self {
            endpoint: endpoint.clone(),
            http: builder.build().map_err(|_| RepositoryError::Network)?,
            token: access.token,
            logger: None,
        })
    }
    pub(crate) fn set_logger(&mut self, logger: Option<Arc<SessionLogger>>) {
        self.logger = logger;
    }
    pub async fn resolve(
        &self,
        request: &RepositoryRequest,
        cancel: &CancellationToken,
    ) -> Result<RepositorySpec, RepositoryError> {
        self.check_source(&request.endpoint)?;
        if request.endpoint.is_modelscope() {
            return self.modelscope_resolve(request, cancel).await;
        }
        #[derive(Deserialize)]
        struct Metadata {
            sha: CommitId,
        }
        let metadata: Metadata = self.json(metadata_url(request)?, cancel).await?;
        Ok(RepositorySpec {
            request: request.clone(),
            resolved_commit: metadata.sha,
        })
    }
    fn check_source(&self, endpoint: &Endpoint) -> Result<(), RepositoryError> {
        if endpoint.url().origin() != self.endpoint.url().origin() {
            return Err(RepositoryError::UnsupportedEndpoint);
        }
        Ok(())
    }
    async fn json<T: serde::de::DeserializeOwned>(
        &self,
        url: url::Url,
        cancel: &CancellationToken,
    ) -> Result<T, RepositoryError> {
        let (_, wire) = self.api_get(url, cancel).await?;
        serde_json::from_slice(&wire.body).map_err(|_| RepositoryError::InvalidResponse)
    }
    async fn api_get(
        &self,
        mut url: url::Url,
        cancel: &CancellationToken,
    ) -> Result<(url::Url, transport::WireResponse), RepositoryError> {
        for hop in 0..=5 {
            let wire = self
                .exchange(
                    RequestPlan {
                        url: url.clone(),
                        method: Method::GET,
                        authenticated: true,
                        range: false,
                        body: true,
                    },
                    cancel,
                )
                .await?;
            if wire.status.is_success() {
                return Ok((url, wire));
            }
            if !matches!(
                wire.status,
                StatusCode::MOVED_PERMANENTLY
                    | StatusCode::FOUND
                    | StatusCode::SEE_OTHER
                    | StatusCode::TEMPORARY_REDIRECT
                    | StatusCode::PERMANENT_REDIRECT
            ) {
                return Err(self.status_error(wire.status));
            }
            if hop == 5 {
                return Err(RepositoryError::UnsupportedEndpoint);
            }
            let location = wire
                .headers
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or(RepositoryError::InvalidResponse)?;
            let target = url
                .join(location)
                .map_err(|_| RepositoryError::InvalidResponse)?;
            if target.origin() != self.endpoint.url().origin()
                || (url.scheme() == "https" && target.scheme() != "https")
                || !target.username().is_empty()
                || target.password().is_some()
                || target.fragment().is_some()
            {
                return Err(RepositoryError::UnsupportedEndpoint);
            }
            url = target;
        }
        Err(RepositoryError::UnsupportedEndpoint)
    }
    fn status_error(&self, status: reqwest::StatusCode) -> RepositoryError {
        if status.is_redirection() {
            return RepositoryError::UnsupportedEndpoint;
        }
        RepositoryError::Http {
            endpoint: self.endpoint.clone(),
            status: status.as_u16(),
        }
    }
}
