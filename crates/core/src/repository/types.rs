//! Shared repository boundary values; secrets deliberately have no serde implementation.
use super::{validate_commit, validate_path, validate_repo_id, validate_revision};
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;
use url::Url;

macro_rules! text_value {
    ($name:ident, $validate:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);
        impl $name {
            pub fn parse(input: &str) -> Result<Self, RepositoryError> {
                $validate(input).map(Self)
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
            }
        }
    };
}
text_value!(RepoId, validate_repo_id);
text_value!(Revision, validate_revision);
text_value!(CommitId, validate_commit);
text_value!(RepoPath, validate_path);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "EndpointValue", into = "EndpointValue")]
pub struct Endpoint {
    pub(super) url: Url,
    pub(super) allow_http: bool,
}
#[derive(Serialize, Deserialize)]
struct EndpointValue {
    url: String,
    allow_http: bool,
}
impl TryFrom<EndpointValue> for Endpoint {
    type Error = RepositoryError;
    fn try_from(value: EndpointValue) -> Result<Self, Self::Error> {
        Self::parse(&value.url, value.allow_http)
    }
}
impl From<Endpoint> for EndpointValue {
    fn from(value: Endpoint) -> Self {
        Self {
            url: value.url.into(),
            allow_http: value.allow_http,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoKind {
    Model,
    Dataset,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Bytes(u64);
impl Bytes {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    pub const fn get(self) -> u64 {
        self.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CatalogId(pub(crate) u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EntryId {
    pub catalog: CatalogId,
    pub index: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HashAlgorithm {
    Sha256,
    GitSha1,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "HashValue")]
pub struct ContentHash {
    pub algorithm: HashAlgorithm,
    pub hex: String,
}
#[derive(Deserialize)]
struct HashValue {
    algorithm: HashAlgorithm,
    hex: String,
}
impl TryFrom<HashValue> for ContentHash {
    type Error = RepositoryError;
    fn try_from(value: HashValue) -> Result<Self, Self::Error> {
        let length = match value.algorithm {
            HashAlgorithm::Sha256 => 64,
            HashAlgorithm::GitSha1 => 40,
        };
        if value.hex.len() != length || !value.hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(RepositoryError::InvalidResponse);
        }
        Ok(Self {
            algorithm: value.algorithm,
            hex: value.hex.to_ascii_lowercase(),
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryRequest {
    pub endpoint: Endpoint,
    pub kind: RepoKind,
    pub repo_id: RepoId,
    pub requested_revision: Revision,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositorySpec {
    pub request: RepositoryRequest,
    pub resolved_commit: CommitId,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub path: RepoPath,
    pub size: Bytes,
    pub content_hash: Option<ContentHash>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogRequest {
    pub id: CatalogId,
    pub source: RepositorySpec,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogPage {
    pub catalog: CatalogId,
    pub entries: Vec<CatalogEntry>,
    pub complete: bool,
}
#[derive(Clone, Debug, Default)]
pub struct NetworkAccess {
    pub token: Option<Token>,
    pub proxy: Option<ProxySpec>,
}

#[derive(Clone)]
pub struct Token(String);
impl Token {
    pub fn parse(input: &str) -> Result<Self, RepositoryError> {
        if input.is_empty() || !input.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(super::input_error());
        }
        Ok(Self(input.into()))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
#[derive(Clone)]
pub struct ProxySpec {
    pub(super) authenticated: Url,
    pub(super) address: ProxyAddress,
}
#[derive(Clone)]
pub struct DownloadUrl(pub(super) Url);
macro_rules! redacted {
    ($($name:ident),+ $(,)?) => { $(
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "([REDACTED])"))
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { fmt::Debug::fmt(self, f) }
        }
    )+ };
}
redacted!(Token, ProxySpec, DownloadUrl);
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyAddress(pub(super) Url);
impl Serialize for ProxyAddress {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.as_str())
    }
}
impl<'de> Deserialize<'de> for ProxyAddress {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

pub enum RepositoryError {
    Input(String),
    Http { endpoint: Endpoint, status: u16 },
    Network,
    UnsupportedEndpoint,
    UnsafePath,
    InvalidResponse,
    Cancelled,
}
impl fmt::Display for RepositoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input(_) => f.write_str("Invalid repository input"),
            Self::Http { endpoint, status } => write!(f, "HTTP {status} from {}", endpoint.url()),
            Self::Network => f.write_str("Repository network request failed"),
            Self::UnsupportedEndpoint => f.write_str("Unsupported repository endpoint"),
            Self::UnsafePath => f.write_str("Unsafe repository path"),
            Self::InvalidResponse => f.write_str("Invalid repository response"),
            Self::Cancelled => f.write_str("Repository operation cancelled"),
        }
    }
}
impl fmt::Debug for RepositoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
impl std::error::Error for RepositoryError {}
