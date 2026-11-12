//! The controller: owns the UI context, pumps backend events into the GTK
//! main loop, renders permission dialogs, and implements the input actions.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::rc::Rc;

use libadwaita as adw;
use adw::prelude::*;
use gio;
use gtk4::glib;
use std::os::unix::process::CommandExt;


use super::input::InputActions;
use super::window::OverlayWindow;
use crate::oc::service::{Cmd, ConnState, ImageDto, UiEvent};
use crate::oc::{ModelRef, PermReq};
use crate::state::{Config, Paths};

/// The configured quick/deep pair. The chip appears only when both are set.
struct ModelPair {
    quick: Option<ModelRef>,
    deep: Option<ModelRef>,
}

pub struct UiContext {
    cmd_tx: async_channel::Sender<Cmd>,
    #[allow(dead_code)] // kept for symmetric ownership; the pump holds the original
    window: RefCell<Option<Rc<OverlayWindow>>>,
    app: RefCell<Option<adw::Application>>,
    paths: Paths,
    config: Config,
    perms: RefCell<VecDeque<PermReq>>,
    current_perm: RefCell<Option<String>>,
    open_dialog: RefCell<Option<adw::AlertDialog>>,
    conn: std::cell::RefCell<Option<ConnState>>,
    dialog_open: Cell<bool>,
    busy: Cell<bool>,
    quick_selected: Cell<bool>,
    models: ModelPair,
}

impl UiContext {
    /// Start the backend and the event pump on the GTK main context.
    pub fn start(paths: Paths, config: Config) -> Rc<Self> {
        let (cmd_tx, cmd_rx) = async_channel::unbounded::<Cmd>();
        let (ui_tx, ui_rx) = async_channel::unbounded::<UiEvent>();
        crate::oc::service::start(paths.clone(), config.clone(), cmd_rx, ui_tx);

        let models = {
            let configured = config.models.as_ref();
            ModelPair {
                quick: configured.and_then(|m| m.quick.as_ref()).map(|m| m.into()),
                deep: configured.and_then(|m| m.deep.as_ref()).map(|m| m.into()),
            }
        };

        let context = Rc::new(Self {
            cmd_tx,
            window: RefCell::new(None),
            app: RefCell::new(None),
            paths,
            config,
            perms: RefCell::new(VecDeque::new()),
            current_perm: RefCell::new(None),
            open_dialog: RefCell::new(None),
            conn: RefCell::new(None),
            dialog_open: Cell::new(false),
            busy: Cell::new(false),
            quick_selected: Cell::new(true),
            models,
        });

        glib::spawn_future_local({
            let weak = Rc::downgrade(&context);
            async move {
                while let Ok(event) = ui_rx.recv().await {
                    if let Some(ctx) = weak.upgrade() {
                        ctx.handle(event);
                    } else {
                        break;
                    }
                }
            }
        });

        context
    }

    /// The GTK application, stored at startup so windows can be built lazily.
    pub fn set_app(&self, app: &adw::Application) {
        *self.app.borrow_mut() = Some(app.clone());
    }

    /// Summon or toggle the overlay (facts 1–3).
    pub fn show_or_toggle(self: &Rc<Self>, toggle: bool) {
        let first = self.window.borrow().is_none();
        if first {
            if let Some(app) = self.app.borrow().as_ref() {
                let window = OverlayWindow::build(app, self);
                *self.window.borrow_mut() = Some(window);
            } else {
                return;
            }
            // The transcript can receive now: ask the backend for a fresh
            // sync (history + connection + model state).
            let _ = self.cmd_tx.send_blocking(Cmd::RetryConnect);
            self.sync_model_chip();
        }

        let window = self
            .window
            .borrow()
            .as_ref()
            .expect("overlay window was built")
            .clone();
        if !window.is_visible() {
            // Spotlight-style: every summon starts as the slim pill.
            window.collapse_for_summon();
        }
        if window.is_visible() {
            if toggle {
                window.hide();
            } else {
                window.present();
            }
        } else {
            window.present();
        }
        self.push_theme_to_window();
        self.show_next_perm();
    }

    pub fn hide_overlay(&self) {
        if let Some(window) = self.window.borrow().as_ref() {
            window.hide();
        }
    }

    pub fn push_theme(&self, dark: bool) {
        if let Some(window) = self.window.borrow().as_ref() {
            window.push_theme(dark);
        }
    }

    fn push_theme_to_window(&self) {
        let dark = adw::StyleManager::default().is_dark();
        if let Some(window) = self.window.borrow().as_ref() {
            window.push_theme(dark);
        }
    }

    pub fn permission_dialog_open(&self) -> bool {
        self.dialog_open.get()
    }

    fn handle(self: &Rc<Self>, event: UiEvent) {
        if std::env::var("SPOTLIGHT_DEBUG").as_deref() == Ok("1") {
            let summary = match &event {
                UiEvent::Conn(c) => format!("conn {c:?}"),
                UiEvent::Busy(b) => format!("busy {b}"),
                UiEvent::Usage { tokens, cost } => format!("usage {tokens}/{cost}"),
                UiEvent::Perm(p) => format!("perm {}", p.id),
                UiEvent::PermReplied { id } => format!("perm-replied {id}"),
                UiEvent::Bridge(p) => format!("bridge {:?}", p),
                UiEvent::PromptQueued => "prompt-queued".into(),
                UiEvent::Error(m) => format!("error {m}"),
            };
            eprintln!("spotlight: event {summary}");
        }
        crate::ui::debug_ipc::record_event(&event);
        match event {
            UiEvent::Conn(state) => {
                *self.conn.borrow_mut() = Some(state.clone());
                if let Some(window) = self.window.borrow().as_ref() {
                    window.set_conn(state);
                }
            }
            UiEvent::Busy(busy) => {
                self.busy.set(busy);
                if let Some(window) = self.window.borrow().as_ref() {
                    window.set_busy(busy);
                }
            }
            UiEvent::Usage { tokens, cost } => {
                if let Some(window) = self.window.borrow().as_ref() {
                    window.set_usage(tokens, cost);
                }
            }
            UiEvent::Perm(perm) => {
                self.perms.borrow_mut().push_back(perm);
                self.show_next_perm();
            }
            UiEvent::PermReplied { id } => {
                if !id.is_empty()
                    && self.current_perm.borrow().as_deref() == Some(id.as_str())
                {
                    self.current_perm.borrow_mut().take();
                    if let Some(dialog) = self.open_dialog.borrow_mut().take() {
                        dialog.force_close();
                    }
                    self.dialog_open.set(false);
                }
                if !id.is_empty() {
                    self.perms.borrow_mut().retain(|perm| perm.id != id);
                }
            }
            UiEvent::Bridge(payload) => {
                if let Some(window) = self.window.borrow().as_ref() {
                    window.transcript.send(&payload);
                }
            }
            UiEvent::PromptQueued => {
                if let Some(window) = self.window.borrow().as_ref() {
                    window.attachments.clear();
                    window.input.set_attachments(0);
                    window.input.clear();
                }
            }
            UiEvent::Error(message) => {
                if let Some(window) = self.window.borrow().as_ref() {
                    window.toast(&message);
                } else {
                    eprintln!("opencode-spotlight: {message}");
                }
            }
        }
    }

    // ---- permission dialogs ------------------------------------------------

    fn show_next_perm(self: &Rc<Self>) {
        if self.dialog_open.get() {
            return;
        }
        let Some(perm) = self.perms.borrow_mut().front().cloned() else {
            return;
        };
        let Some(window) = self.window.borrow().as_ref().cloned() else {
            return; // shown when the overlay first appears instead
        };

        self.dialog_open.set(true);
        *self.current_perm.borrow_mut() = Some(perm.id.clone());

        let heading = format!("Permit {}?", perm.action);
        let mut body = perm.message.clone().unwrap_or_default();
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str(&perm.resources.join("\n"));
        if !perm.save.is_empty() {
            body.push_str(&format!("\n\nAllow always would save: {}", perm.save.join(", ")));
        }

        let dialog = adw::AlertDialog::new(Some(&heading), Some(&body));
        if !perm.save.is_empty() {
            dialog.add_response("always", "Allow always");
        }
        dialog.add_response("reject", "Reject");
        dialog.add_response("once", "Allow once");
        dialog.set_response_appearance("once", adw::ResponseAppearance::Suggested);
        dialog.set_response_appearance("reject", adw::ResponseAppearance::Destructive);
        dialog.set_close_response("reject");
        *self.open_dialog.borrow_mut() = Some(dialog.clone());

        // `choose` resolves when the dialog is dismissed (close = reject).
        let weak = Rc::downgrade(self);
        let request_id = perm.id.clone();
        dialog.choose(
            Some(&window.window),
            None::<&gio::Cancellable>,
            move |response| {
                let Some(ctx) = weak.upgrade() else { return };
                *ctx.current_perm.borrow_mut() = None;
                *ctx.open_dialog.borrow_mut() = None;
                ctx.dialog_open.set(false);
                let _ = ctx.cmd_tx.send_blocking(Cmd::ReplyPermission {
                    request_id: request_id.clone(),
                    decision: response.to_string(),
                    message: None,
                });
                ctx.show_next_perm();
            },
        );
    }

    // ---- input actions -----------------------------------------------------

    fn send_message(&self) {
        let Some(window) = self.window.borrow().as_ref().cloned() else {
            return;
        };
        if self.busy.get() {
            return; // the send button is disabled while busy anyway
        }
        // Apple-Spotlight behavior: sending from the collapsed pill expands
        // the chat card so the answer is visible as it streams.
        window.expand();
        let text = window.input.text();
        if window.input.can_send() {
            let images: Vec<ImageDto> = self
                .materialize_attachments()
                .into_iter()
                .map(|(path, name)| ImageDto {
                    data_uri: format!("file://{}", path.display()),
                    name,
                })
                .collect();
            let _ = self.cmd_tx.send_blocking(Cmd::Send { text, images });
            window.input.set_attachments(window.attachments.len());
        }
    }

    /// Write staged clipboard images into the scratch directory and return
    /// `(file path, name)` pairs. OpenCode providers deliver images by file
    /// path (data-URI blobs never reach the model).
    fn materialize_attachments(&self) -> Vec<(std::path::PathBuf, String)> {
        let attach_dir = self
            .config
            .scratch_dir(&self.paths)
            .join("media");
        let _ = std::fs::create_dir_all(&attach_dir);
        let mut out = Vec::new();
        for (png, name) in self.window.borrow().as_ref().map_or(vec![], |w| {
            w.attachments.take_all()
        }) {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            let path = attach_dir.join(format!("img-{stamp}-{}.png", out.len()));
            match std::fs::write(&path, &png) {
                Ok(()) => out.push((path, name)),
                Err(e) => {
                    eprintln!("opencode-spotlight: could not save attachment: {e}");
                }
            }
        }
        out
    }

    fn stop(&self) {
        if self.busy.get() {
            let _ = self.cmd_tx.send_blocking(Cmd::Interrupt);
        }
    }

    fn toggle_model(&self) {
        let next_quick = !self.quick_selected.get();
        self.quick_selected.set(next_quick);
        self.sync_model_chip();
        let target = if next_quick {
            self.models.quick.clone()
        } else {
            self.models.deep.clone()
        };
        if let Some(target) = target {
            let _ = self.cmd_tx.send_blocking(Cmd::SwitchModel(target));
        }
    }

    fn sync_model_chip(&self) {
        let Some(window) = self.window.borrow().as_ref().cloned() else {
            return;
        };
        let visible = self.models.quick.is_some() && self.models.deep.is_some();
        let label = if self.quick_selected.get() { "⚡ quick" } else { "🧠 deep" };
        window.set_model_chip(label, visible);
    }

    // ---- clipboard ---------------------------------------------------------

    /// Ctrl+V: stage a clipboard image for the next message. Returns true when
    /// the paste is consumed; text pastes fall through to the `GtkTextView`.
    pub fn try_stage_clipboard_image(self: &Rc<Self>) -> bool {
        let Some(window) = self.window.borrow().as_ref().cloned() else {
            return false;
        };
        let clipboard = window.window.clipboard();
        let formats = clipboard.formats();
        let mime_types = formats.mime_types();
        let image = mime_types.iter().any(|m| m.starts_with("image/"));
        let text = mime_types.iter().any(|m| m.starts_with("text/plain"));
        if !image || text {
            // When both are present, prefer pasting text into the input.
            return false;
        }

        // The tray and input bar are needed by the read callback.
        window.expand();
        let tray = window.attachments.clone();
        let window_weak: std::rc::Weak<OverlayWindow> = Rc::downgrade(&window);
        clipboard.read_texture_async(None::<&gio::Cancellable>, move |result| {
            let Some(window) = window_weak.upgrade() else { return };
            match result {
                Ok(Some(texture)) => match tray.add_from_texture(&texture) {
                    Ok(_name) => window.input.set_attachments(tray.len()),
                    Err(why) => window.toast(&why),
                },
                Ok(None) => {}
                Err(e) => window.toast(&format!("Could not read the clipboard image: {e}")),
            }
        });
        true
    }

    // ---- desktop actions ---------------------------------------------------

    /// Open the scratch chat in a real terminal running the OpenCode TUI
    /// (fact 25).
    pub fn handoff_to_terminal(self: &Rc<Self>) {
        let scratch = self.config.scratch_dir(&self.paths);
        let _ = std::fs::create_dir_all(&scratch);
        match handoff_command(&self.config, &scratch) {
            Some(argv) => {
                let (program, args) = argv.split_first().expect("non-empty argv");
                let result = Command::new(program)
                    .args(args.iter().map(String::as_str))
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .process_group(0)
                    .spawn();
                if let Err(e) = result {
                    self.toast_in_window(&format!("Could not open the terminal ({program}): {e}"));
                }
            }
            None => {
                self.toast_in_window(
                    "No terminal found — set handoff_command in the config.",
                );
            }
        }
    }

    pub fn new_chat(&self) {
        let _ = self.cmd_tx.send_blocking(Cmd::NewChat);
    }

    /// Debug hook for the model toggle chip.
    pub fn toggle_model_chip(&self) {
        self.toggle_model();
    }

    fn toast_in_window(&self, message: &str) {
        if let Some(window) = self.window.borrow().as_ref() {
            window.toast(message);
        } else {
            eprintln!("opencode-spotlight: {message}");
        }
    }

    // ---- debug IPC ---------------------------------------------------------

    /// The overlay window handle for the debug surface.
    pub fn window_take(&self) -> Option<Rc<OverlayWindow>> {
        self.window.borrow().clone()
    }

    pub fn busy(&self) -> bool {
        self.busy.get()
    }

    pub fn connection(&self) -> Option<ConnState> {
        self.conn.borrow().clone()
    }

    /// Make sure the window exists and is presented (debug determinism).
    pub fn debug_summon(self: &Rc<Self>) {
        match self.window.borrow().as_ref() {
            Some(window) => window.present(),
            None => self.show_or_toggle(false),
        }
    }

    /// Type into the input bar exactly as if the user had, then send.
    pub fn debug_send(self: &Rc<Self>, text: &str) {
        self.debug_summon();
        if let Some(window) = self.window.borrow().as_ref() {
            window.input.set_text(text);
        }
        self.send_message();
    }

    /// Stage a PNG like a Ctrl+V paste.
    pub fn debug_paste_png(self: &Rc<Self>, bytes: Vec<u8>) {
        self.debug_summon();
        if let Some(window) = self.window.borrow().as_ref() {
            match window.attachments.add_from_png(bytes) {
                Ok(_name) => window.input.set_attachments(window.attachments.len()),
                Err(why) => window.toast(&why),
            }
        }
    }
}

impl InputActions for UiContext {
    fn on_send(&self) {
        self.send_message();
    }
    fn on_stop(&self) {
        self.stop();
    }
    fn on_toggle_model(&self) {
        self.toggle_model();
    }
}

/// Build the terminal hand-off command: the config override when present,
/// otherwise a detection pass over known terminals. `{dir}` is substituted.
fn handoff_command(config: &Config, scratch_dir: &Path) -> Option<Vec<String>> {
    let target = scratch_dir.to_string_lossy().to_string();
    if let Some(template) = &config.handoff_command {
        let words = split_words(template);
        if words.is_empty() {
            return None;
        }
        return Some(
            words
                .into_iter()
                .map(|word| word.replace("{dir}", &target))
                .collect(),
        );
    }
    const CANDIDATES: &[(&str, &[&str])] = &[
        ("ptyxis", &["--directory", "{dir}", "opencode"]),
        ("kgx", &["--working-directory", "{dir}", "opencode"]),
        ("gnome-terminal", &["--working-directory={dir}", "--", "opencode"]),
        ("konsole", &["--workdir", "{dir}", "-e", "opencode"]),
    ];
    for (binary, template) in CANDIDATES {
        if exec_in_path(binary) {
            return Some(
                std::iter::once((*binary).to_string())
                    .chain(template.iter().map(|word| word.replace("{dir}", &target)))
                    .collect(),
            );
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
        .map(|dir| PathBuf::from(dir).join(binary))
        .any(|candidate| candidate.is_file())
}

use std::path::Path;
