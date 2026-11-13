# Build environment for opencode-spotlight (Fedora 44).
#
# The image does a full cargo-chef build: dependency compilation lives in
# `cook-*` layers that only re-bake when Cargo.toml / Cargo.lock change, and
# app source changes only re-run the final build layer. The repo dir is no
# longer bind-mounted for builds — the image builds it, and the justfile
# extracts the binary to ./target/{release,debug}/ on the host (same runtime
# libs image and host ship, so the binary runs directly on any Fedora 44+
# desktop).
#
# Stage graph (dependency order; only the target's chain is ever built):
#   toolchain     fedora:44 + -devel libs + cargo-chef
#   planner       manifests + build.rs only -> recipe.json.  Deliberately NOT
#                 the source: a changing planner layer would re-bake kitchen.
#   cook-dev      cargo chef cook (dev profile) — parent of test/lint/debug
#   test          cargo test
#   lint          cargo clippy --all-targets -- -D warnings
#   debug         cargo build (dev profile)
#   cook-release  cargo chef cook --release — parent of builder
#   builder       full release build (last stage: default `podman build`
#                 never builds the dev chain)
#
# `just image` tags builder as spotlight-builder; test/lint/debug are tagged
# spotlight-test / spotlight-lint / spotlight-debug. Retagging on rebuild
# leaves old untagged images behind — `podman image prune -f` cleans up.

FROM registry.fedoraproject.org/fedora:44 AS toolchain
RUN dnf -y --setopt=install_weak_deps=False install \
        cargo \
        rust \
        gcc \
        pkgconf-pkg-config \
        gtk4-devel \
        libadwaita-devel \
        webkitgtk6.0-devel \
        clippy \
        rustfmt \
        strace \
        git \
    && dnf clean all
RUN cargo install cargo-chef --locked
ENV PATH="/root/.cargo/bin:${PATH}"

FROM toolchain AS planner
WORKDIR /project
COPY Cargo.toml Cargo.lock build.rs ./
# cargo metadata (what prepare runs) insists a bin target exists; the stub
# never lands in recipe.json, so app source changes can't re-bake the cooks.
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs \
    && cargo chef prepare --recipe-path recipe.json

FROM toolchain AS cook-dev
WORKDIR /project
COPY --from=planner /project/recipe.json recipe.json
RUN cargo chef cook --recipe-path recipe.json

FROM cook-dev AS test
COPY . .
RUN cargo test --locked

FROM cook-dev AS lint
COPY . .
RUN cargo clippy --all-targets --locked -- -D warnings

FROM cook-dev AS debug
COPY . .
RUN cargo build --locked

FROM toolchain AS cook-release
WORKDIR /project
COPY --from=planner /project/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

FROM cook-release AS builder
COPY . .
RUN cargo build --release --locked
