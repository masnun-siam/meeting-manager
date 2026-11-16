#!/usr/bin/env bash
# Installs the host tools the Tauri crate's gates need (macOS only): cargo-llvm-cov 0.9.1 into
# ~/.cargo/bin, checked against the publisher's SHA-256. Rust itself comes from rust-toolchain.toml.
set -euo pipefail
version=0.9.1
case $(uname -sm) in
  "Darwin arm64") triple=aarch64-apple-darwin; sum=7e821a6c90c0884f79f759f5d9be50799ade67bb82515186d922f26032ef43b8 ;;
  "Darwin x86_64") triple=x86_64-apple-darwin; sum=ddf949b82a63017441ea76aa068bdd7e2dde8055f47cd3ef736fdbbd633bc33e ;;
  *) echo "the desktop app builds only on macOS" >&2; exit 1 ;;
esac
bin=$HOME/.cargo/bin
if [ "$("$bin/cargo-llvm-cov" llvm-cov --version 2>/dev/null)" = "cargo-llvm-cov $version" ]; then
  echo "cargo-llvm-cov $version already installed"
  exit 0
fi
tmp=$(mktemp -d)
curl -fsSLo "$tmp/cov.tgz" "https://github.com/taiki-e/cargo-llvm-cov/releases/download/v$version/cargo-llvm-cov-$triple.tar.gz"
echo "$sum  $tmp/cov.tgz" | shasum -a 256 -c - >/dev/null
mkdir -p "$bin"
tar -xzf "$tmp/cov.tgz" -C "$bin" cargo-llvm-cov
rm -f "$tmp/cov.tgz"
rmdir "$tmp"
echo "installed $("$bin/cargo-llvm-cov" llvm-cov --version)"
