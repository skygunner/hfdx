# hfdx

[简体中文](README.md) · [繁體中文](README.zh-TW.md) · [English](README.en.md)

hfdx is a Rust downloader for Hugging Face models and datasets and for ModelScope models. Use the desktop GUI to select shards, preview disk usage and manage several repositories, or use the terminal UI (TUI) over SSH.

## Download and install

### Linux distribution packages

Choose your distribution and CPU architecture on the [Releases page](https://github.com/skygunner/hfdx/releases/latest). Each package includes the GUI, TUI and desktop menu entry. Download the matching package and SHA256SUMS.native to the same directory. Run `sha256sum --ignore-missing -c SHA256SUMS.native` and check that your package reports OK. Replace the example filename below with the downloaded filename.

| Distribution | Package and architecture | Install |
| --- | --- | --- |
| Debian / Ubuntu | .deb, amd64 or arm64 | `sudo apt install ./filename.deb` |
| Fedora and compatible RPM distributions | .rpm, x86_64 or aarch64 | `sudo dnf install ./filename.rpm` |
| Arch Linux / Arch Linux ARM | .pkg.tar.zst, x86_64 or aarch64 | `sudo pacman -U ./filename.pkg.tar.zst` |

Your package manager resolves dependencies, including aria2. Open **hfdx** from the desktop menu, run `hfdx-gui`, or run `hfdx` in a terminal. The project distributes these packages through GitHub. To upgrade a system installation, download the new package and repeat the installation command. To remove it, use `sudo apt remove hfdx`, `sudo dnf remove hfdx` or `sudo pacman -R hfdx`.

### Portable packages and Windows

Download `hfdx-linux-x86_64.tar.gz`, `hfdx-linux-aarch64.tar.gz` or `hfdx-windows-x86_64.zip` and SHA256SUMS from the same release. Each archive includes both interfaces. The individual executables on the release page are also used by the built-in updater.

For a per-user Linux installation, install aria2 and run the following in your download directory:

```sh
package=hfdx-linux-$(uname -m)
sha256sum --ignore-missing -c SHA256SUMS &&
tar -xzf "$package.tar.gz" &&
mkdir -p ~/.local/bin &&
install -m 755 "$package/hfdx" "$package/hfdx-gui" ~/.local/bin/
```

Start `~/.local/bin/hfdx-gui` on the desktop or `~/.local/bin/hfdx` in a terminal. On Windows, compare the ZIP's `Get-FileHash -Algorithm SHA256` output with SHA256SUMS, extract it to a user-writable folder, install aria2 and add its directory to PATH. Launch hfdx-gui.exe or run hfdx.exe in a terminal. The GUI requires a graphical session. If your system libraries are too old for the binaries, build from source.

### Installation prompt to copy to an agent

> Install the latest stable release of https://github.com/skygunner/hfdx for this machine. Check the OS, architecture, glibc and aria2c. Prefer the official Release's compatible deb, rpm or pacman package and verify it against SHA256SUMS.native before installing it with the system package manager. For a per-user installation, use the portable archive and SHA256SUMS. If no compatible binary is available, build from the release tag with Cargo --locked. On a headless server, install only the TUI if building from source. Run the installed program with --help and aria2c --version, then report the installation paths, version and launch commands. Leave token entry to me.

## Your first download

1. In Settings, follow the token link for your platform, enter the token and select **Save tokens**. The two credentials are stored separately and survive restarts. [Hugging Face tokens](https://huggingface.co/settings/tokens) · [ModelScope tokens](https://modelscope.ai/my/myaccesstoken)
2. Paste a repository URL into **New download**. Try `https://huggingface.co/openai-community/gpt2` and select just `config.json` for a small first download. For ModelScope use a URL of the form `https://modelscope.ai/models/owner/model`.
3. Load the catalog and select files by name, wildcard or GGUF quantization group. Preview the final paths, total size and destination before queuing. Shared files are counted once.
4. Open the download task page to see progress, pause or resume. Select a failed task to view error details. You can enable debug logging in Settings. Completed tasks can be removed immediately. Removing a running or failed task asks for confirmation and keeps downloaded files on disk.

In the TUI, Tab moves between fields and your terminal's paste shortcut works in input fields. F3 loads the catalog, F4 starts the download, Ctrl+D opens tasks and Ctrl+C saves and exits. The footer shows shortcuts for the current page.

## How downloads work

- The Slint GUI and Ratatui TUI share a Rust core that pins the repository revision, manages the queue and verifies files. An external aria2c process transfers the data and resumes partial files. Exiting stops transfers. Reopen the task and resume when ready. Keep its .hfdx directory and .aria2 control files.
- Verification uses an upstream digest where available, such as LFS SHA-256 or Git blob SHA-1. Files without a supported digest appear as unverified. Existing files can be compared before resolving a conflict.
- Anonymous downloads are subject to platform quotas and rate limits. Save a read token for large or repeated downloads. Gated repositories also require account approval. A temporary endpoint token takes precedence over the saved platform token. Clear it to use the saved credential again. Linux settings normally live in `~/.config/hfdx/settings.json`. Keep this file private when sharing logs or feedback.
- Transfers and verification share a concurrency limit. Reduce the concurrent file count if a mechanical drive struggles with simultaneous reads and writes. Allow enough disk space for the selected files and for staging replacements.

## Updates and feedback

Both interfaces can check GitHub for stable releases. Automatic checks default to off. Choose daily, weekly or every 30 days and save the GUI settings. For portable installations, **Update** downloads the executable for the current interface and verifies GitHub's SHA-256 before replacement. The installation directory must be writable. Active transfers and verification continue. Once the queue is idle, confirm the restart or choose **Later** and return to Settings when ready. For system packages, upgrade through the package manager.

**Draft GitHub issue** opens an editable draft in your browser with the program version, platform, error categories, status codes and counts of recent log events. You decide whether to submit it. The draft omits account details, tokens, raw logs, timestamps, repository names, local paths and task identifiers. You can also [open an issue](https://github.com/skygunner/hfdx/issues/new) or submit a pull request from a fork.

Feature releases increment the minor version (0.1.0 → 0.2.0). Bug-fix releases increment the patch version (0.2.0 → 0.2.1).

## Build from source

Install Git, Rustup, a C/C++ toolchain and aria2. The repository pins the Rust toolchain in rust-toolchain.toml. Linux GUI builds need fontconfig development headers.

```sh
git clone https://github.com/skygunner/hfdx.git
cd hfdx
cargo build --release --locked -p hfdx-gui -p hfdx-tui
./target/release/hfdx-gui
```

For a terminal-only build, use `cargo build --release --locked -p hfdx-tui`. The core, GUI and TUI live in crates/core, crates/gui and crates/tui. Run the test suite with `cargo test --workspace --locked`. Some tests need Python 3, aria2 and a graphical display.

## Credits and licensing

Thanks to **padeoe**, author of the original hfd Bash script. Its approach to retrieving a repository manifest, filtering files and delegating parallel, resumable transfers to aria2 inspired hfdx. The download core and both interfaces were redesigned and implemented in Rust. [Original hfd script and documentation](https://gist.github.com/padeoe/697678ab8e528b85a2a7bddafea1fa4f)

GGUF grouping draws on bodaay's HuggingFaceModelDownloader. The project and its dependencies retain their respective terms. [Upstream project](https://github.com/bodaay/HuggingFaceModelDownloader) · [Project license](LICENSE) · [Third-party notices](docs/licenses/THIRD_PARTY.md)

[![Made with Slint](https://raw.githubusercontent.com/slint-ui/slint/master/logo/MadeWithSlint-logo-whitebg.png)](https://slint.dev)
