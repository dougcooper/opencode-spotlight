//! The overlay window: an Apple-Spotlight-style flow. Summoning shows a slim
//! centered pill; sending, pasting an image, or pressing ↓ expands it into
//! the chat card with the transcript above the input. Esc, focus loss, and
//! close all hide it in either shape.

use std::cell::Cell;
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
/* Iridescence-style glass: accent-tinted libadwaita surfaces at high
   opacity, hairline contrast outlines, and a subtle accent window edge.
   The palette adapts automatically to the system accent and light/dark. */

window.spot {
  border-radius: 24px;
  outline: 1px solid color-mix(in srgb, @accent_bg_color 7%, alpha(#ffffff, 0.18));
  outline-offset: -1px;
  background: transparent;
}

.chat-card {
  padding: 8px;
  border-radius: 23px;
  background: color-mix(in srgb, transparent 9%,
      color-mix(in srgb, @accent_bg_color 7%, @window_bg_color));
  color: @window_fg_color;
  border: 1px solid color-mix(in srgb, @accent_bg_color 5%, alpha(#ffffff, 0.15));
}

.chat-card .glass-pill {
  background: color-mix(in srgb, transparent 12%,
      color-mix(in srgb, @accent_bg_color 5%, @card_bg_color));
  border-radius: 22px;
  border: 1px solid color-mix(in srgb, @accent_bg_color 4%, alpha(#ffffff, 0.13));
  padding: 4px 6px;
}

/* Native contrast strokes on the pill's controls; the accent send circle
   and flat chips stay clean. */
.chat-card .glass-pill button {
  box-shadow: inset 0 0 0 1px color-mix(in srgb, currentColor 22%, transparent);
}
.chat-card .glass-pill button.flat,
.chat-card .glass-pill button.send-circle {
  box-shadow: none;
  min-width: 30px;
  min-height: 30px;
}

.chat-card .overlay-status { padding: 0 6px 2px; }
.dot { font-size: 10px; }
.conn-ok { color: @success_color; }
.conn-warn { color: @warning_color; }
.conn-err { color: @error_color; }
.attachments { padding: 2px 4px; }
.attachment-chip { background: alpha(@accent_bg_color, 0.24); border-radius: 6px; padding: 2px 6px; }
";
const APP_TITLE: &str = "OpenCode Spotlight";
const COLLAPSED: (i32, i32) = (600, 62);
const EXPANDED: (i32, i32) = (620, 660);

pub struct OverlayWindow {
    pub window: adw::Window,
    pub transcript: Transcript,
    pub input: Rc<InputBar>,
    pub attachments: Rc<AttachmentTray>,
    toast: adw::ToastOverlay,
    conn_dot: Label,
    busy_spinner: Spinner,
    usage_label: Label,
    status: GtkBox,
    expanded: Cell<bool>,
}

impl OverlayWindow {
    pub fn build(app: &adw::Application, ui: &Rc<UiContext>) -> Rc<Self> {
        load_css();

        // AdwWindow gives the floating rounded surface (CSD radius + shadow).
        let window = adw::Window::builder()
            .application(app)
            .title(APP_TITLE)
            .default_width(COLLAPSED.0)
            .default_height(COLLAPSED.1)
            .build();
        window.add_css_class("spot");
        window.add_css_class(if adw::StyleManager::default().is_dark() { "dark" } else { "light" });

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
            .visible(false)
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
            .spacing(4)
            .css_classes(["chat-card"])
            .build();
        // Transcript on top and stretching; the widget strip (connection,
        // usage, actions) and the input pill dock at the bottom of the card.
        transcript.webview.set_hexpand(true);
        transcript.webview.set_vexpand(true);
        root.append(&transcript.webview);
        root.append(attachments.widget());
        root.append(&status);
        root.append(&input.widget);

        let toast = adw::ToastOverlay::new();
        toast.set_child(Some(&root));
        window.set_content(Some(&toast));

        // Handlers run inside Rc::new_cyclic below so they can reach the
        // window itself (weak self ref) for the expand/collapse transitions.

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

        Rc::new_cyclic(|me_w: &std::rc::Weak<Self>| {
            // Esc hides; Ctrl+V stages images (and expands); ↓ expands to peek.
            {
                let me_w = me_w.clone();
                let ui_w = Rc::downgrade(ui);
                let key_controller = EventControllerKey::new();
                key_controller.set_propagation_phase(PropagationPhase::Capture);
                key_controller.connect_key_pressed(move |_ctrl, keyval, _keycode, state| {
                    let Some(me) = me_w.upgrade() else {
                        return glib::Propagation::Proceed
                    };
                    let Some(ui) = ui_w.upgrade() else {
                        return glib::Propagation::Stop
                    };
                    if keyval == gdk::Key::Escape {
                        ui.hide_overlay();
                        return glib::Propagation::Stop;
                    }
                    let paste =
                        keyval == gdk::Key::v && state.contains(gdk::ModifierType::CONTROL_MASK);
                    if paste {
                        me.expand();
                        let handled = ui.try_stage_clipboard_image();
                        if handled {
                            return glib::Propagation::Stop;
                        }
                    }
                    if keyval == gdk::Key::Down && me.expand() {
                        return glib::Propagation::Stop;
                    }
                    glib::Propagation::Proceed
                });
                window.add_controller(key_controller);
            }

            // Focus loss hides (fact 4) — but not while a permission dialog
            // owns the screen.
            {
                let ui_w = Rc::downgrade(ui);
                window.connect_is_active_notify(move |win| {
                    let Some(ui) = ui_w.upgrade() else { return };
                    if !win.is_active() && !ui.permission_dialog_open() {
                        win.set_visible(false);
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

            Self {
                window,
                transcript,
                input,
                attachments,
                toast,
                conn_dot,
                busy_spinner,
                usage_label,
                status,
                expanded: Cell::new(false),
            }
        })
    }

    pub fn is_visible(&self) -> bool {
        self.window.is_visible()
    }

    pub fn is_expanded(&self) -> bool {
        self.expanded.get()
    }

    pub fn present(&self) {
        self.window.set_visible(true);
        self.window.present();
        self.input.focus();
    }

    pub fn hide(&self) {
        self.window.set_visible(false);
    }

    /// Expand into the chat card. Returns true when the shape changed.
    pub fn expand(&self) -> bool {
        if self.expanded.get() {
            return false;
        }
        self.expanded.set(true);
        self.transcript.webview.set_visible(true);
        self.status.set_visible(true);
        self.attachments
            .widget()
            .set_visible(self.attachments.len() > 0);
        self.input.set_pill(false);
        self.retarget(EXPANDED);
        true
    }

    /// Collapse to the search pill (used before presenting so every summon
    /// starts compact, Spotlight-style).
    pub fn collapse_for_summon(&self) {
        self.expanded.set(false);
        self.transcript.webview.set_visible(false);
        self.status.set_visible(false);
        self.attachments.widget().set_visible(false);
        self.input.set_pill(true);
        // Pin the minimum so hidden-but-sizeable children cannot inflate the
        // pill's mapped height.
        self.window.set_size_request(COLLAPSED.0, COLLAPSED.1);
        self.window.set_default_size(COLLAPSED.0, COLLAPSED.1);
    }

    /// Resize by hiding and remapping — reliable on Wayland.
    fn retarget(&self, size: (i32, i32)) {
        self.window.set_size_request(-1, -1);
        self.window.set_default_size(size.0, size.1);
        self.window.set_visible(false);
        let window = self.window.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(20), move || {
            window.set_visible(true);
            window.present();
        });
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
        self.usage_label.set_text(&format!("▲ {tokens} tokens · ${cost:.4}"));
    }

    pub fn set_model_chip(&self, label: &str, visible: bool) {
        self.input.set_model_label(label, visible);
    }

    pub fn push_theme(&self, dark: bool) {
        self.window.remove_css_class("light");
        self.window.remove_css_class("dark");
        self.window.add_css_class(if dark { "dark" } else { "light" });
        self.transcript.send(&Self::theme_payload(dark));
    }

    /// The transcript page paints itself with the same resolved glass tint
    /// the card uses (accent mixed into the Adwaita window base color), so
    /// the page area reads as part of the card in both modes.
    fn theme_payload(dark: bool) -> BridgeIn {
        let base: [f64; 3] = if dark { [0.141, 0.141, 0.157] } else { [0.980, 0.980, 0.980] };
        // `accent_color` resolves the user's chosen GNOME accent to an RGBA.
        let weights = if dark {
            (13.0f64 / 100.0, 1.0 - 13.0f64 / 100.0)
        } else {
            (9.0f64 / 100.0, 1.0 - 9.0f64 / 100.0)
        };
        let accent = adw::StyleManager::default().accent_color().to_rgba();
        let accent_channel = |a: &gdk::RGBA| (a.red() + a.green() + a.blue()) / 3.0;
        let mix = |b: f64| -> u8 {
            let a = accent_channel(&accent) as f64;
            let c = weights.1 * b + weights.0 * a;
            (c.clamp(0.0, 1.0) * 255.0).round() as u8
        };
        BridgeIn::Theme {
            dark,
            bg: format!("#{:02x}{:02x}{:02x}", mix(base[0]), mix(base[1]), mix(base[2])),
        }
    }

    pub fn size(&self) -> (i32, i32) {
        (self.window.width(), self.window.height())
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
