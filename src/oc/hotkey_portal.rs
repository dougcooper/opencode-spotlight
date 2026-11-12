//! The Super+Space hotkey via the xdg-desktop-portal GlobalShortcuts
//! interface.
//!
//! On GNOME 45+ this is the sanctioned mechanism — the desktop's
//! media-keys custom-shortcut registry is dead in GNOME 50 (the
//! `gsd-media-keys` component no longer exists, and GNOME Shell's JS no
//! longer consumes `custom-keybindings`). The portal also shows the user a
//! single authorization dialog, then the compositor binds the key natively.

use futures_util::StreamExt;
use tokio::time::{sleep, Duration};

use ashpd::desktop::CreateSessionOptions;
use crate::oc::service::UiEvent;

const SHORTCUT_ID: &str = "toggle";

/// Try to bind the global shortcut; on success, stream activations into the
/// backend. Retries a few times so a slow-starting portal at login still
/// binds. Returns quietly when the portal never becomes available.
pub async fn run(
    ui_tx: async_channel::Sender<UiEvent>,
    trigger: String,
) -> Result<(), String> {
    let mut attempts = 0;
    let shortcuts = loop {
        match ashpd::desktop::global_shortcuts::GlobalShortcuts::new().await {
            Ok(shortcuts) => break shortcuts,
            Err(e) => {
                attempts += 1;
                if attempts >= 4 {
                    return Err(format!("global shortcuts portal unavailable: {e}"));
                }
                sleep(Duration::from_secs(20)).await;
            }
        }
    };

    let session = shortcuts.create_session(CreateSessionOptions::default())
        .await
        .map_err(|e| format!("global shortcuts session failed: {e}"))?;

    // bind_shortcuts().await already includes the authorization-dialog
    // response (ashpd's Proxy::request joins the response signal); errors
    // propagate here, and the Activated stream below carries activations.
    shortcuts
        .bind_shortcuts(
            &session,
            &[ashpd::desktop::global_shortcuts::NewShortcut::new(
                SHORTCUT_ID,
                "Toggle the OpenCode Spotlight overlay",
            )
            .preferred_trigger(trigger.as_str())],
            None,
            ashpd::desktop::global_shortcuts::BindShortcutsOptions::default(),
        )
        .await
        .map_err(|e| format!("global shortcut binding failed: {e}"))?;

    eprintln!(
        "opencode-spotlight: global shortcut bound via portal ({SHORTCUT_ID}, waiting for Activated events)"
    );

    let mut activated = shortcuts
        .receive_activated()
        .await
        .map_err(|e| format!("global shortcut stream failed: {e}"))?;

    while let Some(event) = activated.next().await {
        if event.shortcut_id() == SHORTCUT_ID {
            let _ = ui_tx.send(UiEvent::ToggleHotkey).await;
        }
    }
    Ok(())
}
