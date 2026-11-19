use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn sources(path: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(path).expect("source directory") {
        let path = entry.expect("source entry").path();
        if path.is_dir() {
            sources(&path, files);
        } else if matches!(
            path.extension().and_then(|s| s.to_str()),
            Some("rs" | "slint" | "po" | "toml" | "svg")
        ) {
            files.push(path);
        }
    }
}
fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
fn main() {
    let root =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest")).join("../..");
    println!("cargo:rerun-if-changed={}", root.join("crates").display());
    for name in [
        "Cargo.toml",
        "Cargo.lock",
        ".git/HEAD",
        ".git/refs",
        ".git/packed-refs",
    ] {
        if root.join(name).exists() {
            println!("cargo:rerun-if-changed={}", root.join(name).display());
        }
    }
    for name in ["HFDX_BUILD_COMMIT", "HFDX_BUILD_DIRTY"] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    let commit = std::env::var("HFDX_BUILD_COMMIT")
        .ok()
        .or_else(|| git(&root, &["rev-parse", "HEAD"]))
        .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or_else(|| "unknown".into());
    let dirty = std::env::var("HFDX_BUILD_DIRTY")
        .ok()
        .map(|s| s == "1" || s == "true")
        .unwrap_or_else(|| {
            git(
                &root,
                &[
                    "status",
                    "--porcelain",
                    "--untracked-files=normal",
                    "--",
                    "crates",
                    "Cargo.toml",
                    "Cargo.lock",
                ],
            )
            .is_some_and(|s| !s.is_empty())
        });
    let mut files = vec![root.join("Cargo.toml"), root.join("Cargo.lock")];
    sources(&root.join("crates"), &mut files);
    files.sort();
    let mut digest = Sha256::new();
    for path in files {
        digest.update(
            path.strip_prefix(&root)
                .expect("source within repository")
                .to_string_lossy()
                .replace('\\', "/"),
        );
        digest.update([0]);
        digest.update(fs::read(&path).expect("read source"));
        digest.update([0]);
    }
    println!("cargo:rustc-env=HFDX_GIT_COMMIT={commit}");
    println!("cargo:rustc-env=HFDX_GIT_DIRTY={dirty}");
    println!("cargo:rustc-env=HFDX_SOURCE_ID={:x}", digest.finalize());
}
