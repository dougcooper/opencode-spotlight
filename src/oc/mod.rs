//! Common client types shared across the OpenCode service layer.

pub mod events;
pub mod http;
pub mod service;

use serde::{Deserialize, Serialize};

pub type Json = serde_json::Value;

/// A model pointer as the OpenCode API expects: `{id, providerID}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRef {
    pub id: String,
    #[serde(rename = "providerID")]
    pub provider_id: String,
}

/// A prompt file attachment: inline data for pasted images.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileAttachment {
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SourceKind {
    /// The bytes are inside `uri` (a data: URI).
    Inline,
}

impl FileAttachment {
    /// An image attachment whose bytes are encoded into a data URI.
    pub fn inline_data(data_uri: String, display_name: Option<String>) -> Self {
        Self {
            uri: data_uri,
            source: Some(SourceKind::Inline),
            name: display_name,
        }
    }
}

/// A pending permission request for a session.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PermReq {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "sessionID", default)]
    pub session_id: String,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub resources: Vec<String>,
    #[serde(default)]
    pub save: Vec<String>,
    #[serde(default)]
    pub message: Option<String>,
}

/// Snapshot of a session as returned by the list endpoints. Parsed leniently.
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default)]
pub struct SessionInfo {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model: Option<ModelRef>,
    #[serde(default)]
    pub time: Option<SessionTime>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default)]
pub struct SessionTime {
    #[serde(default)]
    pub created: f64,
    #[serde(default)]
    pub updated: f64,
}

/// Pull an array of objects out of `{ data: [...] }` or a bare array.
pub(crate) fn data_array(v: &Json) -> Vec<&Json> {
    if let Some(arr) = v.as_array() {
        return arr.iter().collect();
    }
    v.get("data")
        .and_then(|d| d.as_array())
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

/// Best-effort truncation for detail previews.
pub(crate) fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let cut: String = text.chars().take(max).collect();
        format!("{cut}…")
    }
}
