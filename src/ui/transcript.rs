//! The transcript webview: loads a bundled page (gresource), exchanges
//! messages over the WebKitGTK script-message bridge, and buffers outbound
//! messages until the page is ready.
//!
//! The page makes no network requests and receives no credentials: all data
//! crosses this module as JSON.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use gio;
use javascriptcore6::Value as JscValue;
use webkit6::prelude::*;
use webkit6::{LoadEvent, UserContentManager, WebView};

use crate::bridge::{BridgeIn, BridgeOut};

pub const PAGE_URI: &str = "spotlight:///index.html";
const HANDLER_NAME: &str = "bridge";
const SCHEME: &str = "spotlight";
const RESOURCE_PREFIX: &str = "/dev/dougcooper/opencode-spotlight/webview";

/// Allow-listed page files served via the custom scheme; anything else 404s.
const PAGE_FILES: &[(&str, &str)] = &[
    ("index.html", "text/html"),
    ("style.css", "text/css"),
    ("app.js", "application/javascript"),
    ("vendor/marked.min.js", "application/javascript"),
    ("vendor/purify.min.js", "application/javascript"),
    ("vendor/highlight.min.js", "application/javascript"),
];

static SCHEME_REGISTERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Register the `spotlight://` scheme once. The web process cannot see
/// resources registered in the UI process, so the handler serves embedded
/// bundle bytes per request instead.
fn ensure_scheme_registered(webview: &WebView) {
    if SCHEME_REGISTERED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    webview
        .web_context()
        .expect("the webview always has a web context")
        .register_uri_scheme(SCHEME, |request| {
        let path = request.path().unwrap_or_default();
        let rel = path.trim_start_matches('/');
        let Some((_, content_type)) = PAGE_FILES.iter().find(|(name, _)| *name == rel) else {
            eprintln!("opencode-spotlight: spotlight request for unknown path: {rel}");
            return;
        };
        let resource = format!("{RESOURCE_PREFIX}/{rel}");
        match gio::resources_lookup_data(&resource, gio::ResourceLookupFlags::NONE) {
            Ok(bytes) => {
                let stream = gio::MemoryInputStream::from_bytes(&bytes);
                let response = webkit6::URISchemeResponse::new(&stream, bytes.len() as i64);
                response.set_content_type(content_type);
                request.finish_with_response(&response);
            }
            Err(e) => {
                eprintln!("opencode-spotlight: spotlight/{rel} lookup failed: {e}");
            }
        }
    });
}

pub type CopySink = Box<dyn Fn(&str)>;

/// Shared state the page-facing signals can hold a clone of.
struct Core {
    webview: RefCell<Option<WebView>>,
    ready: Cell<bool>,
    queue: RefCell<VecDeque<BridgeIn>>,
    on_copy: CopySink,
}

pub struct Transcript {
    pub webview: WebView,
    core: Rc<Core>,
}

impl Transcript {
    /// Build the webview, wire the bridge, and start loading the page.
    /// `on_copy` receives code-block copy requests from the page.
    pub fn new(on_copy: CopySink) -> Self {
        let webview = WebView::new();
        ensure_scheme_registered(&webview);
        let core = Rc::new(Core {
            webview: RefCell::new(None),
            ready: Cell::new(false),
            queue: RefCell::new(VecDeque::new()),
            on_copy,
        });

        let manager: UserContentManager = webview
            .user_content_manager()
            .expect("the webview always has a user content manager");

        // Surface page-load failures on stderr during bring-up.
        webview.connect_load_failed(|_webview, _event, uri, error| {
            eprintln!("opencode-spotlight: webview load failed: {uri}: {error}");
            false
        });

        // JS -> Rust. The page posts a JSON payload string to
        // `window.webkit.messageHandlers.bridge.postMessage(...)`.
        // The signal MUST be connected *before* the handler is registered
        // (WebKitGTK docs); the signal carries a JavaScriptCore value.
        {
            let core = core.clone();
            manager.connect_closure(
                "script-message-received::bridge",
                false,
                glib::closure_local!(move |_emitter: UserContentManager, value: JscValue| {
                    // The page sends an already-stringified JSON payload.
                    let payload = if value.is_string() {
                        value.to_str().to_string()
                    } else {
                        value.to_json(0).map(|s| s.to_string()).unwrap_or_default()
                    };
                    let Ok(out) = serde_json::from_str::<BridgeOut>(&payload) else {
                        return;
                    };
                    match out {
                        BridgeOut::Copy { text } => (core.on_copy)(&text),
                    }
                }),
            );
        }

        if !manager.register_script_message_handler(HANDLER_NAME, None) {
            eprintln!("opencode-spotlight: could not register the bridge message handler");
        }

        // Page readiness: buffer everything sent before load completes.
        {
            let core = core.clone();
            webview.connect_load_changed(move |_webview, event| {
                if event == LoadEvent::Finished {
                    core.ready.set(true);
                    for payload in core.queue.borrow_mut().drain(..) {
                        Self::dispatch(&core, &payload);
                    }
                }
            });
        }

        webview.load_uri(PAGE_URI);
        *core.webview.borrow_mut() = Some(webview.clone());

        Self { webview, core }
    }

    /// Push a message into the page, buffering until it has loaded.
    pub fn send(&self, payload: &BridgeIn) {
        if self.core.ready.get() {
            Self::dispatch(&self.core, payload);
        } else {
            self.core.queue.borrow_mut().push_back(payload.clone());
        }
    }

    fn dispatch(core: &Rc<Core>, payload: &BridgeIn) {
        let Ok(json) = serde_json::to_string(payload) else {
            eprintln!("opencode-spotlight: bridge serialization failed");
            return;
        };
        let script = format!("window.__bridge.receive({json});");
        if let Some(webview) = core.webview.borrow().as_ref() {
            webview.evaluate_javascript(&script, None, None, None::<&gio::Cancellable>, |_| {});
        }
    }
}

