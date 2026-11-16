# Toolchain for local hooks and `make check`: the `meeting` crate, the extension and the desktop UI.
# The Tauri crate (desktop/src-tauri) links macOS frameworks and runs on the host instead.
# Rust matches rust-toolchain.toml; Node matches .nvmrc and CI. Bump the pins together.
FROM node:24-bookworm@sha256:9d266751ca3d7fdbe651c878ffd5ad0a42fed49a18c283509267c97f025b7496 AS node

FROM rust:1.99.1-bookworm@sha256:e6f2cff35d0f309acf8433b4f05878ed77092602c5065cc81801177da0bb3a9c
RUN apt-get update \
 && apt-get install -y --no-install-recommends ffmpeg python3 \
 && rm -rf /var/lib/apt/lists/*
RUN rustup component add rustfmt clippy llvm-tools-preview
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s ../lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
 && ln -s ../lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx

# Release binaries, checked against the publishers' SHA-256 sums.
RUN set -eu; \
    arch=$(dpkg --print-architecture); \
    case "$arch" in \
      arm64) cov=aarch64; cov_sum=abf5f13c1520f8756d2192bfaaeadb0208f5b7eaa8cadc15bf847befa42b7360; \
             gl=arm64; gl_sum=e4a487ee7ccd7d3a7f7ec08657610aa3606637dab924210b3aee62570fb4b080 ;; \
      amd64) cov=x86_64; cov_sum=b3f68e625481fed9b16444174f3fa5ebcdbde4a1878803a35eabe2dcefcdc41a; \
             gl=x64; gl_sum=551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb ;; \
      *) echo "unsupported arch $arch" >&2; exit 1 ;; \
    esac; \
    cd /tmp; \
    curl -fsSLo cov.tgz "https://github.com/taiki-e/cargo-llvm-cov/releases/download/v0.9.1/cargo-llvm-cov-$cov-unknown-linux-gnu.tar.gz"; \
    echo "$cov_sum  cov.tgz" | sha256sum -c -; \
    tar -xzf cov.tgz -C /usr/local/cargo/bin; \
    curl -fsSLo gl.tgz "https://github.com/gitleaks/gitleaks/releases/download/v8.30.1/gitleaks_8.30.1_linux_$gl.tar.gz"; \
    echo "$gl_sum  gl.tgz" | sha256sum -c -; \
    tar -xzf gl.tgz -C /usr/local/bin gitleaks; \
    rm -f cov.tgz gl.tgz

# Cache volumes start as copies of these folders, so a non-root hook user can write to them.
RUN mkdir -p /cache/cargo /cache/target /cache/npm /cache/home \
 && chmod 1777 /cache /cache/cargo /cache/target /cache/npm /cache/home
ENV CARGO_HOME=/cache/cargo CARGO_TARGET_DIR=/cache/target npm_config_cache=/cache/npm HOME=/cache/home \
    PATH=/usr/local/cargo/bin:$PATH CI=1
