use glib_build_tools::compile_resources;

fn main() {
    // Bundle the webview page (styles, scripts, vendored JS libs) into a
    // gresource compiled into the binary. The webview loads everything from
    // `resource:///dev/dougcooper/opencode-spotlight/webview/...` and makes no
    // network requests.
    compile_resources(
        &["assets"],
        "assets/resources.gresource.xml",
        "resources.gresource",
    );
}
