#[path = "repository/types.rs"]
mod types;
pub use types::*;
#[path = "repository/input.rs"]
mod input;
pub use input::parse_repository;
use input::{input_error, validate_commit, validate_path, validate_repo_id, validate_revision};
#[path = "repository/urls.rs"]
mod urls;
pub(crate) use urls::{metadata_url, resolve_url};
#[path = "repository/client.rs"]
mod client;
pub use client::RepositoryClient;

#[cfg(test)]
#[path = "repository/private_tests.rs"]
mod private_tests;

#[cfg(test)]
#[path = "repository/type_tests.rs"]
mod type_tests;
