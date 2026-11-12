//! The input row: the glass text pill, with the stop/send controls beside it
//! (outside the pill) and the model chip living in the status bar.

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
    pub placeholder: Label,
    scroller: gtk4::ScrolledWindow,
    busy: Cell<bool>,
    staged_count: Cell<usize>,
    has_text: Cell<bool>,
}

impl InputBar {
    pub fn build<A: InputActions + 'static>(actions: &Rc<A>) -> Rc<Self> {
        let widget = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(6)
            .css_classes(["input-area"])
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

        // The text pill: magnifier + the scrolling entry.
        let pill = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(6)
            .hexpand(true)
            .css_classes(["glass-pill"])
            .build();

        let magnifier = gtk4::Image::builder()
            .icon_name("edit-find-symbolic")
            .css_classes(["dim-label"])
            .valign(gtk4::Align::Center)
            .build();
        pill.append(&magnifier);

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
        pill.append(&overlay);

        // Actions beside the pill: stop, then the accent send circle.
        let stop_button = Button::builder()
            .icon_name("media-playback-stop-symbolic")
            .tooltip_text("Stop the current turn (Esc hides the overlay)")
            .visible(false)
            .css_classes(["flat", "stop"])
            .build();
        let send_button = Button::builder()
            .icon_name("go-up-symbolic")
            .tooltip_text("Send (Enter, Shift+Enter/Alt+Enter for a newline)")
            .sensitive(false)
            .css_classes(["circular", "suggested-action", "send-circle"])
            .build();
        let action_row = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(4)
            .valign(gtk4::Align::Center)
            .build();
        action_row.append(&stop_button);
        action_row.append(&send_button);

        widget.append(&pill);
        widget.append(&action_row);

        // Enter sends; Shift+Enter and Alt+Enter insert a newline.
        {
            let actions = actions.clone();
            let controller = EventControllerKey::new();
            controller.connect_key_pressed(move |_ctrl, keyval, _keycode, state| {
                let enter =
                    keyval == gtk4::gdk::Key::Return || keyval == gtk4::gdk::Key::KP_Enter;
                let newline = state
                    .intersects(gtk4::gdk::ModifierType::SHIFT_MASK
                        | gtk4::gdk::ModifierType::ALT_MASK);
                if enter && !newline {
                    actions.on_send();
                    return glib::Propagation::Stop;
                }
                // Shift/Alt+Enter fall through: the text view inserts the
                // newline natively.
                glib::Propagation::Proceed
            });
            text_view.add_controller(controller);
        }

        // Clicks on the controls.
        {
            let actions = actions.clone();
            send_button.connect_clicked(move |_| actions.on_send());
        }
        {
            let actions = actions.clone();
            stop_button.connect_clicked(move |_| actions.on_stop());
        }

        // Send is enabled only with text and while idle; the placeholder and
        // height track the buffer.
        let bar = Rc::new(InputBar {
            widget,
            text_view: text_view.clone(),
            send_button: send_button.clone(),
            stop_button: stop_button.clone(),
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
        self.refresh_send();
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
