//! OpenCode background service discovery and the typed HTTP client.
//!
//! Discovery reads the service registration file the OpenCode background
//! service maintains (`~/.local/state/opencode/service.json`), which carries
//! the base URL and the generated password. Every request authenticates with
//! HTTP Basic auth (user `opencode` + that password).

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use reqwest::header::{HeaderValue, AUTHORIZATION};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

use super::{data_array, FileAttachment, Json, ModelRef, SessionInfo};

/// The OpenAPI-exposed server info (`GET /api/info`).
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerInfo {
    #[serde(default)]
    pub version: String,
}

/// A value that never prints itself. The service password must never appear
/// in logs, debug output, or process command lines.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("(redacted)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("(redacted)")
    }
}

/// Failures of [`ensure_connection`].
#[derive(Debug, Error)]
pub enum ConnectFailure {
    #[error("timed out waiting for the OpenCode background service to come up")]
    Timeout,
}

/// Discover a healthy background service: read the registration file, probe
/// it, and if there is no healthy service run `spawn_service` once, polling
/// until the registration file updates with a live server.
pub async fn ensure_connection_with_budget(
    reg_path: &Path,
    mut spawn_service: impl FnMut(),
    budget: Duration,
) -> Result<Client, ConnectFailure> {
    let started = tokio::time::Instant::now();
    let mut spawned = false;
    loop {
        if let Ok(reg) = Registration::load(reg_path)
            && let Ok(client) = Client::new(&reg)
            && client.info().await.is_ok()
        {
            return Ok(client);
        }
        if !spawned {
            (spawn_service)();
            spawned = true;
        }
        if started.elapsed() >= budget {
            return Err(ConnectFailure::Timeout);
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
}

/// The standard-budget variant used at runtime (15 s).
pub async fn ensure_connection(
    reg_path: &Path,
    spawn_service: impl FnMut(),
) -> Result<Client, ConnectFailure> {
    ensure_connection_with_budget(reg_path, spawn_service, Duration::from_secs(15)).await
}

/// The service registration record as written by the OpenCode background
/// service.
#[derive(Debug, Clone)]
pub struct Registration {
    pub url: String,
    pub password: Secret,
    #[allow(dead_code)]
    pub pid: Option<u32>,
    #[allow(dead_code)]
    pub version: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawRegistration {
    url: String,
    password: String,
    pid: Option<u32>,
    version: Option<String>,
}


impl Registration {
    /// The registration file path for the user's OpenCode background service.
    pub fn default_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "opencode")
            .as_ref()
            .and_then(|dirs| dirs.state_dir())
            .map(|dir| dir.join("service.json"))
    }

    pub fn load(path: &Path) -> std::io::Result<Registration> {
        let text = std::fs::read_to_string(path)?;
        let raw: RawRegistration = serde_json::from_str(&text).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
        })?;
        if raw.url.is_empty() || raw.password.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "registration file missing url or password",
            ));
        }
        Ok(Registration {
            url: raw.url.trim_end_matches('/').to_string(),
            password: Secret(raw.password),
            pid: raw.pid,
            version: raw.version,
        })
    }
}

/// Errors surfaced by API calls.
#[derive(Debug, Error)]
pub enum ApiError {
    /// The service is not reachable (not running, rejected, or timed out).
    #[error("service unreachable: {0}")]
    Unreachable(String),
    /// A prompt hit an already-busy session.
    #[error("service busy: a turn is already running in this session")]
    Busy,
    /// Another non-success response.
    #[error("http {status}: {message}")]
    Http { status: u16, message: String },
    #[error("unexpected response: {0}")]
    Parse(String),
}

/// A client bound to one service endpoint.
#[derive(Clone)]
pub struct Client {
    /// Requests that should time out (REST calls).
    http: reqwest::Client,
    /// Long-lived requests (the event stream) without a body timeout.
    stream_http: reqwest::Client,
    base: String,
    password: Secret,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base)
            .field("password", &self.password)
            .finish()
    }
}

impl Client {
    pub fn new(registration: &Registration) -> Result<Self, ApiError> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;
        let stream_http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;
        Ok(Self {
            http,
            stream_http,
            base: registration.url.trim_end_matches('/').to_string(),
            password: Secret(registration.password.expose().to_string()),
        })
    }

    #[cfg(test)]
    pub fn base(&self) -> &str {
        &self.base
    }

    fn auth_header(&self) -> HeaderValue {
        let value = B64.encode(format!("opencode:{}", self.password.expose()));
        HeaderValue::from_str(&format!("Basic {value}")).expect("basic auth is valid header text")
    }

    async fn json_call(&self, path: &str, body: Option<Value>) -> Result<Json, ApiError> {
        let url = format!("{base}{path}", base = self.base, path = path);
        let fut = match body {
            Some(body) => self
                .http
                .post(url)
                .timeout(Duration::from_secs(60))
                .header(AUTHORIZATION, self.auth_header())
                .json(&body)
                .send(),
            None => self
                .http
                .get(url)
                .timeout(Duration::from_secs(60))
                .header(AUTHORIZATION, self.auth_header())
                .send(),
        };
        let resp = fut
            .await
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            if let Ok(parsed) = serde_json::from_str::<Value>(&text)
                && parsed.get("_tag").and_then(|t| t.as_str()) == Some("SessionBusyError") {
                    return Err(ApiError::Busy);
                }
            return Err(ApiError::Http {
                status: status.as_u16(),
                message: super::truncate(&text, 300),
            });
        }
        resp.json::<Value>()
            .await
            .map_err(|e| ApiError::Parse(e.to_string()))
    }

    // ---- resources -------------------------------------------------------

    pub async fn info(&self) -> Result<ServerInfo, ApiError> {
        let v = self.json_call("/api/info", None).await?;
        serde_json::from_value(v).map_err(|e| ApiError::Parse(e.to_string()))
    }

    pub async fn create_session(
        &self,
        title: Option<&str>,
        directory: &str,
    ) -> Result<SessionInfo, ApiError> {
        let mut body = json!({ "location": { "directory": directory } });
        if let Some(title) = title {
            body["title"] = json!(title);
        }
        let v = self.json_call("/api/session", Some(body)).await?;
        session_from_value(&v)
    }

    pub async fn list_sessions(&self, directory: Option<&str>) -> Result<Vec<SessionInfo>, ApiError> {
        let url = format!("{base}/api/session", base = self.base);
        let mut req = self
            .http
            .get(url)
            .header(AUTHORIZATION, self.auth_header())
            .query(&[
                ("order", "desc"),
                ("archived", "false"),
                ("limit", "30"),
            ]);
        if let Some(directory) = directory {
            req = req.query(&[("directory", directory)]);
        }
        let resp = req.send().await.map_err(|e| ApiError::Unreachable(e.to_string()))?;
        let v = self.check(resp).await?;
        let items = data_array(&v);
        let mut out = Vec::new();
        for item in items {
            out.push(session_from_value(item)?);
        }
        Ok(out)
    }

    pub async fn get_session(&self, id: &str) -> Result<SessionInfo, ApiError> {
        let v = self
            .json_call(&format!("/api/session/{id}"), None)
            .await?;
        session_from_value(&v)
    }

    /// All messages of a session, oldest first, following the cursor.
    pub async fn messages(&self, id: &str) -> Result<Vec<Json>, ApiError> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..200 {
            let mut req = self
                .http
                .get(format!("{base}/api/session/{id}/message", base = self.base))
                .header(AUTHORIZATION, self.auth_header())
                .query(&[("order", "asc"), ("limit", "100")]);
            if let Some(cursor) = cursor.take() {
                req = req.query(&[("cursor", cursor)]);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| ApiError::Unreachable(e.to_string()))?;
            let v = self.check(resp).await?;
            out.extend(data_array(&v).into_iter().cloned());
            cursor = v
                .pointer("/cursor/next")
                .and_then(|c| c.as_str().map(String::from));
            if cursor.is_none() {
                break;
            }
        }
        Ok(out)
    }

    pub async fn message_info(&self, id: &str, message_id: &str) -> Result<Json, ApiError> {
        self.json_call(
            &format!("/api/session/{id}/message/{message_id}"),
            None,
        )
        .await
    }

    #[cfg(test)]
    pub async fn prompt(
        &self,
        id: &str,
        text: &str,
        files: &[FileAttachment],
    ) -> Result<Json, ApiError> {
        let body = json!({ "text": text, "files": files });
        self.json_call(&format!("/api/session/{id}/prompt"), Some(body))
            .await
    }

    /// Like [`Client::prompt`], but without a body timeout: the prompt POST
    /// may block for the whole turn.
    pub async fn prompt_no_timeout(
        &self,
        id: &str,
        text: &str,
        files: &[FileAttachment],
    ) -> Result<Json, ApiError> {
        let url = format!("{base}/api/session/{id}/prompt", base = self.base);
        let resp = self
            .http
            .post(url)
            .header(AUTHORIZATION, self.auth_header())
            .json(&json!({ "text": text, "files": files }))
            .send()
            .await
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            if serde_json::from_str::<Value>(&text)
                .map(|v| v.get("_tag").and_then(Value::as_str) == Some("SessionBusyError"))
                .unwrap_or(false)
            {
                return Err(ApiError::Busy);
            }
            return Err(ApiError::Http {
                status: status.as_u16(),
                message: super::truncate(&text, 300),
            });
        }
        serde_json::from_str(&text).map_err(|e| ApiError::Parse(e.to_string()))
    }

    /// Whether a model accepts image input, per `GET /api/model`. Unknown
    /// models are assumed capable (the server surfaces its own errors).
    pub async fn model_supports_image(&self, model: &ModelRef) -> Result<bool, ApiError> {
        let v = self.json_call("/api/model", None).await?;
        for item in data_array(&v) {
            let id = item.get("modelID").or_else(|| item.get("id")).and_then(Value::as_str);
            let provider = item
                .get("providerID")
                .and_then(Value::as_str);
            if id == Some(model.id.as_str()) && provider == Some(model.provider_id.as_str()) {
                return Ok(item
                    .pointer("/capabilities/input")
                    .and_then(Value::as_array)
                    .map(|inputs| {
                        inputs
                            .iter()
                            .filter_map(Value::as_str)
                            .any(|s| s.eq_ignore_ascii_case("image"))
                    })
                    .unwrap_or(true));
            }
        }
        Ok(true)
    }

    pub async fn interrupt(&self, id: &str) -> Result<bool, ApiError> {
        let v = self
            .json_call(&format!("/api/session/{id}/interrupt"), Some(json!({})))
            .await?;
        Ok(v.get("interrupted").and_then(Value::as_bool).unwrap_or(true))
    }

    pub async fn set_model(&self, id: &str, model: &ModelRef) -> Result<(), ApiError> {
        self.json_call(
            &format!("/api/session/{id}/model"),
            Some(json!({ "model": model })),
        )
        .await?;
        Ok(())
    }

    /// The user's default model per OpenCode configuration, if exposed.
    pub async fn default_model(&self) -> Result<Option<ModelRef>, ApiError> {
        let v = self.json_call("/api/model/default", None).await?;
        Ok(extract_model_ref(&v))
    }

    pub async fn reply_permission(
        &self,
        id: &str,
        request_id: &str,
        decision: &str,
        message: Option<&str>,
    ) -> Result<(), ApiError> {
        let mut body = json!({ "decision": decision });
        if let Some(message) = message {
            body["message"] = json!(message);
        }
        self.json_call(
            &format!("/api/session/{id}/permission/{request_id}/reply"),
            Some(body),
        )
        .await?;
        Ok(())
    }

    async fn check(&self, resp: reqwest::Response) -> Result<Json, ApiError> {
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            if let Ok(parsed) = serde_json::from_str::<Value>(&text)
                && parsed.get("_tag").and_then(Value::as_str) == Some("SessionBusyError")
            {
                return Err(ApiError::Busy);
            }
            return Err(ApiError::Http {
                status: status.as_u16(),
                message: super::truncate(&text, 300),
            });
        }
        resp.json::<Value>()
            .await
            .map_err(|e| ApiError::Parse(e.to_string()))
    }

    /// Long-lived event stream response; the caller extracts the byte stream.
    /// Long-lived event stream response; the caller extracts the byte stream.
    /// `types` takes one parameter per event type.
    pub async fn raw_event_response_params(
        &self,
        types: &[(&str, &str)],
    ) -> Result<reqwest::Response, ApiError> {
        let url = format!("{base}/api/event", base = self.base);
        let mut req = self
            .stream_http
            .get(url)
            .header(AUTHORIZATION, self.auth_header());
        if !types.is_empty() {
            req = req.query(types);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ApiError::Unreachable(e.to_string()))?;
        resp.error_for_status()
            .map_err(|e| ApiError::Unreachable(e.to_string()))
    }
}

/// Find a `{id, providerID}` model pointer somewhere in a response.
pub(crate) fn extract_model_ref(v: &Json) -> Option<ModelRef> {
    for candidate in [
        v,
        v.get("data").unwrap_or(&Value::Null),
        v.pointer("/defaultModel").unwrap_or(&Value::Null),
        v.pointer("/data/model").unwrap_or(&Value::Null),
        v.pointer("/data/defaultModel").unwrap_or(&Value::Null),
        v.pointer("/model").unwrap_or(&Value::Null),
    ] {
        if let Ok(m) = serde_json::from_value::<ModelRef>(candidate.clone()) {
            return Some(m);
        }
    }
    None
}

fn session_from_value(v: &Json) -> Result<SessionInfo, ApiError> {
    // The create endpoint may return `{ data: {...} }` or the object itself.
    let inner = v.get("data").unwrap_or(v);
    serde_json::from_value(inner.clone()).map_err(|e| ApiError::Parse(e.to_string()))
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
