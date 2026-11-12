//! The GTK application: single instance, D-Bus activated, command-line
//! toggle, autostart-friendly (hold without any window).

use std::cell::RefCell;
use std::rc::Rc;

use libadwaita as adw;
use adw::prelude::*;
use gtk4::glib;

use crate::state::{Config, Paths};
use crate::ui::controller::UiContext;

pub const APP_ID: &str = "dev.dougcooper.opencode-spotlight";

/// Run the GTK application loop. Install/uninstall never reach here.
pub fn run() -> i32 {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    let context: Rc<RefCell<Option<Rc<UiContext>>>> = Rc::new(RefCell::new(None));

    {
        let context = context.clone();
        let app_c = app.clone();
        app.connect_startup(move |_| {
            gio::resources_register_include!("resources.gresource")
                .expect("the bundled webview resources must compile in");

            let paths = Paths::detect();
            let config = Config::load(&paths);
            let ui = UiContext::start(paths, config);
            ui.set_app(&app_c);
            *context.borrow_mut() = Some(ui.clone());

            if std::env::var("SPOTLIGHT_DEBUG_IPC").as_deref() == Ok("1") {
                crate::ui::debug_ipc::activate(ui);
            }

            // Only the primary stays resident; secondaries must exit after
            // routing their command line to the primary (registration is
            // complete by the time the startup handler runs).
            if !app_c.is_remote() {
                std::mem::forget(app_c.hold());
            }
        });
    }

    {
        let context = context.clone();
        app.connect_command_line(move |_app, cmdline| {
            let args = cmdline.arguments();
            let toggle = args
                .iter()
                .any(|arg| arg.to_string_lossy() == "--toggle");
            if let Some(ui) = context.borrow().as_ref() {
                if toggle {
                    UiContext::show_or_toggle(ui, true);
                } else if cmdline.is_remote() {
                    // A secondary launch without arguments: show, like
                    // activation.
                    UiContext::show_or_toggle(ui, false);
                }
                // A local launch with no arguments is the autostart path:
                // stay window-less.
            }
            glib::ExitCode::SUCCESS
        });
    }

    {
        let context = context.clone();
        app.connect_activate(move |_app| {
            if let Some(ui) = context.borrow().as_ref() {
                UiContext::show_or_toggle(ui, false);
            }
        });
    }

    app.run().into()
}
