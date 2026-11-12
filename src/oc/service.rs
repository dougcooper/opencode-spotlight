//! The backend runtime: keeps a connection to the OpenCode background
//! service, resolves and remembers the scratch session, forwards commands
//! from the UI, and streams events to the GTK side.
//!
//! Runs on its own thread inside a dedicated tokio runtime; talks to the GTK
//! main loop through two async channels.

use std::collections::VecDeque;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::Value;
use tokio::time::sleep;

use crate::bridge::{BridgeIn, MsgBody, ToolInfo};
use crate::oc::events::{tool_parts_from_content, subscribe, BackEvent};
use crate::oc::http::{ensure_connection, ApiError, Client, Registration};
use crate::oc::{truncate, FileAttachment, Json, ModelRef, SessionInfo};
use crate::state::{AppState, Config, Paths};

/// What the GTK side asks the backend to do.
#[derive(Debug, Clone)]
pub enum Cmd {
    Send { text: String, images: Vec<ImageDto> },
    NewChat,
    Interrupt,
    SwitchModel(ModelRef),
    ReplyPermission { request_id: String, decision: String, message: Option<String> },
    Handoff,
    RetryConnect,
}

/// An image staged for sending; `data_uri` carries the encoded bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageDto {
    pub data_uri: String,
    pub name: String,
}

/// Connection state for the status row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnState {
    Connecting,
    Connected,
    Unreachable(String),
}

/// What the backend tells the GTK side.
#[derive(Debug, Clone)]
pub enum UiEvent {
    Conn(ConnState),
    Busy(bool),
    Usage { tokens: u64, cost: f64 },
    Perm(crate::oc::PermReq),
    PermReplied { id: String },
    Bridge(BridgeIn),
    Error(String),
    /// The message was accepted by the backend; clear the input now.
    PromptQueued,
    /// The portal-registered global hotkey fired.
    ToggleHotkey,
}

/// Tracks the session's current model and the image-flow auto-switch.
struct ImageFlow {
    current: Option<ModelRef>,
    restore: Option<ModelRef>,
}

struct Deps {
    paths: Paths,
    config: Config,
    cmd_rx: async_channel::Receiver<Cmd>,
    ui_tx: async_channel::Sender<UiEvent>,
}

impl Deps {
    fn scratch(&self) -> std::path::PathBuf {
        self.config.scratch_dir(&self.paths)
    }
}

/// Start the backend thread; returns the command sender and event receiver
/// for the GTK side.
pub fn start(
    paths: Paths,
    config: Config,
    cmd_rx: async_channel::Receiver<Cmd>,
    ui_tx: async_channel::Sender<UiEvent>,
) {
    std::thread::Builder::new()
        .name("spotlight-backend".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(run(Deps { paths, config, cmd_rx, ui_tx }));
        })
        .expect("spawn backend thread");
}

#[allow(unused_assignments)]
async fn run(deps: Deps) {
    // Bind the global hotkey via the xdg-desktop-portal (GNOME 45+). The
    // desktop's custom-shortcut registry is dead on GNOME 50; the portal is
    // the supported path. Runs independently of the connection loop.
    {
        let ui_tx = deps.ui_tx.clone();
        let trigger = deps.config.keybinding.clone();
        tokio::spawn(async move {
            if let Err(why) =
                crate::oc::hotkey_portal::run(ui_tx, trigger).await
            {
                eprintln!("opencode-spotlight: portal hotkey failed: {why}");
            }
        });
    }
    let mut pending: VecDeque<Cmd> = VecDeque::new();
    #[allow(unused_assignments)] // set again after each ensure()
    let mut client: Option<Client> = None;
    let mut session: Option<SessionInfo> = None;
    let mut resync;
    let mut image_flow = ImageFlow {
        current: None,
        restore: None,
    };
    resync = false;

    loop {
        let _ = deps.ui_tx.send(UiEvent::Conn(ConnState::Connecting)).await;

        match ensure(&deps).await {
            Ok(c) => {
                client = Some(c);

                if !resync {
                    session = resolve_session(&deps, client.as_ref().unwrap()).await;
                } else if let Some(id) = session.as_ref().map(|s| s.id.clone()) {
                    session = match client.as_ref().unwrap().get_session(&id).await {
                        Ok(info) => Some(info),
                        Err(_) => resolve_session(&deps, client.as_ref().unwrap()).await,
                    };
                }
                resync = false;

                // Flush buffered commands right after (re)connection so a
                // message typed while the service was down goes out now.
                while let Some(cmd) = pending.pop_front() {
                    feed(
                        client.as_ref().unwrap(),
                        &mut session,
                        cmd,
                        &deps,
                        &mut image_flow,
                    )
                    .await;
                }

                if let Some(info) = session.as_ref() {
                    load_history(client.as_ref().unwrap(), &info.id, &deps.ui_tx).await;
                } else {
                    let _ = deps.ui_tx.send(UiEvent::Bridge(BridgeIn::Reset)).await;
                }
                let _ = deps.ui_tx.send(UiEvent::Conn(ConnState::Connected)).await;
                let _ = deps.ui_tx.send(UiEvent::Busy(false)).await;

                match connection_loop(
                    client.as_ref().unwrap(),
                    session.take(),
                    &deps,
                    &mut image_flow,
                )
                .await
                {
                    LoopEnd::Reconnect(kept) => {
                        session = kept;
                        resync = true;
                    }
                }
            }
            Err(why) => {
                let _ = deps
                    .ui_tx
                    .send(UiEvent::Conn(ConnState::Unreachable(why.to_string())))
                    .await;
                'wait: loop {
                    tokio::select! {
                        _ = sleep(Duration::from_secs(5)) => break 'wait,
                        cmd = deps.cmd_rx.recv() => match cmd {
                            Ok(Cmd::RetryConnect) => break 'wait,
                            Ok(cmd) => pending.push_back(cmd),
                            Err(_) => return, // GTK side is gone
                        }
                    }
                }
            }
        }
    }
}

async fn ensure(_deps: &Deps) -> Result<Client, ApiError> {
    let reg_path = Registration::default_path().expect("XDG state dir must be resolvable");
    ensure_connection(&reg_path, spawn_service)
        .await
        .map_err(|e| ApiError::Unreachable(e.to_string()))
}

/// Detached start of the shared OpenCode background service, mirroring what
/// the official client's `Service.ensure()` runs (`opencode serve --service`).
fn spawn_service() {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let result = Command::new("opencode")
        .args(["serve", "--service"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    if let Ok(child) = result {
        tokio::spawn(async move {
            tokio::task::spawn_blocking(move || {
                let mut child = child;
                let _ = child.wait();
            })
            .await
            .ok();
        });
    }
}

enum LoopEnd {
    Reconnect(Option<SessionInfo>),
}

async fn connection_loop(
    client: &Client,
    session: Option<SessionInfo>,
    deps: &Deps,
    image_flow: &mut ImageFlow,
) -> LoopEnd {
    image_flow.current = session
        .as_ref()
        .and_then(|s| s.model.clone())
        .or_else(|| image_flow.current.clone());
    let mut stream = match subscribe(client).await {
        Ok(s) => s,
        Err(e) => {
            let _ = deps.ui_tx.send(UiEvent::Error(e.to_string())).await;
            return LoopEnd::Reconnect(session);
        }
    };

    let mut session = session;
    let mut last_assistant_id: Option<String> = None;
    loop {
        tokio::select! {
            biased;

            cmd = deps.cmd_rx.recv() => {
                match cmd {
                    Ok(Cmd::RetryConnect) => return LoopEnd::Reconnect(session),
                    Ok(cmd) => {
                        feed(client, &mut session, cmd, deps, image_flow).await;
                    }
                    Err(_) => return LoopEnd::Reconnect(session), // UI went away; the outer loop retries
                }
            }

            event = stream.next() => {
                match event {
                    None => return LoopEnd::Reconnect(session),
                    Some(Err(e)) => {
                        let _ = deps.ui_tx.send(UiEvent::Bridge(BridgeIn::Note {
                            text: format!("connection interrupted ({e}); reconnecting…"),
                        })).await;
                        return LoopEnd::Reconnect(session);
                    }
                    Some(Ok(ev)) => {
                        let active = session.as_ref().map(|s| s.id.clone()).unwrap_or_default();
                        let is_active = |sid: &str| !active.is_empty() && sid == active;
                        match ev {
                            BackEvent::TextDelta { session_id, message_id, delta } if is_active(&session_id) => {
                                last_assistant_id = Some(message_id.clone());
                                let _ = deps.ui_tx.send(UiEvent::Bridge(BridgeIn::TextDelta { message_id, delta })).await;
                            }
                            BackEvent::ReasoningDelta { session_id, message_id, delta } if is_active(&session_id) => {
                                last_assistant_id = Some(message_id.clone());
                                let _ = deps.ui_tx.send(UiEvent::Bridge(BridgeIn::ReasoningDelta { message_id, delta })).await;
                            }
                            BackEvent::Tool { session_id, message_id, tool_id, name, state, detail } if is_active(&session_id) => {
                                let _ = deps.ui_tx.send(UiEvent::Bridge(BridgeIn::Tool {
                                    message_id,
                                    tool_id,
                                    name,
                                    state: state.unwrap_or_else(|| "unknown".into()),
                                    detail,
                                })).await;
                            }
                            BackEvent::ContentUpdated { session_id, message_id, content } if is_active(&session_id) => {
                                for (tool_id, name, state) in tool_parts_from_content(&content) {
                                    let _ = deps.ui_tx.send(UiEvent::Bridge(BridgeIn::Tool {
                                        message_id: message_id.clone(),
                                        tool_id,
                                        name,
                                        state: state.unwrap_or_else(|| "unknown".into()),
                                        detail: None,
                                    })).await;
                                }
                            }
                            BackEvent::ExecStarted { session_id } if is_active(&session_id) => {
                                let _ = deps.ui_tx.send(UiEvent::Busy(true)).await;
                            }
                            BackEvent::ExecEnded { session_id } | BackEvent::Idle { session_id } if is_active(&session_id) => {
                                let _ = deps.ui_tx.send(UiEvent::Busy(false)).await;
                                if let Some(message_id) = last_assistant_id.take()
                                    && let Ok(info) = client.message_info(&active, &message_id).await
                                        && let Some(BridgeIn::ContentFull { message_id, text, reasoning, tools }) = final_content(&info) {
                                            let _ = deps.ui_tx.send(UiEvent::Bridge(BridgeIn::ContentFull { message_id, text, reasoning, tools })).await;
                                        }
                            }
                            BackEvent::Usage { session_id, tokens, cost } if is_active(&session_id) => {
                                let _ = deps.ui_tx.send(UiEvent::Usage { tokens, cost }).await;
                            }
                            BackEvent::Permission(req) if req.session_id == active => {
                                let _ = deps.ui_tx.send(UiEvent::Perm(req)).await;
                            }
                            BackEvent::PermissionReplied { id } => {
                                let _ = deps.ui_tx.send(UiEvent::PermReplied { id }).await;
                            }
                            BackEvent::SessionDeleted { session_id } if is_active(&session_id) => {
                                let _ = deps.ui_tx.send(UiEvent::Bridge(BridgeIn::Note {
                                    text: "This chat was deleted in another client.".into(),
                                })).await;
                                let mut state = AppState::load(&deps.paths);
                                state.last_session_id = None;
                                state.save(&deps.paths);
                                return LoopEnd::Reconnect(session);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
}

/// Apply a UI command; shared between the connection loop and the post-reconnect flush.
async fn feed(
    client: &Client,
    session: &mut Option<SessionInfo>,
    cmd: Cmd,
    deps: &Deps,
    image_flow: &mut ImageFlow,
) {
    let active = session.as_ref().map(|s| s.id.clone()).unwrap_or_default();
    match cmd {
        Cmd::Send { text, images } => {
            // If there is no session yet, create one now.
            let session_id = if active.is_empty() {
                match create_chat(client, deps).await {
                    Ok(info) => {
                        *session = Some(info.clone());
                        info.id
                    }
                    Err(e) => {
                        let _ = deps
                            .ui_tx
                            .send(UiEvent::Error(format!("Could not start a chat: {e}")))
                            .await;
                        return;
                    }
                }
            } else {
                active
            };

            // Per the user's rule: image messages answer with the quick
            // (flash) model. Switch when needed; restore once idle.
            if !images.is_empty() {
                let quick = deps
                    .config
                    .models
                    .as_ref()
                    .and_then(|m| m.quick.as_ref())
                    .map(|m| m.into());
                if let Some(quick) = quick
                    && image_flow.current.as_ref() != Some(&quick)
                {
                        match client.set_model(&session_id, &quick).await {
                            Ok(()) => {
                                image_flow.restore = image_flow.current.take();
                                image_flow.current = Some(quick.clone());
                                let _ = deps
                                    .ui_tx
                                    .send(UiEvent::Bridge(BridgeIn::Note {
                                        text: "image attached — answering with the quick model".into(),
                                    }))
                                    .await;
                            }
                            Err(e) => {
                                let _ = deps
                                    .ui_tx
                                    .send(UiEvent::Error(format!(
                                        "Could not switch to the quick model for the image: {e}"
                                    )))
                                    .await;
                                return;
                            }
                        }
                }
            }

            // The user bubble shown immediately, before any events arrive.
            let local_id = format!(
                "user-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0)
            );
            let _ = deps
                .ui_tx
                .send(UiEvent::Bridge(BridgeIn::User {
                    id: local_id,
                    text: text.clone(),
                    images: images.iter().map(|i| i.data_uri.clone()).collect(),
                }))
                .await;
            // Input clears as soon as the message is queued.
            let _ = deps.ui_tx.send(UiEvent::PromptQueued).await;

            spawn_prompt(client, &session_id, text, images, deps);
        }
        Cmd::NewChat => match create_chat(client, deps).await {
            Ok(info) => {
                *session = Some(info.clone());
                let _ = deps.ui_tx.send(UiEvent::Bridge(BridgeIn::Reset)).await;
            }
            Err(e) => {
                let _ = deps.ui_tx.send(UiEvent::Error(format!("Could not start a new chat: {e}"))).await;
            }
        },
        Cmd::Handoff => {
            let scratch = deps.scratch();
            let _ = std::fs::create_dir_all(&scratch);
            let session_id = session.as_ref().map(|s| s.id.clone());
            match terminal_argv(deps, &scratch, session_id.as_deref()) {
                Some(argv) => {
                    let (program, args) = argv.split_first().expect("non-empty argv");
                    let result = {
                        use std::os::unix::process::CommandExt;
                        std::process::Command::new(program)
                            .args(args.iter().map(String::as_str))
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .process_group(0)
                            .spawn()
                    };
                    if let Err(e) = result {
                        let _ = deps
                            .ui_tx
                            .send(UiEvent::Error(format!("Could not open the terminal ({program}): {e}")))
                            .await;
                    }
                }
                None => {
                    let _ = deps
                        .ui_tx
                        .send(UiEvent::Error(
                            "No terminal found — set handoff_command in the config.".into(),
                        ))
                        .await;
                }
            }
        }
        Cmd::Interrupt => {
            if active.is_empty() {
                return;
            }
            let client = client.clone();
            let session_id = active.clone();
            tokio::spawn(async move {
                if let Err(e) = client.interrupt(&session_id).await {
                    eprintln!("opencode-spotlight: interrupt failed: {e}");
                }
            });
        }
        Cmd::SwitchModel(model) => {
            if active.is_empty() {
                return;
            }
            let client = client.clone();
            let session_id = active.clone();
            let ui_tx = deps.ui_tx.clone();
            tokio::spawn(async move {
                if let Err(e) = client.set_model(&session_id, &model).await {
                    let _ = ui_tx.send(UiEvent::Error(format!("Model switch failed: {e}"))).await;
                }
            });
        }
        Cmd::ReplyPermission { request_id, decision, message } => {
            if active.is_empty() {
                return;
            }
            let client = client.clone();
            let session_id = active.clone();
            let ui_tx = deps.ui_tx.clone();
            tokio::spawn(async move {
                if let Err(e) = client
                    .reply_permission(&session_id, &request_id, &decision, message.as_deref())
                    .await
                {
                    let _ = ui_tx
                        .send(UiEvent::Error(format!("Permission reply failed: {e}")))
                        .await;
                }
            });
        }
        Cmd::RetryConnect => {}
    }
}

/// Build the terminal hand-off command: the config override when present,
/// otherwise detection over known terminals. `{dir}` and `{session}` are
/// substituted when the template uses them.
fn terminal_argv(
    deps: &Deps,
    scratch_dir: &std::path::Path,
    session_id: Option<&str>,
) -> Option<Vec<String>> {
    let target = scratch_dir.to_string_lossy().to_string();
    if let Some(template) = &deps.config.handoff_command {
        let mut words: Vec<String> = split_words(template)
            .into_iter()
            .map(|word| word.replace("{dir}", &target))
            .collect();
        if let Some(session_id) = session_id {
            for word in words.iter_mut() {
                if word.contains("{session}") {
                    *word = word.replace("{session}", session_id);
                }
            }
        }
        // Drop a bare `{session}` placeholder when no session exists.
        words.retain(|word| *word != "{session}");
        if words.is_empty() {
            return None;
        }
        return Some(words);
    }

    const CANDIDATES: &[(&str, &[&str])] = &[
        ("ptyxis", &["--working-directory={dir}", "--", "opencode"]),
        ("kgx", &["--working-directory", "{dir}", "opencode"]),
        ("gnome-terminal", &["--working-directory={dir}", "--", "opencode"]),
        ("konsole", &["--workdir", "{dir}", "-e", "opencode"]),
    ];
    for (binary, template) in CANDIDATES {
        if exec_in_path(binary) {
            let mut argv: Vec<String> = std::iter::once((*binary).to_string())
                .chain(template.iter().map(|word| word.replace("{dir}", &target)))
                .collect();
            match session_id {
                Some(id) => {
                    argv.push("--session".into());
                    argv.push(id.to_string());
                }
                None => argv.push("--continue".into()),
            }
            return Some(argv);
        }
    }
    None
}

/// Minimal shell-like word splitting (quotes respected, no escapes).
fn split_words(template: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in template.chars() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                } else {
                    current.push(ch);
                }
            }
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                ch if ch.is_whitespace() => {
                    if !current.is_empty() {
                        words.push(std::mem::take(&mut current));
                    }
                }
                ch => current.push(ch),
            },
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn exec_in_path(binary: &str) -> bool {
    let Ok(paths) = std::env::var("PATH") else {
        return false;
    };
    paths
        .split(':')
        .map(|dir| std::path::PathBuf::from(dir).join(binary))
        .any(|candidate| candidate.is_file())
}

fn spawn_prompt(client: &Client, session_id: &str, text: String, images: Vec<ImageDto>, deps: &Deps) {
    let client = client.clone();
    let session_id = session_id.to_string();
    let ui_tx = deps.ui_tx.clone();
    let paths = deps.paths.clone();
    let files: Vec<FileAttachment> = images
        .into_iter()
        .map(|img| FileAttachment::inline_data(img.data_uri, Some(img.name)))
        .collect();
    tokio::spawn(async move {
        // Uses the no-timeout prompt path: a prompt POST may run for the
        // length of the whole turn. Everything that matters to the UI is
        // already queued; the result here just records the session id.
        match client.prompt_no_timeout(&session_id, &text, &files).await {
            Ok(_) => {
                let mut state = AppState::load(&paths);
                if state.last_session_id.as_deref() != Some(session_id.as_str()) {
                    state.last_session_id = Some(session_id);
                    state.save(&paths);
                }
            }
            Err(ApiError::Busy) => {
                let _ = ui_tx
                    .send(UiEvent::Error(
                        "The session is still busy — stop the current turn first.".into(),
                    ))
                    .await;
            }
            Err(e) => {
                let _ = ui_tx.send(UiEvent::Error(format!("Message failed: {e}"))).await;
            }
        }
    });
}

async fn create_chat(client: &Client, deps: &Deps) -> Result<SessionInfo, ApiError> {
    let scratch = deps.scratch();
    if let Err(e) = std::fs::create_dir_all(&scratch) {
        eprintln!("opencode-spotlight: could not create scratch dir: {e}");
    }
    let info = client
        .create_session(Some("Quick chat"), &scratch.to_string_lossy())
        .await?;
    let state = AppState {
        last_session_id: Some(info.id.clone()),
    };
    state.save(&deps.paths);
    Ok(info)
}

/// The session the overlay should open: the remembered one if it still
/// exists, otherwise the newest non-archived session in the scratch directory.
async fn resolve_session(deps: &Deps, client: &Client) -> Option<SessionInfo> {
    let state = AppState::load(&deps.paths);
    if let Some(id) = state.last_session_id.as_deref()
        && let Ok(info) = client.get_session(id).await {
            return Some(info);
        }
    let scratch = deps.scratch();
    if let Ok(list) = client
        .list_sessions(Some(&scratch.to_string_lossy()))
        .await
        && let Some(info) = list.into_iter().next() {
            let state = AppState {
                last_session_id: Some(info.id.clone()),
            };
            state.save(&deps.paths);
            return Some(info);
        }
    None
}

async fn load_history(client: &Client, session_id: &str, ui_tx: &async_channel::Sender<UiEvent>) {
    match client.messages(session_id).await {
        Ok(msgs) => {
            let _ = ui_tx
                .send(UiEvent::Bridge(BridgeIn::Init {
                    messages: msgs_to_bridge(&msgs),
                }))
                .await;
        }
        Err(e) => {
            let _ = ui_tx.send(UiEvent::Error(format!("Could not load history: {e}"))).await;
        }
    }
}

/// Build the authoritative content for an assistant message info object.
fn final_content(info: &Value) -> Option<BridgeIn> {
    let message_id = info.get("id").and_then(Value::as_str)?.to_string();
    let content = info.get("content")?.as_array()?;
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tools = Vec::new();
    let body = |acc: &mut String, t: &str| {
        if !acc.is_empty() {
            acc.push('\n');
        }
        acc.push_str(t);
    };
    for part in content {
        match part.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = part.get("text").and_then(Value::as_str) {
                    body(&mut text, t);
                }
            }
            Some("reasoning") => {
                if let Some(t) = part.get("text").and_then(Value::as_str) {
                    body(&mut reasoning, t);
                }
            }
            Some("tool") => {
                let tool_id = part.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                let name = part.get("name").and_then(Value::as_str).map(String::from);
                let state = part
                    .pointer("/state/status")
                    .and_then(Value::as_str)
                    .map(String::from)
                    .unwrap_or_else(|| "done".into());
                let detail = part
                    .pointer("/state/content/0/text")
                    .and_then(Value::as_str)
                    .map(|d| truncate(d, 120));
                tools.push(ToolInfo { id: tool_id, name, state, detail });
            }
            _ => {}
        }
    }
    Some(BridgeIn::ContentFull { message_id, text, reasoning, tools })
}

/// Convert a page of message infos into the bridge history snapshot.
fn msgs_to_bridge(msgs: &[Json]) -> Vec<MsgBody> {
    let mut out = Vec::new();
    for msg in msgs {
        let kind = msg.get("type").and_then(Value::as_str).unwrap_or("");
        let id = msg.get("id").and_then(Value::as_str).unwrap_or("").to_string();
        if id.is_empty() {
            continue;
        }
        match kind {
            "user" => {
                let text = msg.get("text").and_then(Value::as_str).unwrap_or("").to_string();
                let images = msg
                    .get("files")
                    .and_then(Value::as_array)
                    .map(|files| {
                        files
                            .iter()
                            .map(|f| f.get("uri").and_then(Value::as_str).unwrap_or("").to_string())
                            .filter(|uri| uri.starts_with("data:"))
                            .collect()
                    })
                    .unwrap_or_default();
                out.push(MsgBody::User { id, text, images });
            }
            "assistant" => match final_content(msg) {
                Some(BridgeIn::ContentFull { message_id, text, reasoning, tools }) => {
                    out.push(MsgBody::Assistant { id: message_id, text, reasoning, tools });
                }
                _ => {
                    out.push(MsgBody::Assistant {
                        id: id.clone(),
                        text: String::new(),
                        reasoning: String::new(),
                        tools: Vec::new(),
                    });
                }
            },
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::oc::http::ensure_connection_with_budget;
    use httpmock::prelude::*;
    use serde_json::json;
    use std::path::Path;

    fn write_registration(dir: &Path, base_url: &str) -> std::path::PathBuf {
        let path = dir.join("service.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"id":"t","version":"2.2.7","url":"{base_url}","pid":42,"password":"pw-ensure"}}"#
            ),
        )
        .unwrap();
        path
    }

    /// Healthy registration: ensure returns a client without spawning.
    #[tokio::test]
    async fn ensure_connects_without_spawning() {
        let svc = MockServer::start();
        svc.mock(|when, then| {
            when.method(GET).path("/api/info");
            then.status(200).json_body(json!({"version": "2.2.7"}));
        });
        let dir = tempfile::tempdir().unwrap();
        let reg_path = write_registration(dir.path(), &svc.url(""));
        let mut spawned = false;
        let client = ensure_connection_with_budget(&reg_path, || {
            spawned = true;
        }, std::time::Duration::from_secs(2))
        .await
        .expect("should connect");
        assert!(!spawned, "launcher must not run when the service is healthy");
        assert_eq!(client.base(), svc.url("").trim_end_matches('/'));
    }

    /// Nothing answers: launcher runs exactly once, ensure times out.
    #[tokio::test]
    async fn ensure_spawns_once_then_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let reg_path = dir.path().join("service.json");
        let spawned = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        {
            let spawned = spawned.clone();
            let err = ensure_connection_with_budget(
                &reg_path,
                move || {
                    spawned.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                },
                std::time::Duration::from_millis(900),
            )
            .await
            .expect_err("no service up");
            assert!(matches!(err, crate::oc::http::ConnectFailure::Timeout), "{err:?}");
        }
        assert_eq!(spawned.load(std::sync::atomic::Ordering::SeqCst), 1, "launcher ran exactly once");
    }

    /// Healthy registration appears only after the launcher runs.
    #[tokio::test]
    async fn ensure_waits_for_a_registration_to_appear() {
        let dir = tempfile::tempdir().unwrap();
        let reg_path = dir.path().join("service.json");
        let svc = MockServer::start();
        svc.mock(|when, then| {
            when.method(GET).path("/api/info");
            then.status(200).json_body(json!({"version": "3"}));
        });
        let reg_path2 = reg_path.clone();
        let base_url = svc.url("").trim_end_matches('/').to_string();
        let base_for_closure = base_url.clone();
        let client = ensure_connection_with_budget(
            &reg_path2,
            move || {
                let _ = write_registration(dir.path(), &base_for_closure);
            },
            std::time::Duration::from_secs(5),
        )
        .await
        .expect("service appears after launch");
        assert_eq!(client.base(), base_url);
    }
}
