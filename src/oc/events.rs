//! The OpenCode event stream (`GET /api/event`, an SSE stream) parsed into
//! the small subset the overlay cares about.

use eventsource_stream::Eventsource;
use futures_util::{Stream, StreamExt};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;

use super::http::Client;
use super::truncate;
use super::{Json, PermReq};

/// Event types the overlay subscribes to. `server.connected` is always
/// delivered first by the server.
pub const EVENT_TYPES: &[&str] = &[
    "session.text.delta",
    "session.reasoning.delta",
    "session.tool.called",
    "session.tool.progress",
    "session.tool.success",
    "session.tool.failed",
    "session.execution.started",
    "session.execution.succeeded",
    "session.execution.failed",
    "session.execution.interrupted",
    "session.idle",
    "session.usage.updated",
    "permission.asked",
    "permission.replied",
    "session.deleted",
];

/// One decoded service event.
#[derive(Debug, Clone, PartialEq)]
pub enum BackEvent {
    TextDelta { session_id: String, message_id: String, delta: String },
    ReasoningDelta { session_id: String, message_id: String, delta: String },
    Tool {
        session_id: String,
        message_id: String,
        tool_id: String,
        name: Option<String>,
        state: Option<String>,
        detail: Option<String>,
    },
    ContentUpdated { session_id: String, message_id: String, content: Json },
    ExecStarted { session_id: String },
    ExecEnded { session_id: String },
    Idle { session_id: String },
    Usage { session_id: String, tokens: u64, cost: f64 },
    Permission(PermReq),
    PermissionReplied { id: String },
    SessionDeleted { session_id: String },
    ServerConnected,
    Other,
}

/// Parse one SSE `data:` payload.
pub fn parse_envelope(data: &str) -> BackEvent {
    let Ok(v) = serde_json::from_str::<Value>(data) else {
        return BackEvent::Other;
    };
    let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
    let d = v.get("data").cloned().unwrap_or(Value::Null);
    match kind {
        "session.text.delta" => {
            let session_id = d.get("sessionID").and_then(Value::as_str).unwrap_or("").into();
            let message_id = d.get("assistantMessageID").and_then(Value::as_str).unwrap_or("").into();
            let delta = d.get("delta").and_then(Value::as_str).unwrap_or("").into();
            BackEvent::TextDelta { session_id, message_id, delta }
        }
        "session.reasoning.delta" => {
            let session_id = d.get("sessionID").and_then(Value::as_str).unwrap_or("").into();
            let message_id = d.get("assistantMessageID").and_then(Value::as_str).unwrap_or("").into();
            let delta = d.get("delta").and_then(Value::as_str).unwrap_or("").into();
            BackEvent::ReasoningDelta { session_id, message_id, delta }
        }
        "session.tool.called" => {
            let detail = d
                .pointer("/input/command")
                .and_then(Value::as_str)
                .map(|c| truncate(c, 120));
            tool_event(d, "called", None, detail)
        }
        "session.tool.progress" => tool_event(d, "running", None, None),
        "session.tool.success" => {
            let detail = d.pointer("/content/0/text").and_then(Value::as_str).map(|c| truncate(c, 120));
            tool_event(d, "success", None, detail)
        }
        "session.tool.failed" => tool_event(d, "failed", None, None),
        "session.message.content.updated" => {
            let session_id = d.get("sessionID").and_then(Value::as_str).unwrap_or("").into();
            let message_id = d.get("messageID").and_then(Value::as_str).unwrap_or("").into();
            let content = d.get("content").cloned().unwrap_or(Value::Null);
            BackEvent::ContentUpdated { session_id, message_id, content }
        }
        "session.execution.started" => BackEvent::ExecStarted { session_id: id_of(&d) },
        "session.execution.succeeded" | "session.execution.failed" | "session.execution.interrupted" => {
            BackEvent::ExecEnded { session_id: id_of(&d) }
        }
        "session.idle" => BackEvent::Idle { session_id: id_of(&d) },
        "session.usage.updated" => {
            let from = d.get("source").and_then(Value::as_str);
            if from.is_none() || from == Some("generate") {
                let session_id = d.get("sessionID").and_then(Value::as_str).unwrap_or("").into();
                let tokens = token_count(&d);
                let cost = d.get("cost").and_then(Value::as_f64).unwrap_or(0.0);
                BackEvent::Usage { session_id, tokens, cost }
            } else {
                BackEvent::Other
            }
        }
        "permission.asked" => {
            let perm: PermReq =
                serde_json::from_value(d).unwrap_or_default();
            BackEvent::Permission(perm)
        }
        "permission.replied" => BackEvent::PermissionReplied {
            id: v
                .pointer("/data/id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
        },
        "session.deleted" => BackEvent::SessionDeleted { session_id: id_of(&d) },
        "server.connected" => BackEvent::ServerConnected,
        _ => BackEvent::Other,
    }
}

fn id_of(d: &Value) -> String {
    d.get("sessionID").and_then(Value::as_str).unwrap_or("").into()
}

fn tool_event(
    d: Value,
    state: &str,
    name: Option<String>,
    detail: Option<String>,
) -> BackEvent {
    BackEvent::Tool {
        session_id: id_of(&d),
        message_id: d.get("assistantMessageID").and_then(Value::as_str).unwrap_or("").into(),
        tool_id: d.get("id").and_then(Value::as_str).unwrap_or("").into(),
        name,
        state: Some(state.into()),
        detail,
    }
}

fn token_count(d: &Value) -> u64 {
    let tokens = d.get("tokens").unwrap_or(&Value::Null);
    if let Some(n) = tokens.as_u64() {
        return n;
    }
    let add = |key: &str| tokens.pointer(key).and_then(Value::as_u64).unwrap_or(0);
    add("/input") + add("/output")
}

/// Subscribe to the service event stream, returning decoded events.
pub async fn subscribe(
    client: &Client,
) -> Result<
    std::pin::Pin<Box<dyn Stream<Item = Result<BackEvent, String>> + Send>>,
    super::http::ApiError,
> {
    // The API takes one `types=` parameter per event type, not a list.
    let query: Vec<(&str, &str)> = EVENT_TYPES.iter().map(|t| ("types", *t)).collect();
    let resp = client.raw_event_response_params(&query).await?;
    let stream = resp
        .bytes_stream()
        .eventsource()
        .map(|item| match item {
            Ok(ev) => Ok(parse_envelope(&ev.data)),
            Err(e) => Err(e.to_string()),
        });
    Ok(Box::pin(stream))
}

/// Assistant tool parts as they appear inside message content arrays.
pub(crate) fn tool_parts_from_content(content: &Value) -> Vec<(String, Option<String>, Option<String>)> {
    let mut out = Vec::new();
    if let Some(items) = content.as_array() {
        for part in items {
            if part.get("type").and_then(Value::as_str) == Some("tool") {
                let id = part.get("id").and_then(Value::as_str).unwrap_or("").into();
                let name = part.get("name").and_then(Value::as_str).map(String::from);
                let state = part
                    .pointer("/state/status")
                    .and_then(Value::as_str)
                    .map(String::from);
                out.push((id, name, state));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_delta() {
        let payload = json!({
            "id": "e1", "created": 1.0,
            "type": "session.text.delta",
            "data": { "sessionID": "s1", "assistantMessageID": "m1", "ordinal": 0, "delta": "Hello" }
        })
        .to_string();
        assert_eq!(
            parse_envelope(&payload),
            BackEvent::TextDelta { session_id: "s1".into(), message_id: "m1".into(), delta: "Hello".into() }
        );
    }

    #[test]
    fn parses_permission_ask() {
        let payload = json!({
            "id": "e2", "created": 1.0,
            "type": "permission.asked",
            "data": {
                "id": "p1", "sessionID": "s1", "action": "shell",
                "resources": ["git push *"], "save": ["shell: git push *"],
                "message": "wants to run a command"
            }
        })
        .to_string();
        match parse_envelope(&payload) {
            BackEvent::Permission(p) => {
                assert_eq!(p.id, "p1");
                assert_eq!(p.action, "shell");
                assert_eq!(p.resources, vec!["git push *".to_string()]);
            }
            other => panic!("wrong event: {other:?}"),
        }
    }

    #[test]
    fn parses_tool_events() {
        let payload = json!({
            "id": "e3", "created": 1.0,
            "type": "session.tool.called",
            "data": {
                "sessionID": "s1", "assistantMessageID": "m1",
                "id": "tool1", "input": { "command": "ls -la" }, "executed": false
            }
        })
        .to_string();
        match parse_envelope(&payload) {
            BackEvent::Tool { tool_id, state, detail, .. } => {
                assert_eq!(tool_id, "tool1");
                assert_eq!(state.as_deref(), Some("called"));
                assert_eq!(detail.as_deref(), Some("ls -la"));
            }
            other => panic!("wrong event: {other:?}"),
        }
    }

    #[test]
    fn parses_usage_events() {
        // usage.updated without a source is accepted.
        let payload = json!({
            "id": "e4", "created": 1.0,
            "type": "session.usage.updated",
            "data": { "sessionID": "s1", "cost": 0.01, "tokens": { "input": 100, "output": 50 } }
        })
        .to_string();
        match parse_envelope(&payload) {
            BackEvent::Usage { tokens, cost, .. } => {
                assert_eq!(tokens, 150);
                assert!((cost - 0.01).abs() < 1e-9);
            }
            other => panic!("wrong event: {other:?}"),
        }
    }

    #[test]
    fn tool_parts_extraction() {
        let content = json!([
            { "type": "text", "text": "hi" },
            { "type": "tool", "id": "t1", "name": "shell", "state": { "status": "completed" } }
        ]);
        let parts = tool_parts_from_content(&content);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].0, "t1");
        assert_eq!(parts[0].1.as_deref(), Some("shell"));
        assert_eq!(parts[0].2.as_deref(), Some("completed"));
    }
}
