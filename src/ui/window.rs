//! The overlay window: a compact floating spotlight. Esc, focus loss, and
//! close all hide it; the hotkey and button bar stay in the controller.

use std::rc::Rc;

use libadwaita as adw;
use glib;

use gtk4::gdk;
use gtk4::Box as GtkBox;
use adw::prelude::*;
use gtk4::prelude::*;
use gtk4::{Button, EventControllerKey, Label, Orientation, Spinner, PropagationPhase};

use super::attachments::AttachmentTray;
use super::controller::UiContext;
use super::input::InputBar;
use super::transcript::Transcript;
use crate::bridge::BridgeIn;
use crate::oc::service::ConnState;

pub const RESOURCE_CSS: &str = "
.overlay-status { padding: 4px 10px; }
.dot { font-size: 10px; }
.conn-ok { color: @success_color; }
.conn-warn { color: @warning_color; }
.conn-err { color: @error_color; }
.attachments { padding: 2px 10px; }
.attachment-chip { background: alpha(@accent_color, 0.2); border-radius: 6px; padding: 2px 6px; }
";
const APP_TITLE: &str = "OpenCode Spotlight";

pub struct OverlayWindow {
    pub window: adw::ApplicationWindow,
    pub transcript: Transcript,
    pub input: Rc<InputBar>,
    pub attachments: Rc<AttachmentTray>,
    toast: adw::ToastOverlay,
    conn_dot: Label,
    busy_spinner: Spinner,
    usage_label: Label,
}

impl OverlayWindow {
    pub fn build(app: &adw::Application, ui: &Rc<UiContext>) -> Rc<Self> {
        load_css();

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title(APP_TITLE)
            .default_width(620)
            .default_height(700)
            .build();

        let transcript = Transcript::new(make_copy_sink());
        let attachments = AttachmentTray::build();
        let input = InputBar::build(ui);

        let conn_dot = Label::builder()
            .label("●")
            .css_classes(["dot", "conn-warn"])
            .tooltip_text("Connection to the OpenCode service")
            .build();
        let busy_spinner = Spinner::builder().build();
        let usage_label = Label::builder()
            .css_classes(["dim-label", "caption"])
            .label("")
            .build();

        let handoff_button = Button::builder()
            .icon_name("utilities-terminal-symbolic")
            .tooltip_text("Continue this chat in the OpenCode TUI")
            .css_classes(["flat"])
            .build();
        let new_chat_button = Button::builder()
            .icon_name("tab-new-symbolic")
            .tooltip_text("Start a new chat")
            .css_classes(["flat"])
            .build();

        let status = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(6)
            .css_classes(["overlay-status"])
            .build();
        status.append(&conn_dot);
        status.append(&busy_spinner);
        status.append(&usage_label);
        {
            let filler = gtk4::Box::new(Orientation::Horizontal, 0);
            filler.set_hexpand(true);
            status.append(&filler);
        }
        status.append(&handoff_button);
        status.append(&new_chat_button);

        let root = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(0)
            .build();
        root.append(&status);
        root.append(&transcript.webview);
        root.append(attachments.widget());
        root.append(&input.widget);

        let toast = adw::ToastOverlay::new();
        toast.set_child(Some(&root));
        window.set_content(Some(&toast));

        // Esc hides; Ctrl+V stages clipboard images; everything else passes.
        {
            let ui_w = Rc::downgrade(ui);
            let key_controller = EventControllerKey::new();
            key_controller.set_propagation_phase(PropagationPhase::Capture);
            key_controller.connect_key_pressed(move |_ctrl, keyval, _keycode, state| {
                let Some(ui) = ui_w.upgrade() else { return glib::Propagation::Proceed };
                if keyval == gdk::Key::Escape {
                    ui.hide_overlay();
                    return glib::Propagation::Stop;
                }
                let paste = keyval == gdk::Key::v
                    && state
                        .contains(gdk::ModifierType::CONTROL_MASK);
                if paste {
                    let handled = ui.try_stage_clipboard_image();
                    if handled {
                        return glib::Propagation::Stop;
                    }
                }
                glib::Propagation::Proceed
            });
            window.add_controller(key_controller);
        }

        // Focus loss hides (fact 4) — but not while a permission dialog owns
        // the screen.
        {
            let ui_w = Rc::downgrade(ui);
            let window_w: glib::WeakRef<adw::ApplicationWindow> = {
                let weak = glib::WeakRef::new();
                weak.set(Some(&window));
                weak
            };
            window.connect_is_active_notify(move |win| {
                let Some(ui) = ui_w.upgrade() else { return };
                if !win.is_active() && !ui.permission_dialog_open()
                    && let Some(w) = window_w.upgrade() {
                        w.set_visible(false);
                    }
            });
        }

        // Close hides; the app keeps running (fact 5).
        window.connect_close_request(|win| {
            win.set_visible(false);
            glib::Propagation::Stop
        });

        {
            let ui_w = Rc::downgrade(ui);
            handoff_button.connect_clicked(move |_| {
                if let Some(ui) = ui_w.upgrade() {
                    ui.handoff_to_terminal();
                }
            });
        }
        {
            let ui_w = Rc::downgrade(ui);
            new_chat_button.connect_clicked(move |_| {
                if let Some(ui) = ui_w.upgrade() {
                    ui.new_chat();
                }
            });
        }

        // Theme follows the system style.
        {
            let ui_w = Rc::downgrade(ui);
            let manager = adw::StyleManager::default();
            manager.connect_dark_notify(move |manager| {
                if let Some(ui) = ui_w.upgrade() {
                    ui.push_theme(manager.is_dark());
                }
            });
        }

        Rc::new(Self {
            window,
            transcript,
            input,
            attachments,
            toast,
            conn_dot,
            busy_spinner,
            usage_label,
        })
    }

    pub fn is_visible(&self) -> bool {
        self.window.is_visible()
    }

    pub fn present(&self) {
        self.window.set_visible(true);
        self.window.present();
        self.input.focus();
    }

    pub fn hide(&self) {
        self.window.set_visible(false);
    }

    pub fn toast(&self, message: &str) {
        // Toast titles are parsed as markup; escape everything.
        let escaped = glib::markup_escape_text(message);
        let toast = adw::Toast::builder().title(&*escaped).timeout(3).build();
        self.toast.add_toast(toast);
    }

    pub fn set_conn(&self, state: ConnState) {
        let (class, tooltip) = match state {
            ConnState::Connecting => {
                ("conn-warn", "Connecting to the OpenCode service…".to_string())
            }
            ConnState::Connected => {
                ("conn-ok", "Connected to the OpenCode service".to_string())
            }
            ConnState::Unreachable(why) => ("conn-err", why),
        };
        self.conn_dot.set_css_classes(&["dot", class]);
        self.conn_dot.set_tooltip_text(Some(tooltip.as_str()));
    }

    pub fn set_busy(&self, busy: bool) {
        self.input.set_busy(busy);
        if busy {
            self.busy_spinner.start();
        } else {
            self.busy_spinner.stop();
        }
    }

    pub fn set_usage(&self, tokens: u64, cost: f64) {
        self.usage_label.set_text(&format!(
            "▲ {tokens} tokens · ${cost:.4}",
        ));
    }

    /// Push the current model chip state (chip visible only when
    /// quick/deep is configured).
    pub fn set_model_chip(&self, label: &str, visible: bool) {
        self.input.set_model_label(label, visible);
    }

    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    pub fn push_theme(&self, dark: bool) {
        self.transcript.send(&BridgeIn::Theme { dark });
    }
}

fn make_copy_sink() -> std::boxed::Box<dyn Fn(&str)> {
    Box::new(|text: &str| {
        if let Some(display) = gdk::Display::default() {
            let clipboard = display.clipboard();
            clipboard.set_text(text);
        }
    })
}

fn load_css() {
    static LOADED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if LOADED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    if let Some(display) = gdk::Display::default() {
        let provider = gtk4::CssProvider::new();
        #[allow(deprecated)]
        gtk4::CssProvider::load_from_data(&provider, RESOURCE_CSS);
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
