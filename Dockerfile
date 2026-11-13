# syntax=docker/dockerfile:1
#
# Build insider without Rust on the host. See "Build in Docker" in README.md.
#
#   make build   throwaway build, binary extracted to ./out/<target>/
#   make dev     incremental build with the source mounted via -v
#
# Stages:
#   toolchain    Rust + zig + cargo-zigbuild + every release target, with the
#                crate dependencies pre-fetched (the image `make dev` runs)
#   build        compiles the sources copied into it
#   export       just the binary, for `docker build --output`

FROM rust:1-slim AS toolchain

# Leave empty for the latest zig, or pin one (e.g. 0.13.0) if a release breaks
ARG ZIG_VERSION=

RUN apt-get update \
 && apt-get install -y --no-install-recommends make python3-pip \
 && rm -rf /var/lib/apt/lists/* \
 && pip3 install --no-cache-dir --break-system-packages "ziglang${ZIG_VERSION:+==$ZIG_VERSION}" \
 && rustup component add rustfmt clippy \
 && rustup target add \
      aarch64-apple-darwin x86_64-apple-darwin \
      aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu \
      x86_64-pc-windows-gnu \
 && cargo install --locked cargo-zigbuild \
 && rm -rf "$CARGO_HOME/registry"

# Build output and caches live in /target: a named volume for `make dev`,
# a BuildKit cache mount for `make build`. Anyone may write there so the
# container can run as the host user (keeps files in ./out owned by you).
ENV CARGO_TARGET_DIR=/target \
    XDG_CACHE_HOME=/target/.cache \
    ZIG_GLOBAL_CACHE_DIR=/target/.cache/zig
RUN mkdir -p /target && chmod 1777 /target

# Pre-fetch dependencies into the image, like `npm ci` in a prebuild step
WORKDIR /src/snshn/insider
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
 && cargo fetch --locked \
 && rm -rf src \
 && chmod -R a+rwX "$CARGO_HOME"

CMD ["make", "BUILD"]


FROM toolchain AS build
ARG TARGET=aarch64-apple-darwin
COPY . .
RUN --mount=type=cache,target=/target,sharing=locked \
    make BUILD TARGET="$TARGET" OUT=/out


FROM scratch AS export
COPY --from=build /out/ /
