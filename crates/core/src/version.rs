//! Build identity shared by the GUI, TUI and command-line version output.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const COMMIT: &str = env!("HFDX_GIT_COMMIT");
pub const SOURCE_ID: &str = env!("HFDX_SOURCE_ID");
pub const MODIFIED: bool = matches!(env!("HFDX_GIT_DIRTY").as_bytes(), b"true");

pub fn description() -> String {
    format!(
        "hfdx {VERSION}\ncommit: {COMMIT}{}\nsource: {}",
        if MODIFIED { " (modified)" } else { "" },
        &SOURCE_ID[..12]
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn identity_has_a_real_source_digest_and_reports_modified_code() {
        assert!(
            super::COMMIT == "unknown"
                || (super::COMMIT.len() == 40
                    && super::COMMIT.bytes().all(|b| b.is_ascii_hexdigit()))
        );
        assert_eq!(super::SOURCE_ID.len(), 64);
        assert!(super::SOURCE_ID.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(super::description().contains(super::VERSION));
        assert!(super::description().contains(&super::SOURCE_ID[..12]));
        assert_eq!(super::description().contains("(modified)"), super::MODIFIED);
    }
}
