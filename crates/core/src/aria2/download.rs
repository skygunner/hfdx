use super::{Aria2, ProbeError};
use crate::{
    repository::{Bytes, DownloadUrl, ProxySpec},
    storage::{DownloadOptions, FailureKind, FileFailure, PreparedFile},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Gid(u64);
impl Gid {
    fn parse(value: &str) -> Result<Self, ProbeError> {
        if value.len() != 16 {
            return Err(ProbeError::Protocol);
        }
        match u64::from_str_radix(value, 16) {
            Ok(0) | Err(_) => Err(ProbeError::Protocol),
            Ok(value) => Ok(Self(value)),
        }
    }
    fn text(self) -> String {
        format!("{:016x}", self.0)
    }
}
pub(crate) struct DownloadJob {
    pub url: DownloadUrl,
    pub file: PreparedFile,
    pub proxy: Option<ProxySpec>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EngineState {
    Active,
    Waiting,
    Paused,
    Complete,
    Failed,
}
#[derive(Debug)]
pub(crate) struct EngineFileStatus {
    pub gid: Gid,
    pub state: EngineState,
    pub completed_bytes: Bytes,
    pub total_bytes: Bytes,
    pub bytes_per_second: u64,
    pub failure: Option<FileFailure>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    gid: String,
    status: String,
    completed_length: String,
    total_length: String,
    download_speed: String,
    #[serde(default)]
    error_code: Option<String>,
    #[serde(default)]
    error_message: Option<String>,
}
impl Status {
    fn parse(self) -> Result<EngineFileStatus, ProbeError> {
        let state = match self.status.as_str() {
            "active" => EngineState::Active,
            "waiting" => EngineState::Waiting,
            "paused" => EngineState::Paused,
            "complete" => EngineState::Complete,
            "error" | "removed" => EngineState::Failed,
            _ => return Err(ProbeError::Response("unknown download status")),
        };
        let failure = if state == EngineState::Failed {
            Some(failure(
                self.error_code.as_deref(),
                self.error_message.as_deref(),
                self.status == "removed",
            ))
        } else {
            None
        };
        Ok(EngineFileStatus {
            gid: Gid::parse(&self.gid).map_err(|_| ProbeError::Response("download GID"))?,
            state,
            completed_bytes: Bytes::new(number(&self.completed_length, "completedLength")?),
            total_bytes: Bytes::new(number(&self.total_length, "totalLength")?),
            bytes_per_second: number(&self.download_speed, "downloadSpeed")?,
            failure,
        })
    }
}
fn number(value: &str, field: &'static str) -> Result<u64, ProbeError> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ProbeError::Response(field));
    }
    value.parse().map_err(|_| ProbeError::Response(field))
}
fn failure(code: Option<&str>, message: Option<&str>, removed: bool) -> FileFailure {
    // Keep only numeric diagnostics: aria2 messages can contain URLs and credentials.
    let http_status = message.and_then(|message| {
        let (_, status) = message.split_once("status=")?;
        let digits: String = status.chars().take_while(char::is_ascii_digit).collect();
        let value = digits.parse::<u16>().ok()?;
        (100..=599).contains(&value).then_some(value)
    });
    let (kind, message) = if removed {
        (FailureKind::Interrupted, "Download interrupted")
    } else {
        match code {
            Some("3" | "4" | "5" | "6" | "7" | "19" | "21" | "22" | "23" | "29") => {
                (FailureKind::Network, "Download network request failed")
            }
            Some("24") => (
                FailureKind::Authentication,
                "Download authentication failed",
            ),
            Some("9" | "13" | "14" | "15" | "16" | "17" | "18") => {
                (FailureKind::Disk, "Download disk operation failed")
            }
            Some("32") => (FailureKind::Integrity, "Download integrity check failed"),
            Some("2") => (FailureKind::Network, "Download timed out"),
            _ => (FailureKind::Engine, "Download engine failed"),
        }
    };
    FileFailure {
        kind: if matches!(http_status, Some(401 | 403)) {
            FailureKind::Authentication
        } else {
            kind
        },
        message: message.into(),
        aria2_code: code.and_then(|code| code.parse().ok()),
        http_status,
    }
}
impl Aria2 {
    pub(crate) async fn configure(&mut self, options: &DownloadOptions) -> Result<(), ProbeError> {
        if self.next_gid != 1 {
            return Err(ProbeError::Protocol);
        }
        self.rpc
            .ok(
                "aria2.changeGlobalOption",
                vec![json!({
                    "split":options.connections_per_file().to_string(),
                    "max-connection-per-server":options.connections_per_file().to_string(),
                    "max-concurrent-downloads":options.concurrent_files().to_string(),
                    "min-split-size":"1M"
                })],
            )
            .await?;
        self.options = Some(*options);
        Ok(())
    }
    pub(crate) async fn add(&mut self, job: DownloadJob) -> Result<Gid, ProbeError> {
        let options = self.options.as_ref().ok_or(ProbeError::Protocol)?;
        let directory = job.file.directory().to_str().ok_or(ProbeError::Protocol)?;
        let filename = job.file.filename().to_str().ok_or(ProbeError::Protocol)?;
        let mut settings = json!({"dir":directory,"out":filename,
            "split":options.connections_per_file().to_string(),
            "max-connection-per-server":options.connections_per_file().to_string(),
            "min-split-size":"1M", "continue":"true", "allow-overwrite":"false",
            "auto-file-renaming":"false", "always-resume":"true",
            "all-proxy":"", "http-proxy":"", "https-proxy":"", "ftp-proxy":""});
        if let Some(proxy) = job.proxy {
            if proxy.authenticated_url().scheme() != "http" {
                return Err(ProbeError::UnsupportedProxy);
            }
            settings["http-proxy"] = json!(proxy.authenticated_url().as_str());
            settings["https-proxy"] = json!(proxy.authenticated_url().as_str());
        }
        let gid = Gid(self.next_gid).text();
        self.next_gid = self.next_gid.checked_add(1).ok_or(ProbeError::Protocol)?;
        settings["gid"] = json!(gid);
        let params = vec![json!([job.url.url().as_str()]), settings];
        let first = self
            .rpc
            .call::<String>("aria2.addUri", params.clone())
            .await;
        let result = match first {
            Err(ProbeError::Timeout | ProbeError::Connection) => {
                self.recover_add(&gid, &params).await?
            }
            result => result?,
        };
        if result != gid {
            return Err(ProbeError::Response("unexpected download GID"));
        }
        Gid::parse(&result)
    }
    async fn recover_add(&mut self, gid: &str, params: &[Value]) -> Result<String, ProbeError> {
        // addUri can have succeeded even when its reply was lost. Reuse the GID if it needs replaying.
        tokio::time::timeout(Duration::from_secs(60), async {
            let mut rejected = None;
            loop {
                if let Some(exit) = self.process.child.try_wait()? {
                    return Err(ProbeError::Exited(exit.code()));
                }
                match self
                    .rpc
                    .call::<Value>("aria2.tellStatus", vec![json!(gid), json!(["gid"])])
                    .await
                {
                    Ok(status) if status["gid"].as_str() == Some(gid) => return Ok(gid.to_owned()),
                    Ok(_) => return Err(ProbeError::Response("unexpected download GID")),
                    Err(ProbeError::Timeout | ProbeError::Connection) => {}
                    Err(ProbeError::Rpc(1)) => {
                        if let Some(error) = rejected {
                            return Err(error);
                        }
                        match self.rpc.call("aria2.addUri", params.to_vec()).await {
                            Err(ProbeError::Timeout | ProbeError::Connection) => {}
                            // A late first request can win between the lookup and the replay.
                            Err(error @ ProbeError::Rpc(1)) => rejected = Some(error),
                            result => return result,
                        }
                    }
                    Err(error) => return Err(error),
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
        .await
        .map_err(|_| ProbeError::Timeout)?
    }
    pub(crate) async fn statuses(
        &mut self,
        gids: &[Gid],
    ) -> Result<Vec<EngineFileStatus>, ProbeError> {
        let mut output = Vec::with_capacity(gids.len());
        for gid in gids {
            let params = vec![
                json!(gid.text()),
                json!([
                    "gid",
                    "status",
                    "completedLength",
                    "totalLength",
                    "downloadSpeed",
                    "errorCode",
                    "errorMessage"
                ]),
            ];
            let result = self.rpc.call::<Status>("aria2.tellStatus", params).await;
            if matches!(result, Err(ProbeError::Timeout | ProbeError::Connection))
                && let Some(exit) = self.process.child.try_wait()?
            {
                return Err(ProbeError::Exited(exit.code()));
            }
            let raw = result?;
            let status = raw.parse()?;
            if status.gid != *gid {
                return Err(ProbeError::Response("unexpected download GID"));
            }
            output.push(status);
        }
        Ok(output)
    }
    #[cfg(test)]
    pub(crate) async fn pause_all(&mut self) -> Result<(), ProbeError> {
        self.rpc.ok("aria2.forcePauseAll", vec![]).await
    }
    #[cfg(test)]
    pub(crate) async fn resume_all(&mut self) -> Result<(), ProbeError> {
        self.rpc.ok("aria2.unpauseAll", vec![]).await
    }
    #[cfg(test)]
    pub(crate) async fn forget(&mut self, gid: &Gid) -> Result<(), ProbeError> {
        self.rpc
            .ok("aria2.removeDownloadResult", vec![json!(gid.text())])
            .await
    }
    #[cfg(test)]
    pub(crate) async fn remove(&mut self, gid: &Gid) -> Result<(), ProbeError> {
        let result: String = self
            .rpc
            .call("aria2.forceRemove", vec![json!(gid.text())])
            .await?;
        if Gid::parse(&result)? != *gid {
            return Err(ProbeError::Protocol);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rpc_download_failures_keep_codes_and_discard_sensitive_message_text() {
        for (code, status, kind) in [
            ("22", 429, FailureKind::Network),
            ("24", 401, FailureKind::Authentication),
            ("22", 403, FailureKind::Authentication),
        ] {
            let raw: Status = serde_json::from_value(json!({
                "gid":"0123456789abcdef","status":"error","completedLength":"0",
                "totalLength":"100","downloadSpeed":"0","errorCode":code,
                "errorMessage":format!("The response status is not successful. status={status} https://host/?token=fake-token fake-password")
            })).unwrap();
            let failure = raw.parse().unwrap().failure.unwrap();
            assert_eq!(failure.http_status, Some(status));
            assert_eq!(failure.aria2_code, Some(code.parse().unwrap()));
            assert_eq!(failure.kind, kind);
            let persisted = serde_json::to_string(&failure).unwrap();
            assert!(!persisted.contains("fake-token") && !persisted.contains("fake-password"));
            let loaded: FileFailure = serde_json::from_str(&persisted).unwrap();
            let visible = crate::locale::file_failure(crate::Language::ZhHans, &loaded);
            assert!(
                visible.contains(&format!("HTTP {status}"))
                    && visible.contains(&format!("aria2 {code}"))
            );
            if status == 429 {
                assert!(visible.contains("限流") && visible.contains("令牌"));
            }
        }
    }
    #[test]
    fn old_records_and_invalid_engine_details_still_render_without_secrets() {
        let failure: FileFailure = serde_json::from_value(
            json!({"kind":"network","message":"https://secret?token=legacy-secret"}),
        )
        .unwrap();
        assert!(failure.http_status.is_none() && failure.aria2_code.is_none());
        assert!(
            !crate::locale::file_failure(crate::Language::English, &failure).contains("secret")
        );
        for message in [
            "status=garbage",
            "status=9999",
            "https://host/?token=secret",
        ] {
            assert_eq!(
                super::failure(Some("22"), Some(message), false).http_status,
                None
            );
        }
    }
}
