# Build environment for opencode-spotlight (Fedora 44).
#
# The build mounts the repo at /project; artifacts land in ./target on the
# host, and the resulting binary runs directly on a Fedora 44+ desktop (same
# GTK/libadwaita/WebKitGTK runtime libs the image builds against).
#
# Usage (rootless podman; --userns=keep-id keeps file ownership yours, and
# label=disable avoids SELinux relabeling of the repo):
#
#   podman build -t spotlight-builder .
#   podman run --rm --userns=keep-id --security-opt label=disable \
#       -v "$PWD:/project" -w /project spotlight-builder cargo build --release
#   podman run --rm --userns=keep-id --security-opt label=disable \
#       -v "$PWD:/project" -w /project spotlight-builder cargo test
#
# Optional: keep the cargo registry cache between runs by adding
#   -v spotlight-cargo:/root/.cargo/registry

FROM registry.fedoraproject.org/fedora:44

RUN dnf -y --setopt=install_weak_deps=False install \
        cargo \
        rust \
        gcc \
        pkgconf-pkg-config \
        gtk4-devel \
        libadwaita-devel \
        webkitgtk6.0-devel \
        strace \
        git \
    && dnf clean all

WORKDIR /project
