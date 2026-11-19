use super::{RepositoryClient, RequestPlan, transport::WireResponse};
use crate::repository::*;
use reqwest::{Method, StatusCode, header::LOCATION};
use tokio_util::sync::CancellationToken;
use url::Url;

impl RepositoryClient {
    pub async fn download_url(
        &self,
        source: &RepositorySpec,
        entry: &CatalogEntry,
        cancel: &CancellationToken,
    ) -> Result<DownloadUrl, RepositoryError> {
        self.check_source(&source.request.endpoint)?;
        let initial = resolve_url(source, entry)?;
        let (url, response) = self.follow(initial, true, cancel).await?;
        if !response.status.is_success() {
            return Err(self.status_error(response.status));
        }
        let (url, anonymous) = self.follow(url, false, cancel).await?;
        if matches!(
            anonymous.status,
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ) {
            return Err(RepositoryError::UnsupportedEndpoint);
        }
        if !anonymous.status.is_success() {
            return Err(self.status_error(anonymous.status));
        }
        DownloadUrl::from_url(url)
    }
    async fn follow(
        &self,
        mut url: Url,
        mut authenticated: bool,
        cancel: &CancellationToken,
    ) -> Result<(Url, WireResponse), RepositoryError> {
        let mut method = Method::HEAD;
        for _ in 0..10 {
            let response = self
                .exchange(
                    RequestPlan {
                        url: url.clone(),
                        range: method == Method::GET,
                        method: method.clone(),
                        authenticated,
                        body: false,
                    },
                    cancel,
                )
                .await?;
            if matches!(
                response.status,
                StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED
            ) && method == Method::HEAD
            {
                method = Method::GET;
                continue;
            }
            if matches!(
                response.status,
                StatusCode::MOVED_PERMANENTLY
                    | StatusCode::FOUND
                    | StatusCode::SEE_OTHER
                    | StatusCode::TEMPORARY_REDIRECT
                    | StatusCode::PERMANENT_REDIRECT
            ) {
                let location = response
                    .headers
                    .get(LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or(RepositoryError::InvalidResponse)?;
                let target = url
                    .join(location)
                    .map_err(|_| RepositoryError::InvalidResponse)?;
                DownloadUrl::from_url(target.clone())?;
                if url.scheme() == "https" && target.scheme() != "https" {
                    return Err(RepositoryError::UnsupportedEndpoint);
                }
                authenticated &= url.origin() == target.origin();
                url = target;
            } else {
                return Ok((url, response));
            }
        }
        Err(RepositoryError::UnsupportedEndpoint)
    }
}
