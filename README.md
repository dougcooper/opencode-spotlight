# opencode-spotlight

A spotlight-style chat overlay for [OpenCode](https://opencode.ai): press a
global hotkey anywhere on your desktop, a compact floating chat pops up on top
of your work — ask your LLM, get the answer, dismiss with `Esc` or by clicking
away. Built for GNOME, designed to work on any Linux desktop: everything
desktop-specific lives behind one small integration layer, and the hotkey
contract is a plain command.

## How it works

- The app talks to **your existing OpenCode background service** (the one your
  TUI/desktop app already use), so your providers, default agent, MCP servers,
  and sessions are exactly the ones you already have. Chats live in a fixed
  *scratch directory* (`~/.local/share/opencode-spotlight/scratch` by default)
  and re-open where you left off.
- Everything in the window is native GTK, except the chat transcript, which is
  a local WebKitGTK page (bundled with the binary, zero network access).

## Build & install

Building needs the GTK4 + libadwaita + WebKitGTK 6 development libraries
(`gtk4-devel`, `libadwaita-devel`, `webkitgtk6.0-devel` — on immutable Fedora
systems, build them in a toolbox container; the binary then runs on the host
against the same runtime).

```sh
cargo build --release
# or to install the binary:
cargo install --path .
```

Then register the desktop entry, autostart entry, and the GNOME keybinding:

```sh
opencode-spotlight install
```

`install` writes:

- `~/.local/share/applications/dev.dougcooper.opencode-spotlight.desktop`
  (D-Bus activatable — launching the app anywhere routes to the single
  resident instance)
- `~/.config/autostart/dev.dougcooper.opencode-spotlight.desktop`
  (born window-less at login; every hotkey press is instant)
- a GNOME custom keybinding (default `<Super>space`) that runs
  `opencode-spotlight --toggle`

`uninstall` reverses exactly those files and leaves everything else alone.

## The toggle contract (any desktop)

The overlay is driven by exactly one command:

```sh
opencode-spotlight --toggle     # show when hidden, hide when shown
```

Bind that command to any hotkey your desktop supports:

| Desktop | How |
|---|---|
| GNOME | Settings → Keyboard → Custom Shortcuts (or `opencode-spotlight install`) |
| KDE Plasma | System Settings → Shortcuts → Add Command → `opencode-spotlight --toggle` |
| Sway | `bindsym $mod+space exec opencode-spotlight --toggle` |
| Hyprland | `bind = $mainMod, Space, exec, opencode-spotlight --toggle` |
| XFCE | Keyboard → Application Shortcuts → add the command |

Window shape on other desktops: GNOME centers the popup; tiling compositors
(KDE, Sway, Hyprland) will tile it unless you add a floating rule for the
app id `dev.dougcooper.opencode-spotlight` — e.g. in sway:
`for_window [app_id="dev.dougcooper.opencode-spotlight"] floating enable`.

## Controls

- **Hotkey** — show/hide; **Esc**, click-away, and close all hide (the
  conversation keeps running in the service either way).
- **Enter** sends, **Shift+Enter** newline; paste text as usual.
- **Ctrl+V with a screenshot in the clipboard** stages it as an image
  attachment (shown as a removable chip).
- **⚡/🧠 chip** — flips the next message between your configured quick and
  deep models (see config).
- **Stop** — aborts the running turn.
- **Terminal button** — continues the current chat in the OpenCode TUI, spawned
  in the scratch directory.
- When a tool needs permission, the overlay asks with **Allow once / Allow
  always / Reject**, just like the TUI would.

## Configuration

`~/.config/opencode-spotlight/config.toml` (created with a commented template
on first launch):

```toml
scratch_dir = "~/.local/share/opencode-spotlight/scratch"
keybinding = "<Super>space"
handoff_command = "ptyxis --directory {dir} opencode"

[models]
quick = { id = "glm-5.3-flash", providerID = "ollama-cloud" }
deep  = { id = "glm-5.3",        providerID = "ollama-cloud" }
```

## Security notes

- The OpenCode service password lives in
  `~/.local/state/opencode/service.json`; the app reads it once per
  connection, sends it only as Basic auth, and never writes it to logs, the
  webview page, or a process command line.
- The transcript page is bundled in the binary and makes no network requests;
  model output is rendered with a sanitized markdown pipeline before it lands
  in the DOM.

## Acknowledgements

Vendored, MIT-licensed: [marked](https://github.com/markedjs/marked),
[DOMPurify](https://github.com/cure53/DOMPurify),
[highlight.js](https://github.com/highlightjs/highlight.js).
