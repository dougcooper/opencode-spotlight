//! Opt-in debug IPC: a unix-socket command surface that lets tooling drive
//! and inspect the real UI.
//!
//! Active only when `SPOTLIGHT_DEBUG_IPC=1`. Line protocol over
//! `$XDG_RUNTIME_DIR/opencode-spotlight.debug.sock`: `<verb> [payload]`,
//! one JSON object back per line.
//!
//! Verbs: `ping`, `state`, `events [n]`, `transcript`, `send <text>`,
//! `escape`, `paste-png-base64 <data|@file>`, `toggle`.
//! Every verb routes through the same code paths the keyboard and mouse
//! would hit, so assertions made here apply to real behavior.
//!
//! The listener runs on its own thread; verbs execute on the GTK main loop
//! (marshalled through a channel) so gio/WebKit futures can be awaited.

use std::collections::VecDeque;
use std::rc::Rc;

use libadwaita::prelude::*;
use gtk4::glib;
use serde::Serialize;

use crate::oc::service::UiEvent;
use crate::ui::controller::UiContext;

pub const SOCKET_NAME: &str = "opencode-spotlight.debug.sock";
const RING_CAPACITY: usize = 400;
const TRANSCRIPT_JS: &str = r#"
(() => {
  const meta = {
    href: window.location.href,
    readyState: document.readyState,
    bridge: typeof window.__bridge === "object" && window.__bridge !== null,
  };
  const root = document.getElementById("transcript");
  if (!root) {
    return JSON.stringify({ window: false, ...meta, count: 0, messages: [] });
  }
  const out = [];
  for (const node of root.children) {
    if (node.classList.contains("msg")) {
      out.push({
        kind: node.classList.contains("user") ? "user" : "assistant",
        text: node.textContent,
        codeBlocks: node.querySelectorAll("pre code").length,
        copyButtons: node.querySelectorAll(".copy-btn").length,
        images: node.querySelectorAll("img").length,
      });
    } else {
      out.push({ kind: node.className, text: node.innerText });
    }
  }
  return JSON.stringify({ window: true, ...meta, count: out.length, messages: out });
})()
"#;

/// Ring of observed UI events (always collected; cheap).
#[derive(Default)]
pub struct EventRing {
    entries: std::sync::Mutex<VecDeque<String>>,
}

impl EventRing {
    pub fn push(&self, entry: String) {
        let mut ring = self.entries.lock().expect("ring");
        if ring.len() >= RING_CAPACITY {
            ring.pop_front();
        }
        ring.push_back(entry);
    }

    pub fn last(&self, n: usize) -> Vec<String> {
        let ring = self.entries.lock().expect("ring");
        let skip = ring.len().saturating_sub(n);
        ring.iter().skip(skip).cloned().collect()
    }
}

static RING: EventRing = EventRing {
    entries: std::sync::Mutex::new(std::collections::VecDeque::new()),
};

/// Record one handled UI event into the ring (called from the main loop).
pub fn record_event(event: &UiEvent) {
    let summary = match event {
        UiEvent::Conn(c) => format!("conn {c:?}"),
        UiEvent::Busy(b) => format!("busy {b}"),
        UiEvent::Usage { tokens, cost } => format!("usage {tokens}/{cost}"),
        UiEvent::Perm(p) => format!("perm {} ({})", p.id, p.action),
        UiEvent::PermReplied { id } => format!("perm-replied {id}"),
        UiEvent::Bridge(p) => format!("bridge {:?}", p),
        UiEvent::PromptQueued => "prompt-queued".into(),
        UiEvent::Error(m) => format!("error {m}"),
    };
    RING.push(summary);
}

/// Attach the context and start the socket listener thread.
pub fn activate(context: Rc<UiContext>) {
    let (cmd_tx, cmd_rx) =
        async_channel::bounded::<(String, std::sync::mpsc::Sender<String>)>(64);

    // Main-loop worker: owns the UI handle and runs verbs where gio futures
    // are safe to await.
    glib::spawn_future_local(async move {
        while let Ok((line, reply)) = cmd_rx.recv().await {
            let response = execute_on_main(&context, line).await;
            let _ = reply.send(response);
        }
    });

    // Listener thread: accepts connections, forwards to the main loop.
    std::thread::Builder::new()
        .name("spotlight-debug-ipc".into())
        .spawn(move || listener_loop(cmd_tx))
        .expect("spawn debug listener");
}

fn socket_path() -> String {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    format!("{runtime_dir}/{SOCKET_NAME}")
}

fn listener_loop(cmd_tx: async_channel::Sender<(String, std::sync::mpsc::Sender<String>)>) {
    use std::io::Write;
    let path = socket_path();
    let _ = std::fs::remove_file(&path);
    let listener = match std::os::unix::net::UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("opencode-spotlight: debug listener bind failed: {e}");
            return;
        }
    };
    eprintln!("opencode-spotlight: debug ipc listening on {path}");
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let Ok(clone) = stream.try_clone() else {
            continue;
        };
        let mut reader = std::io::BufReader::new(clone);
        let mut line = String::new();
        if std::io::BufRead::read_line(&mut reader, &mut line).is_err() {
            continue;
        }
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        if cmd_tx.send_blocking((line.trim().to_string(), reply_tx)).is_err() {
            continue;
        }
        if let Ok(response) = reply_rx.recv() {
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(b"\n");
            let _ = stream.flush();
        }
    }
}

#[derive(Serialize)]
struct DebugStateView {
    window_exists: bool,
    window_visible: bool,
    expanded: bool,
    size: (i32, i32),
    busy: bool,
    connection: String,
    input_text: String,
    attachments: usize,
    model_chip: String,
}

async fn execute_on_main(context: &Rc<UiContext>, line: String) -> String {
    let (verb, rest) = match line.split_once(' ') {
        Some((verb, rest)) => (verb, rest.trim().to_string()),
        None => (line.as_str(), String::new()),
    };
    match verb {
        "ping" => r#"{"ok":true}"#.into(),
        "events" => {
            let n: usize = rest.parse().unwrap_or(40);
            let entries = RING.last(n);
            format!(
                "{{\"events\":{}}}",
                serde_json::to_string(&entries).unwrap_or_else(|_| "[]".into())
            )
        }
        "state" => state_view(context).await,
        "transcript" => transcript_view(context).await,
        "send" => {
            context.debug_send(&rest);
            r#"{"ok":true,"sent":true}"#.into()
        }
        "escape" => {
            context.hide_overlay();
            r#"{"ok":true,"escaped":true}"#.into()
        }
        "model" => {
            context.toggle_model_chip();
            r#"{"ok":true,"toggled_model":true}"#.into()
        }
        "newchat" => {
            context.new_chat();
            r#"{"ok":true,"new_chat":true}"#.into()
        }
        "eval" => {
            let Some(window) = context.window_take() else {
                return "{\"error\":\"no window\"}".into();
            };
            let script = glib::GString::from(format!(
                "(()=>{{ try {{ return JSON.stringify(eval({rest:?})); }} catch (e) {{ return JSON.stringify({{jserror: String(e)}}); }} }})()"
            ));
            match webkit6::prelude::WebViewExt::evaluate_javascript_future(
                &window.transcript.webview,
                &script,
                None,
                None,
            )
            .await
            {
                Ok(value) => value.to_str().to_string(),
                Err(e) => format!("{{\"error\":\"js: {e}\"}}"),
            }
        }
        "toggle" => {
            UiContext::show_or_toggle(context, true);
            r#"{"ok":true,"toggled":true}"#.into()
        }
        "paste-png-base64" => {
            let bytes = if let Some(file) = rest.strip_prefix('@') {
                std::fs::read(file).unwrap_or_default()
            } else {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD
                    .decode(rest.replace('\n', ""))
                    .unwrap_or_default()
            };
            if bytes.is_empty() {
                return r#"{"ok":false,"error":"no png bytes"}"#.into();
            }
            context.debug_paste_png(bytes);
            r#"{"ok":true,"staged":true}"#.into()
        }
        other => format!("{{\"ok\":false,\"error\":\"unknown verb '{other}'\"}}"),
    }
}

async fn state_view(context: &Rc<UiContext>) -> String {
    let Some(window) = context.window_take() else {
        let empty = DebugStateView {
            window_exists: false,
            window_visible: false,
            expanded: false,
            size: (0, 0),
            busy: context.busy(),
            connection: context
                .connection()
                .map(|c| format!("{c:?}"))
                .unwrap_or_else(|| "unknown".into()),
            input_text: String::new(),
            attachments: 0,
            model_chip: String::new(),
        };
        return serde_json::to_string(&empty).expect("serialize");
    };
    let view = DebugStateView {
        window_exists: true,
        window_visible: window.is_visible(),
        expanded: window.is_expanded(),
        size: window.size(),
        busy: context.busy(),
        connection: context
            .connection()
            .map(|c| format!("{c:?}"))
            .unwrap_or_else(|| "unknown".into()),
        input_text: window.input.text(),
        attachments: window.attachments.len(),
        model_chip: window
            .input
            .model_button
            .label()
            .unwrap_or_default()
            .to_string(),
    };
    serde_json::to_string(&view).expect("serialize")
}

async fn transcript_view(context: &Rc<UiContext>) -> String {
    let Some(window) = context.window_take() else {
        return "{\"error\":\"no window\"}".into();
    };
    let webview = &window.transcript.webview;
    let script = glib::GString::from(TRANSCRIPT_JS);
    match webkit6::prelude::WebViewExt::evaluate_javascript_future(
        webview,
        &script,
        None,
        None,
    )
    .await
    {
        Ok(value) => value.to_str().to_string(),
        Err(e) => format!("{{\"error\":\"js: {e}\"}}"),
    }
}
