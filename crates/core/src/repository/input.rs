use super::*;
use url::Url;

pub(super) fn input_error() -> RepositoryError {
    RepositoryError::Input("Invalid repository value".into())
}
pub(super) fn clean(input: &str) -> bool {
    !input.is_empty() && !input.chars().any(char::is_control)
}
pub(super) fn validate_repo_id(input: &str) -> Result<String, RepositoryError> {
    let parts: Vec<_> = input.split('/').collect();
    if !(1..=2).contains(&parts.len())
        || parts.iter().any(|part| {
            part.is_empty()
                || *part == "."
                || *part == ".."
                || part.starts_with('.')
                || part.ends_with('.')
                || part.contains("..")
                || !part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        })
    {
        return Err(input_error());
    }
    Ok(input.into())
}
pub(super) fn validate_revision(input: &str) -> Result<String, RepositoryError> {
    if !clean(input)
        || input.chars().any(char::is_whitespace)
        || input.contains(['\\', '?', '#', '~', '^', ':', '*', '['])
        || input.contains("..")
        || input.contains("@{")
        || input == "@"
        || input
            .split('/')
            .any(|p| p.is_empty() || p.starts_with('.') || p.ends_with('.') || p.ends_with(".lock"))
    {
        return Err(input_error());
    }
    Ok(input.into())
}
pub(super) fn validate_commit(input: &str) -> Result<String, RepositoryError> {
    if !matches!(input.len(), 40 | 64) || !input.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(RepositoryError::InvalidResponse);
    }
    Ok(input.to_ascii_lowercase())
}
pub(super) fn validate_path(input: &str) -> Result<String, RepositoryError> {
    if !clean(input)
        || input.contains(['\\', ':'])
        || input
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(RepositoryError::UnsafePath);
    }
    Ok(input.into())
}
fn web_url(input: &str) -> Result<Url, RepositoryError> {
    if !clean(input) || input.trim() != input || input.contains('\\') {
        return Err(input_error());
    }
    let url = Url::parse(input).map_err(|_| input_error())?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(input_error());
    }
    Ok(url)
}
fn has_userinfo(input: &str) -> bool {
    input
        .split_once("://")
        .is_some_and(|(_, rest)| rest.split('/').next().is_some_and(|a| a.contains('@')))
}
fn root_url(input: &str) -> Result<Url, RepositoryError> {
    let url = web_url(input)?;
    let (_, authority) = input.split_once("://").ok_or_else(input_error)?;
    if authority
        .split_once('/')
        .is_some_and(|(_, path)| !path.is_empty())
    {
        return Err(input_error());
    }
    Ok(url)
}
impl Endpoint {
    pub fn parse(input: &str, allow_http: bool) -> Result<Self, RepositoryError> {
        let url = root_url(input)?;
        if has_userinfo(input) || (url.scheme() == "http" && !allow_http) {
            return Err(input_error());
        }
        Ok(Self { url, allow_http })
    }
    pub fn url(&self) -> &Url {
        &self.url
    }
    pub fn is_modelscope(&self) -> bool {
        matches!(
            self.url.host_str(),
            Some("modelscope.ai" | "www.modelscope.ai")
        )
    }
}
impl ProxySpec {
    pub fn parse(input: &str) -> Result<Self, RepositoryError> {
        let authenticated = root_url(input)?;
        let origin = authenticated.origin().ascii_serialization();
        let address = ProxyAddress::parse(&origin)?;
        Ok(Self {
            authenticated,
            address,
        })
    }
    pub fn address(&self) -> ProxyAddress {
        self.address.clone()
    }
    pub(crate) fn authenticated_url(&self) -> &Url {
        &self.authenticated
    }
}
impl ProxyAddress {
    pub fn parse(input: &str) -> Result<Self, RepositoryError> {
        let url = root_url(input)?;
        if has_userinfo(input) {
            return Err(input_error());
        }
        Ok(Self(url))
    }
    pub fn url(&self) -> &Url {
        &self.0
    }
}
impl DownloadUrl {
    pub(crate) fn from_url(url: Url) -> Result<Self, RepositoryError> {
        if !matches!(url.scheme(), "https" | "http")
            || url.host_str().is_none()
            || has_userinfo(url.as_str())
            || url.fragment().is_some()
        {
            return Err(RepositoryError::InvalidResponse);
        }
        Ok(Self(url))
    }
    pub(crate) fn url(&self) -> &Url {
        &self.0
    }
}
fn decode_segment(input: &str) -> Result<String, RepositoryError> {
    let mut bytes = Vec::with_capacity(input.len());
    let mut iter = input.bytes();
    while let Some(byte) = iter.next() {
        if byte == b'%' {
            let high = iter
                .next()
                .and_then(|c| char::from(c).to_digit(16))
                .ok_or_else(input_error)?;
            let low = iter
                .next()
                .and_then(|c| char::from(c).to_digit(16))
                .ok_or_else(input_error)?;
            bytes.push(u8::try_from(high * 16 + low).map_err(|_| input_error())?);
        } else {
            bytes.push(byte);
        }
    }
    String::from_utf8(bytes).map_err(|_| input_error())
}
pub fn parse_repository(
    input: &str,
    defaults: &RepositoryRequest,
) -> Result<RepositoryRequest, RepositoryError> {
    if !input.contains("://") {
        return Ok(RepositoryRequest {
            repo_id: RepoId::parse(input)?,
            ..defaults.clone()
        });
    }
    let url = web_url(input)?;
    if has_userinfo(input) {
        return Err(input_error());
    }
    let (_, rest) = input.split_once("://").ok_or_else(input_error)?;
    let (_, raw_path) = rest.split_once('/').ok_or_else(input_error)?;
    let path = raw_path.strip_suffix('/').unwrap_or(raw_path);
    let mut parts = path
        .split('/')
        .map(decode_segment)
        .collect::<Result<Vec<_>, _>>()?;
    if parts
        .iter()
        .any(|p| p.is_empty() || p == "." || p == ".." || p.contains('/'))
    {
        return Err(input_error());
    }
    let endpoint = Endpoint::parse(
        &url.origin().ascii_serialization(),
        defaults.endpoint.allow_http,
    )?;
    if endpoint.is_modelscope() && parts.first().is_some_and(|p| p == "models") {
        parts.remove(0);
        if parts.len() == 3 && matches!(parts[2].as_str(), "summary" | "files") {
            parts.pop();
        }
    }
    let kind = if parts.first().is_some_and(|p| p == "datasets") {
        parts.remove(0);
        RepoKind::Dataset
    } else {
        RepoKind::Model
    };
    let requested_revision = if endpoint.is_modelscope()
        && !defaults.endpoint.is_modelscope()
        && defaults.requested_revision.as_str() == "main"
    {
        Revision::parse("master")?
    } else {
        defaults.requested_revision.clone()
    };
    Ok(RepositoryRequest {
        endpoint,
        kind,
        repo_id: RepoId::parse(&parts.join("/"))?,
        requested_revision,
    })
}
