//! The Rust↔JS bridge protocol for the transcript webview.
//!
//! `BridgeIn` messages are serialized with serde and evaluated in the page as
//! `window.__bridge.receive(<json>)`. `BridgeOut` messages arrive from the
//! page as a JSON string posted to the `bridge` script-message handler.
//! No service credential ever crosses this boundary.

use serde::{Deserialize, Serialize};

/// Messages from the webview page to the host.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "t", rename_all = "kebab-case")]
pub enum BridgeOut {
    /// Copy the given text to the GTK clipboard (the page has no direct
    /// clipboard access; the host performs the write).
    Copy { text: String },
}

/// One fenced code block in a message.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolInfo {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A message in a history snapshot (`Init`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum MsgBody {
    User {
        id: String,
        text: String,
        #[serde(default)]
        images: Vec<String>,
    },
    Assistant {
        id: String,
        text: String,
        #[serde(default)]
        reasoning: String,
        #[serde(default)]
        tools: Vec<ToolInfo>,
    },
}

/// Messages from the host to the webview page.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "t", rename_all = "kebab-case")]
pub enum BridgeIn {
    /// Switch the page color scheme.
    Theme { dark: bool },
    /// Replace the whole transcript (history load or resync).
    Init { messages: Vec<MsgBody> },
    /// Append a user message.
    User { id: String, text: String, images: Vec<String> },
    /// Append a text delta to the message.
    TextDelta { message_id: String, delta: String },
    /// Append a reasoning ("thinking") delta to the message.
    ReasoningDelta { message_id: String, delta: String },
    /// Register or update a tool call line.
    Tool { message_id: String, tool_id: String, name: Option<String>, state: String, detail: Option<String> },
    /// Authoritative final content for a message (replaces deltas).
    ContentFull { message_id: String, text: String, reasoning: String, tools: Vec<ToolInfo> },
    /// Show a transient system line.
    Note { text: String },
    /// Clear the transcript (new chat).
    Reset,
}
