//! The input bar: multiline text entry with Enter-to-send, the model toggle
//! chip, and the send/stop controls. Key handling for Esc and Ctrl+V lives at
//! the window level (see `window.rs`); this widget only handles Enter.

use std::cell::Cell;
use std::rc::Rc;

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::Box as GtkBox;
use gtk4::{Button, EventControllerKey, Label, Orientation, TextView};

/// What the input bar calls when its controls are used.
pub trait InputActions {
    fn on_send(&self);
    fn on_stop(&self);
    fn on_toggle_model(&self);
}

pub struct InputBar {
    pub widget: GtkBox,
    pub text_view: TextView,
    pub send_button: Button,
    pub stop_button: Button,
    pub model_button: Button,
    pub hint: Label,
    pub placeholder: Label,
    scroller: gtk4::ScrolledWindow,
    busy: Cell<bool>,
    staged_count: Cell<usize>,
    has_text: Cell<bool>,
}

impl InputBar {
    pub fn build<A: InputActions + 'static>(actions: &Rc<A>) -> Rc<Self> {
        let widget = GtkBox::builder()
            .orientation(Orientation::Vertical)
            .spacing(4)
            .css_classes(["input-area"])
            .build();

        let hint = Label::builder()
            .halign(gtk4::Align::Start)
            .visible(false)
            .css_classes(["dim-label", "caption"])
            .build();

        let text_view = TextView::builder()
            .wrap_mode(gtk4::WrapMode::WordChar)
            .accepts_tab(true)
            .top_margin(8)
            .bottom_margin(8)
            .left_margin(10)
            .right_margin(10)
            .css_classes(["chat-input"])
            .build();

        let input_row = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(6)
            .css_classes(["glass-pill"])
            .build();

        let model_button = Button::builder()
            .label("⚡ quick")
            .tooltip_text("Toggle between the quick and deep model")
            .visible(false)
            .css_classes(["flat", "model-chip"])
            .build();
        let stop_button = Button::builder()
            .icon_name("media-playback-stop-symbolic")
            .tooltip_text("Stop the current turn (Esc hides the overlay)")
            .visible(false)
            .css_classes(["flat", "stop"])
            .build();
        let send_button = Button::builder()
            .icon_name("go-up-symbolic")
            .tooltip_text("Send (Enter)")
            .sensitive(false)
            .css_classes(["circular", "suggested-action", "send-circle"])
            .build();

        let magnifier = gtk4::Image::builder()
            .icon_name("edit-find-symbolic")
            .css_classes(["dim-label"])
            .valign(gtk4::Align::Center)
            .build();
        input_row.append(&magnifier);

        input_row.append(&model_button);
        let scroller = gtk4::ScrolledWindow::builder()
            .hexpand(true)
            .vscrollbar_policy(gtk4::PolicyType::External)
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .height_request(36)
            .build();
        scroller.set_child(Some(&text_view));

        // Spotlight-style placeholder shown while the entry is empty.
        let placeholder = Label::builder()
            .label("Ask OpenCode…")
            .halign(gtk4::Align::Start)
            .valign(gtk4::Align::Start)
            .margin_start(10)
            .margin_top(9)
            .css_classes(["dim-label"])
            .build();
        let overlay = gtk4::Overlay::new();
        overlay.set_child(Some(&scroller));
        overlay.add_overlay(&placeholder);
        placeholder.set_sensitive(false); // clicks fall through to the entry
        placeholder.set_visible(true);
        input_row.append(&overlay);
        {
            let action_row = GtkBox::builder()
                .orientation(Orientation::Horizontal)
                .spacing(4)
                .valign(gtk4::Align::Center)
                .build();
            action_row.append(&stop_button);
            action_row.append(&send_button);
            input_row.append(&action_row);
        }
        widget.append(&hint);
        widget.append(&input_row);

        // Enter sends; Shift+Enter keeps the newline.
        {
            let actions = actions.clone();
            let controller = EventControllerKey::new();
            controller.connect_key_pressed(move |_ctrl, keyval, _keycode, state| {
                let enter = keyval == gtk4::gdk::Key::Return
                    || keyval == gtk4::gdk::Key::KP_Enter;
                let newline = state
                    .intersects(gtk4::gdk::ModifierType::SHIFT_MASK
                        | gtk4::gdk::ModifierType::ALT_MASK);
                if enter && !newline {
                    actions.on_send();
                    return glib::Propagation::Stop;
                }
                // Shift+Enter and Alt+Enter fall through: the text view
                // inserts the newline natively.
                glib::Propagation::Proceed
            });
            text_view.add_controller(controller);
        }

        // Send on click; stop on click; model chip on click.
        {
            let actions = actions.clone();
            send_button.connect_clicked(move |_| actions.on_send());
        }
        {
            let actions = actions.clone();
            stop_button.connect_clicked(move |_| actions.on_stop());
        }
        {
            let actions = actions.clone();
            model_button.connect_clicked(move |_| actions.on_toggle_model());
        }

        // Send is enabled only with text and while idle; input changes are
        // reported so the controller can refresh chips.
        let bar = Rc::new(InputBar {
            widget,
            text_view: text_view.clone(),
            send_button: send_button.clone(),
            stop_button: stop_button.clone(),
            model_button: model_button.clone(),
            hint,
            placeholder: placeholder.clone(),
            scroller: scroller.clone(),
            busy: Cell::new(false),
            staged_count: Cell::new(0),
            has_text: Cell::new(false),
        });
        {
            let bar = bar.clone();
            text_view
                .buffer()
                .connect_changed(move |_| {
                    let buffer = bar.text_view.buffer();
                    let text = buffer
                        .text(&buffer.start_iter(), &buffer.end_iter(), false)
                        .trim()
                        .to_string();
                    let had = bar.has_text.get();
                    bar.has_text.set(!text.is_empty());
                    bar.placeholder.set_visible(text.is_empty());
                    bar.update_height();
                    if had != bar.has_text.get() {
                        bar.refresh_send();
                    }
                });
        }
        bar
    }

    /// Seed the input bar (debug IPC `send` seeds then triggers the send).
    pub fn set_text(&self, text: &str) {
        self.text_view.buffer().set_text(text);
        self.has_text.set(!text.is_empty());
        self.update_height();
        self.refresh_send();
    }

    pub fn text(&self) -> String {
        let buffer = self.text_view.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .trim()
            .to_string()
    }

    pub fn clear(&self) {
        self.text_view.buffer().set_text("");
        self.has_text.set(false);
        self.placeholder.set_visible(true);
        self.update_height();
        self.refresh_send();
    }

    /// Grow the input to fit the content: one line at the pill height,
    /// capping at five lines before it scrolls internally.
    fn update_height(&self) {
        let buffer = self.text_view.buffer();
        let (y, line_height) = self.text_view.line_yrange(&buffer.end_iter());
        const VERTICAL_CHROME: i32 = 8 + 8 + 2; // text margins + sliver
        const MIN: i32 = 36;
        const MAX: i32 = 140;
        let want = (y + line_height + VERTICAL_CHROME).clamp(MIN, MAX);
        self.scroller.set_size_request(-1, want);
    }

    pub fn focus(&self) {
        self.text_view.grab_focus();
        // Put the cursor at the end of any restored draft.
        let buffer = self.text_view.buffer();
        buffer.place_cursor(&buffer.end_iter());
    }

    pub fn set_busy(&self, busy: bool) {
        self.busy.set(busy);
        self.stop_button.set_visible(busy);
        self.refresh_send();
    }

    pub fn set_attachments(&self, count: usize) {
        self.staged_count.set(count);
        if count > 0 {
            self.hint
                .set_text(&format!("{count} image{plural} attached", plural = if count == 1 { "" } else { "s" }));
            self.hint.set_visible(true);
        } else {
            self.hint.set_visible(false);
        }
        self.refresh_send();
    }

    pub fn set_model_label(&self, label: &str, visible: bool) {
        self.model_button.set_label(label);
        self.model_button.set_visible(visible);
    }

    /// Whether a send is currently possible (text or attachments, not busy).
    pub fn can_send(&self) -> bool {
        let staged = self.staged_count.get() > 0;
        !self.busy.get() && (self.has_text.get() || staged)
    }

    fn refresh_send(&self) {
        self.send_button.set_sensitive(self.can_send());
    }
}
