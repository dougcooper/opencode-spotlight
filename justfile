# opencode-spotlight — task runner.
#
# Builds happen inside the spotlight-builder container (see Containerfile);
# run/toggle/install execute the built binary directly on the host.

set shell := ["bash", "-cu"]

bin := "./target/release/opencode-spotlight"

default:
    @just --list

# Rebuild the spotlight-builder container image (Containerfile)
image:
    podman build -t spotlight-builder .

# Run cargo inside the build container (mounts the repo)
cargo *args:
    podman run --rm --userns=keep-id --security-opt label=disable \
        -v "$PWD:/project" -w /project spotlight-builder cargo {{args}}

# Debug build
build:
    just cargo build

# Release build (what install/run/toggle use)
build-release:
    just cargo build --release

# Run the test suite (container)
test:
    just cargo test

# Clippy with warnings as errors (container)
lint:
    just cargo clippy --all-targets -- -D warnings

# Format code
fmt:
    just cargo fmt

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
