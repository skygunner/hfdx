# hfdx

hfdx 是用 Rust 编写的 Hugging Face 和 ModelScope 下载器。桌面用户可以在 GUI 中挑选模型分片、预览大小、管理多个仓库；通过 SSH 登录服务器时，可以在 TUI 中完成同样的下载流程。Hugging Face 同时支持模型和数据集，ModelScope 支持模型。

## 下载与安装

从 [GitHub Releases](https://github.com/skygunner/hfdx/releases/latest) 下载 Linux x86_64 压缩包和同一版本的 SHA256SUMS。包内有桌面程序 `hfdx-gui`、终端程序 `hfdx` 和许可说明。下载引擎需要单独安装，推荐 aria2 1.37 或更新版本。GUI 需要图形桌面。此版本的预编译 GUI 需要 glibc 2.43 或更新版本，TUI 需要 glibc 2.39 或更新版本；较旧的发行版可使用下方源码构建步骤。

在 Debian / Ubuntu 上可用 `sudo apt install aria2` 安装下载引擎，Arch Linux 使用 `sudo pacman -S aria2`。在下载目录执行：

```sh
sha256sum -c SHA256SUMS &&
tar -xzf hfdx-linux-x86_64.tar.gz &&
mkdir -p ~/.local/bin &&
install -m 755 hfdx-linux-x86_64/hfdx hfdx-linux-x86_64/hfdx-gui ~/.local/bin/
~/.local/bin/hfdx-gui --help
```

启动桌面界面用 `~/.local/bin/hfdx-gui`，终端界面用 `~/.local/bin/hfdx`。如果系统缺少动态库，可用 `ldd ~/.local/bin/hfdx-gui` 查看。

### 复制给 Agent 的一键安装提示词

> 请为当前用户安装 https://github.com/skygunner/hfdx 的最新正式版。先检查系统、CPU、glibc 和 aria2c，在官方 GitHub Release 下载兼容的压缩包及 SHA256SUMS，验证成功后安装到用户的可执行目录。没有兼容二进制时，从该发行标签按 README 用 Cargo 的 --locked 选项构建；无桌面的服务器只安装 TUI。最后运行已安装程序的 --help 和 aria2c --version，告诉我版本、安装路径和启动命令。让我自己在界面里填写平台令牌。

## 第一次下载

1. 打开“设置”，按平台点击获取令牌链接，填写后点击“保存令牌”。Hugging Face 和 ModelScope 凭据分别保存，重启后可以继续使用。[Hugging Face 令牌](https://huggingface.co/settings/tokens) · [ModelScope 令牌](https://modelscope.ai/my/myaccesstoken)
2. 在“新建下载”粘贴仓库地址，例如 `https://huggingface.co/openai-community/gpt2`。先用一个小文件试下载，例如 `config.json`。ModelScope 使用形如 `https://modelscope.ai/models/组织/模型` 的地址。
3. 加载文件列表，按名称、通配符或 GGUF 量化组合选择内容。预览最终清单，核对分片、总大小和保存目录，再加入队列。多组选中的共同文件只下载一次。
4. 到“下载任务”查看进度、暂停或继续。出错时选中任务，打开错误详情；需要诊断时可在设置中打开 Debug 日志。删除已完成的任务立即生效，运行中或出错的任务会要求确认。删除任务会保留磁盘上的模型文件。

TUI 用 Tab 切换输入位置，用终端粘贴快捷键输入。F3 加载文件，F4 开始下载，Ctrl+D 查看任务，Ctrl+C 保存退出。每页底部显示当前可用快捷键。

## 下载设计和注意事项

- GUI（Slint）和 TUI（Ratatui）共用 Rust 核心。核心读取目录、固定仓库版本、管理队列并验证文件，外部 aria2c 负责传输和断点续传。退出程序会停止传输，下次打开后选择任务并继续。保留任务目录中的 `.hfdx/` 和未完成文件的 `.aria2` 记录。
- 默认按上游提供的摘要校验，例如 LFS SHA-256 或 Git blob SHA-1。缺少支持的摘要会显示“未校验”。同名文件可先核对摘要，内容冲突时再决定如何处理。
- 匿名下载受平台配额和限流影响，批量或大型下载建议先保存对应平台的只读令牌。受限仓库还需要账户获准访问。临时端点令牌优先于已保存的令牌；清空临时值即可恢复平台凭据。令牌保存在本机配置文件中，Linux 默认位置是 `~/.config/hfdx/settings.json`。分享日志或反馈时请勿附上该文件。
- 下载和校验共用并发额度。机械硬盘上同时读写多个大分片可能拖慢速度，可调低并发文件数；开始前请按预览大小预留空间，替换已有文件时还需要暂存空间。

## 从源码构建

安装 Git、Rustup、系统 C/C++ 构建工具和 aria2。仓库的 rust-toolchain.toml 固定构建工具链。

```sh
git clone https://github.com/skygunner/hfdx.git
cd hfdx
cargo build --release --locked -p hfdx-gui -p hfdx-tui
./target/release/hfdx-gui
```

服务器可单独构建 `cargo build --release --locked -p hfdx-tui`。三个 crate 分别位于 `crates/core`、`crates/gui`、`crates/tui`。运行测试用 `cargo test --workspace --locked`，部分测试需要 Python 3、aria2 和桌面显示环境。

## 致谢与许可

感谢 **padeoe** 编写原版 hfd Bash 脚本。hfdx 学习了 hfd 先读取仓库清单、按需筛选文件、再交给 aria2 并发下载和续传的设计，并用 Rust 重新设计和实现了下载核心、桌面界面和终端界面。[原作者的 hfd 脚本与说明](https://gist.github.com/padeoe/697678ab8e528b85a2a7bddafea1fa4f)

GGUF 文件分组参考了 bodaay 的 HuggingFaceModelDownloader。项目和第三方组件各自的使用条件见随附声明。[上游项目](https://github.com/bodaay/HuggingFaceModelDownloader) · [项目许可](LICENSE) · [第三方声明](docs/licenses/THIRD_PARTY.md)

[![Made with Slint](https://raw.githubusercontent.com/slint-ui/slint/master/logo/MadeWithSlint-logo-whitebg.png)](https://slint.dev)
