//! Staged image attachments shown as removable chips above the input bar.
//! Bytes are encoded to PNG when staged and turned into inline data-URI
//! attachments when the message is sent.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::Box as GtkBox;
use gtk4::{gdk, Button, Label, Orientation};

pub const MAX_ATTACHMENTS: usize = 4;

struct Core {
    container: GtkBox,
    chips: GtkBox,
    staged: RefCell<Vec<StagedImage>>,
    counter: Cell<usize>,
}

struct StagedImage {
    png: Vec<u8>,
    name: String,
    chip: GtkBox,
}

pub struct AttachmentTray {
    core: Rc<Core>,
}

impl AttachmentTray {
    pub fn build() -> Rc<Self> {
        let container = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(4)
            .visible(false)
            .css_classes(["attachments"])
            .build();
        let chips = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(4)
            .valign(gtk4::Align::Center)
            .build();
        container.append(&chips);
        let core = Rc::new(Core {
            container,
            chips,
            staged: RefCell::new(Vec::new()),
            counter: Cell::new(0),
        });
        Rc::new(Self { core })
    }

    pub fn widget(&self) -> &GtkBox {
        &self.core.container
    }

    pub fn len(&self) -> usize {
        self.core.staged.borrow().len()
    }

    /// Encode a clipboard texture into the tray as a PNG chip.
    /// Returns the display name on success.
    pub fn add_from_texture(&self, texture: &gdk::Texture) -> Result<String, String> {
        if self.len() >= MAX_ATTACHMENTS {
            return Err(format!("At most {MAX_ATTACHMENTS} images per message."));
        }
        #[allow(deprecated)] // 4.12 deprecates the helper; the replacement needs raw memory plumbing
        let Some(pixbuf) = gdk::pixbuf_get_from_texture(texture) else {
            return Err("Could not decode the clipboard image.".into());
        };
        let png = pixbuf
            .save_to_bufferv("png", &[])
            .map_err(|e| format!("Could not encode the image: {e}"))?;

        let n = self.core.counter.get() + 1;
        self.core.counter.set(n);
        let size_kb = png.len().div_ceil(1024);
        let name = format!("clipboard-{n}.png ({size_kb} KB)");

        let chip = GtkBox::builder()
            .orientation(Orientation::Horizontal)
            .spacing(2)
            .valign(gtk4::Align::Center)
            .css_classes(["attachment-chip"])
            .build();
        chip.append(&Label::builder().label(&name).build());
        let remove = Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Remove attachment")
            .css_classes(["flat"])
            .build();
        chip.append(&remove);
        chip.set_margin_start(2);
        chip.set_margin_end(2);

        {
            let core = self.core.clone();
            let chip_key = chip.clone();
            remove.connect_clicked(move |_| {
                core.staged
                    .borrow_mut()
                    .retain(|staged| staged.chip != chip_key);
                core.chips.remove(&chip_key);
                if core.staged.borrow().is_empty() {
                    core.container.set_visible(false);
                }
            });
        }

        self.core
            .staged
            .borrow_mut()
            .push(StagedImage { png, name: name.clone(), chip: chip.clone() });
        self.core.chips.append(&chip);
        self.core.container.set_visible(true);
        Ok(name)
    }

    /// Take all staged images for sending and clear the tray.
    pub fn take_all(&self) -> Vec<(Vec<u8>, String)> {
        let items = self
            .core
            .staged
            .borrow_mut()
            .drain(..)
            .map(|s| (s.png, s.name))
            .collect();
        self.drain_chips();
        self.core.container.set_visible(false);
        items
    }

    /// Non-consuming view of the staged attachments as `(png, name)`.
    pub fn staged(&self) -> Vec<(Vec<u8>, String)> {
        self.core
            .staged
            .borrow()
            .iter()
            .map(|s| (s.png.clone(), s.name.clone()))
            .collect()
    }

    /// Clear the tray (used when staged images are rejected).
    pub fn clear(&self) {
        self.take_all();
    }

    fn drain_chips(&self) {
        while let Some(child) = self.core.chips.first_child() {
            self.core.chips.remove(&child);
        }
    }
}

