# hfdx

[简体中文](README.md) · [繁體中文](README.zh-TW.md) · [English](README.en.md)

hfdx 是用 Rust 編寫的 Hugging Face 和 ModelScope 下載器。桌面使用者可以在 GUI 中挑選模型分片、預覽大小、管理多個倉庫；通過 SSH 登入伺服器時，可以在 TUI 中完成同樣的下載流程。Hugging Face 同時支援模型和資料集，ModelScope 支援模型。

hfdx 自有程式碼採用 **PolyForm Noncommercial 1.0.0**。在許可允許的非商業用途範圍內，可以免費使用、修改和分發原始碼與程式，分發時須保留許可與必要宣告。企業為自身業務下載模型、將 hfdx 整合到收費產品等超出許可範圍的商業用途，須事先向權利人取得單獨商業授權。aria2、Slint 等第三方元件各自適用原有許可。[許可全文](LICENSE) · [聯絡維護者諮詢商業授權](https://github.com/skygunner) · [第三方宣告](docs/licenses/THIRD_PARTY.md)

上述授權適用於隨附本授權條款的版本。已發布的 v0.2.1 安裝套件附有當時的授權條款，使用該版本時請查閱套件內的 LICENSE。

## 下載與安裝

有系統安裝許可權並希望統一管理升級時，選發行版包；沒有 root 許可權、在共享伺服器上安裝，或希望用內建更新器時，選 Linux 可攜版。Windows x64 選 ZIP。

### Linux 發行版安裝包

在 [Releases](https://github.com/skygunner/hfdx/releases/latest) 選擇對應發行版和 CPU 架構的安裝包。同時下載 SHA256SUMS.native，在下載目錄執行 `sha256sum --ignore-missing -c SHA256SUMS.native`，確認所選套件顯示 OK。三個格式都包含 GUI、TUI 和桌面選單入口。套件管理員會安裝 aria2 等依賴，並記錄檔案以便升級和解除安裝。

| 發行版 | 下載格式 | 在下載目錄安裝（將檔名換為實際下載的包） |
| --- | --- | --- |
| Debian / Ubuntu | `.deb`（amd64 / arm64） | `sudo apt install ./檔名.deb` |
| Fedora 等 RPM 系統 | `.rpm`（x86_64 / aarch64） | `sudo dnf install ./檔名.rpm` |
| Arch Linux / Arch Linux ARM | `.pkg.tar.zst`（x86_64 / aarch64） | `sudo pacman -U ./檔名.pkg.tar.zst` |

在選單中開啟 **hfdx**，或執行 `hfdx-gui` / `hfdx`。這些包由專案在 GitHub 釋出。安裝新版請下載新包並重復相應命令；系統套件管理員負責管理 `/usr/bin` 下的程式。解除安裝使用 `sudo apt remove hfdx`、`sudo dnf remove hfdx` 或 `sudo pacman -R hfdx`。

### 可攜版與 Windows

從 [GitHub Releases](https://github.com/skygunner/hfdx/releases/latest) 下載對應平台的套件及 SHA256SUMS。包內含桌面程式和終端程式；Release 中同名平台的獨立執行檔也供內建更新器使用。

| 平台 | 下載包 |
| --- | --- |
| Linux x86_64 | hfdx-linux-x86_64.tar.gz |
| Linux ARM64（aarch64） | hfdx-linux-aarch64.tar.gz |
| Windows x64 | hfdx-windows-x86_64.zip |

Linux 可攜版直接裝入自己的目錄。先執行 `aria2c --version`。若缺少 aria2c，可將相容的 aria2c 放入使用者 PATH（例如已加入 PATH 的 `~/.local/bin`），或請管理員提供。然後在下載目錄執行：

```sh
package=hfdx-linux-$(uname -m)
awk -v f="$package.tar.gz" '$2 == f { print }' SHA256SUMS | sha256sum -c - &&
tar -xzf "$package.tar.gz" &&
mkdir -p ~/.local/bin &&
install -m 755 "$package/hfdx" "$package/hfdx-gui" ~/.local/bin/
```

桌面啟動 `~/.local/bin/hfdx-gui`，終端執行 `~/.local/bin/hfdx`。Windows 使用者用 PowerShell 的 `Get-FileHash -Algorithm SHA256` 核對 ZIP 摘要後解壓到使用者可寫目錄，安裝 aria2 並將 aria2c.exe 加入 PATH，然後雙擊 hfdx-gui.exe 或在終端執行 hfdx.exe。GUI 需要圖形桌面；遇到系統執行庫不相容可按下文從原始碼建置。

### 複製給 Agent 的一鍵安裝提示詞

> 請從 https://github.com/skygunner/hfdx/releases/latest 安裝與我的系統、CPU 及執行庫相容的正式版。先檢查已有安裝位置、許可權及 aria2c。需要全域性安裝且有相應授權時，選 deb、rpm 或 pacman 包，按同版 SHA256SUMS.native 核對所選檔案後用系統套件管理員安裝。無 root 許可權或僅為我安裝時，選 Linux tar.gz，按同版 SHA256SUMS 核對所選檔案後用普通使用者許可權裝入 ~/.local/bin；無桌面只需 hfdx。Windows x64 選 ZIP，按 SHA256SUMS 校驗後解壓到使用者目錄。確認為使用者 PATH 提供了可用的 aria2c；如缺少依賴請說明。沒有相容包時從發行標籤用 Cargo --locked 建置。最後驗證程式和 aria2c 的版本，告訴我安裝路徑、啟動和升級方式。平臺權杖由我在程式內填寫，商業用途請提醒我另行取得授權。

## 第一次下載

1. 開啟“設定”，按平台點選獲取令牌連結，填寫後點擊“儲存令牌”。Hugging Face 和 ModelScope 憑據分別儲存，重啟後可以繼續使用。[Hugging Face 令牌](https://huggingface.co/settings/tokens) · [ModelScope 令牌](https://modelscope.ai/my/myaccesstoken)
2. 在“新建下載”貼上倉庫地址，例如 `https://huggingface.co/openai-community/gpt2`。先用一個小檔案試下載，例如 `config.json`。ModelScope 使用形如 `https://modelscope.ai/models/組織/模型` 的地址。
3. 載入檔案列表，按名稱、萬用字元或 GGUF 量化組合選擇內容。預覽最終清單，核對分片、總大小和儲存目錄，再加入佇列。多組選中的共同檔案只下載一次。
4. 到“下載任務”檢視進度、暫停或繼續。出錯時選中任務，開啟錯誤詳情；需要診斷時可在設定中開啟 Debug 日誌。刪除已完成的任務立即生效，執行中或出錯的任務會要求確認。刪除任務會保留磁碟上的模型檔案。

TUI 用 Tab 切換輸入位置，用終端貼上快捷鍵輸入。F3 載入檔案，F4 開始下載，Ctrl+D 檢視任務，Ctrl+C 儲存退出。每頁底部顯示當前可用快捷鍵。

## 下載設計和注意事項

- GUI（Slint）和 TUI（Ratatui）共用 Rust 核心。核心讀取目錄、固定倉庫版本、管理佇列並驗證檔案，外部 aria2c 負責傳輸和斷點續傳。退出程式會停止傳輸，下次開啟後選擇任務並繼續。保留任務目錄中的 `.hfdx/` 和未完成檔案的 `.aria2` 記錄。
- 預設按上游提供的摘要校驗，例如 LFS SHA-256 或 Git blob SHA-1。缺少支援的摘要會顯示“未校驗”。同名檔案可先核對摘要，內容衝突時再決定如何處理。
- 匿名下載受平台配額和限流影響，批次或大型下載建議先儲存對應平台的只讀令牌。受限倉庫還需要賬戶獲准訪問。臨時端點令牌優先於已儲存的令牌；清空臨時值即可恢復平台憑據。令牌儲存在本機配置檔案中，Linux 預設位置是 `~/.config/hfdx/settings.json`。分享日誌或反饋時請勿附上該檔案。
- 下載和校驗共用併發額度。機械硬碟上同時讀寫多個大分片可能拖慢速度，可調低併發檔案數；開始前請按預覽大小預留空間，替換已有檔案時還需要暫存空間。

## 檢查更新與反饋

GUI 和 TUI 的設定均支援手動檢查 GitHub 正式版。自動檢查預設關閉，可選每天、每週或每 30 天；GUI 修改後點選儲存。發行版套件透過套件管理員升級。可攜版發現新版後選擇“更新”，程式按當前平台與介面下載執行檔，核對 GitHub 釋出的 SHA-256 後替換。GUI 和 TUI 可分別更新。安裝目錄需要目前使用者有寫許可權。正在下載、校驗或準備任務時可繼續工作；佇列空閒後才會詢問是否重啟。選擇“稍後”可在設定中再次請求重啟。

選擇“起草 GitHub Issue”會開啟瀏覽器，將程式版本、平台、錯誤類別、狀態碼和最近的日誌計數預填到草稿。使用者可修改並決定是否提交。草稿省略賬戶、令牌、原始日誌、時間戳、倉庫名、本地路徑和任務標識。也可直接[提交 Issue](https://github.com/skygunner/hfdx/issues/new) 或通過 fork 提交 PR。

功能釋出遞增次版本號（例如 0.1.0 → 0.2.0）；僅修復問題的釋出遞增補丁版本號（例如 0.2.0 → 0.2.1）。

## 從原始碼建置

安裝 Git、Rustup、系統 C/C++ 建置工具和 aria2。倉庫的 rust-toolchain.toml 固定建置工具鏈。

```sh
git clone https://github.com/skygunner/hfdx.git
cd hfdx
cargo build --release --locked -p hfdx-gui -p hfdx-tui
./target/release/hfdx-gui
```

伺服器可單獨建置 `cargo build --release --locked -p hfdx-tui`。三個 crate 分別位於 `crates/core`、`crates/gui`、`crates/tui`。執行測試用 `cargo test --workspace --locked`，部分測試需要 Python 3、aria2 和桌面顯示環境。

## 致謝與許可

感謝 **padeoe** 編寫原版 hfd Bash 指令碼。hfdx 學習了 hfd 先讀取倉庫清單、按需篩選檔案、再交給 aria2 併發下載和續傳的設計，並用 Rust 重新設計和實現了下載核心、桌面介面和終端介面。[原作者的 hfd 指令碼與說明](https://gist.github.com/padeoe/697678ab8e528b85a2a7bddafea1fa4f)

GGUF 檔案分組參考了 bodaay 的 HuggingFaceModelDownloader。專案和第三方元件各自的使用條件見隨附宣告。[上游專案](https://github.com/bodaay/HuggingFaceModelDownloader) · [專案許可](LICENSE) · [第三方宣告](docs/licenses/THIRD_PARTY.md)

[![Made with Slint](https://raw.githubusercontent.com/slint-ui/slint/master/logo/MadeWithSlint-logo-whitebg.png)](https://slint.dev)
