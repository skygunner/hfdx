use super::*;
use url::Url;

pub(crate) fn metadata_url(request: &RepositoryRequest) -> Result<Url, RepositoryError> {
    let mut url = request.endpoint.url().clone();
    let kind = match request.kind {
        RepoKind::Model => "models",
        RepoKind::Dataset => "datasets",
    };
    url.path_segments_mut()
        .map_err(|_| RepositoryError::UnsupportedEndpoint)?
        .pop_if_empty()
        .push("api")
        .push(kind)
        .extend(request.repo_id.as_str().split('/'))
        .push("revision")
        .push(request.requested_revision.as_str());
    Ok(url)
}
pub(crate) fn resolve_url(
    source: &RepositorySpec,
    entry: &CatalogEntry,
) -> Result<Url, RepositoryError> {
    let mut url = source.request.endpoint.url().clone();
    if source.request.endpoint.is_modelscope() {
        url.path_segments_mut()
            .map_err(|_| RepositoryError::UnsupportedEndpoint)?
            .clear()
            .extend(["api", "v1", "models"])
            .extend(source.request.repo_id.as_str().split('/'))
            .push("repo");
        url.query_pairs_mut()
            .append_pair("Revision", source.resolved_commit.as_str())
            .append_pair("FilePath", entry.path.as_str());
        return Ok(url);
    }
    let mut segments = url
        .path_segments_mut()
        .map_err(|_| RepositoryError::UnsupportedEndpoint)?;
    segments.pop_if_empty();
    match source.request.kind {
        RepoKind::Dataset => {
            segments.push("datasets");
        }
        RepoKind::Model => {}
    }
    segments
        .extend(source.request.repo_id.as_str().split('/'))
        .push("resolve")
        .push(source.resolved_commit.as_str())
        .extend(entry.path.as_str().split('/'));
    drop(segments);
    Ok(url)
}
