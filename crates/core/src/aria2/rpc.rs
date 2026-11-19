use super::ProbeError;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;
use std::time::Duration;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Deserialize)]
struct Fault {
    code: i64,
}
#[derive(Deserialize)]
struct Reply<T> {
    jsonrpc: String,
    id: String,
    result: Option<T>,
    error: Option<Fault>,
}
pub(super) struct Rpc {
    client: reqwest::Client,
    endpoint: String,
    secret: String,
}
fn exchange_error(error: reqwest::Error) -> ProbeError {
    if error.is_timeout() {
        ProbeError::Timeout
    } else if error.is_decode() {
        ProbeError::Response("JSON response")
    } else {
        ProbeError::Connection
    }
}
impl Rpc {
    pub(super) fn new(port: u16, secret: String) -> Result<Self, ProbeError> {
        Ok(Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(5))
                .timeout(REQUEST_TIMEOUT)
                .build()
                .map_err(|_| ProbeError::Protocol)?,
            endpoint: format!("http://127.0.0.1:{port}/jsonrpc"),
            secret,
        })
    }
    pub(super) async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Vec<Value>,
    ) -> Result<T, ProbeError> {
        self.request(method, params, true).await
    }
    // Only used for requests that are safe to repeat after a lost reply.
    pub(super) async fn retry<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Vec<Value>,
        authenticated: bool,
    ) -> Result<T, ProbeError> {
        for delay in [250, 500, 1000] {
            match self.request(method, params.clone(), authenticated).await {
                Err(ProbeError::Timeout | ProbeError::Connection) => {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                result => return result,
            }
        }
        self.request(method, params, authenticated).await
    }
    pub(super) async fn request<T: DeserializeOwned>(
        &self,
        method: &str,
        mut params: Vec<Value>,
        authenticated: bool,
    ) -> Result<T, ProbeError> {
        if authenticated {
            params.insert(0, Value::String(format!("token:{}", self.secret)));
        }
        let response = self
            .client
            .post(&self.endpoint)
            .json(&serde_json::json!({"jsonrpc":"2.0", "id":"hfdx", "method":method, "params":params}))
            .send()
            .await
            .map_err(exchange_error)?;
        // aria2 negative authentication uses HTTP 400 with a JSON-RPC fault.
        let reply: Reply<T> = response.json().await.map_err(exchange_error)?;
        if reply.jsonrpc != "2.0" || reply.id != "hfdx" {
            return Err(ProbeError::Response("JSON-RPC version or ID"));
        }
        match (reply.result, reply.error) {
            (Some(value), None) => Ok(value),
            (None, Some(fault)) => Err(ProbeError::Rpc(fault.code)),
            (Some(_), Some(_)) | (None, None) => Err(ProbeError::Response("result or error")),
        }
    }
    pub(super) async fn ok(&self, method: &str, params: Vec<Value>) -> Result<(), ProbeError> {
        let result: String = self.retry(method, params, true).await?;
        if result == "OK" {
            Ok(())
        } else {
            Err(ProbeError::Protocol)
        }
    }
}

#[cfg(test)]
#[tokio::test]
async fn repeatable_commands_survive_three_lost_connections() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rpc = Rpc::new(listener.local_addr().unwrap().port(), "fake-secret".into()).unwrap();
    let server = tokio::spawn(async move {
        for attempt in 1..=4 {
            let (mut socket, _) = listener.accept().await.unwrap();
            assert!(socket.read(&mut [0; 4096]).await.unwrap() > 0);
            if attempt == 4 {
                let body = r#"{"jsonrpc":"2.0","id":"hfdx","result":"OK"}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        }
    });
    rpc.ok("aria2.changeGlobalOption", vec![]).await.unwrap();
    server.await.unwrap();
}
