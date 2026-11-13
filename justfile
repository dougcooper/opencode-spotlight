# opencode-spotlight — task runner.
#
# Builds happen inside the spotlight-builder image (see Containerfile), which
# uses cargo-chef to keep dependency compilation in cached image layers —
# source changes only recompile the app. The binary is built in the image and
# extracted to ./target/{release,debug}/ on the host, so run/toggle/install
# execute it on the host directly, as before.
#
# test/lint run at image-build time (only the final layer re-runs on source
# changes). fmt is the one bind-mount run: formatting needs no compilation.

set shell := ["bash", "-cu"]

bin := "./target/release/opencode-spotlight"

default:
    @just --list

# Rebuild the spotlight-builder image (default stage = release builder)
image:
    podman build -t spotlight-builder .

# Build the release binary in the image, extract it to ./target/release
build-release:
    podman build --target builder -t spotlight-builder .
    mkdir -p target/release
    podman run --rm spotlight-builder cat /project/target/release/opencode-spotlight > target/release/opencode-spotlight
    chmod +x target/release/opencode-spotlight

# Debug build (reuses the dev-profile chef layers)
build:
    podman build --target debug -t spotlight-debug .
    mkdir -p target/debug
    podman run --rm spotlight-debug cat /project/target/debug/opencode-spotlight > target/debug/opencode-spotlight
    chmod +x target/debug/opencode-spotlight

# Run the test suite (image-build time; chef-cached)
test:
    podman build --target test -t spotlight-test .

# Clippy with warnings as errors (image-build time; chef-cached)
lint:
    podman build --target lint -t spotlight-lint .

# Format code (bind-mount run; no compilation involved)
fmt:
    podman run --rm --userns=keep-id --security-opt label=disable \
        -v "$PWD:/project" -w /project spotlight-builder cargo fmt

# Make sure a binary exists (release), building it if not
[private]
ensure-bin:
    @test -x {{bin}} || just build-release

# Start the resident app in the background (no window until summoned)
run: ensure-bin
    setsid sh -c 'exec {{bin}} >/dev/null 2>&1' &

# Summon or hide the overlay
toggle: ensure-bin
    {{bin}} --toggle

# Register desktop entry + autostart + hotkey (host)
install: ensure-bin
    {{bin}} install

# Remove everything install created (host)
uninstall: ensure-bin
    {{bin}} uninstall

# Stop the resident app
stop:
    @pkill -f "opencode[-]spotlight" 2>/dev/null || true

# Print local paths (config/state/scratch)
paths: ensure-bin
    @{{bin}} --version
