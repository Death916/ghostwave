#!/usr/bin/env bash
# Build static musl ghostd binaries into assets/.
#
# Why static musl: NixOS remotes have no /lib64 dynamic loader, so the
# daemon must be a self-contained static binary to run from ~/.cache
# after an ssh-stdin upload with zero installation.
#
# Primary path (any distro with rustup): two commands.
#   rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
#   cargo build --release --target <target>   (with the flags below)
# rustup's musl targets bundle musl libc + rust-lld, so no C toolchain
# is needed anywhere.
#
# NixOS path: nixpkgs rustc can't add targets, so this script falls back
# to the official prebuilt rustc + rust-std components + patchelf (the
# same components rustup uses). Requires: curl, tar, patchelf.
#
# Usage: bash scripts/build-ghostd.sh [x86_64|aarch64|both]  (default: both)
set -euo pipefail

RUST_VERSION="1.86.0"
RUST_DATE="2025-04-03"
DIST="https://static.rust-lang.org/dist/${RUST_DATE}"
# Toolchain cache lives IN the project (ghostd/.toolchain) — /tmp gets wiped,
# forcing a full 160MB re-download + patchelf pass on every fresh boot.
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORK="$SCRIPT_DIR/../ghostd/.toolchain"

declare -A HASHES=(
  ["rustc-${RUST_VERSION}-x86_64-unknown-linux-gnu"]="4438b809ce4a083af31ed17aeeedcc8fc60ccffc0625bef1926620751b6989d7"
  ["cargo-${RUST_VERSION}-x86_64-unknown-linux-gnu"]="c5c1590f7e9246ad9f4f97cfe26ffa92707b52a769726596a9ef81565ebd908b"
  ["rust-std-${RUST_VERSION}-x86_64-unknown-linux-musl"]="8313c108da105181d27a5a0190ab46d1ac5b3e32bb5b2e7e0a41b7b5d353cba6"
  ["rust-std-${RUST_VERSION}-aarch64-unknown-linux-musl"]="4ce6d1e5ec3678311e08d3104e61266ac4a129bbef8d3083fe6cda7d92934e58"
  ["rust-std-${RUST_VERSION}-x86_64-unknown-linux-gnu"]="67be7184ea388d8ce0feaf7fdea46f1775cfc2970930264343b3089898501d37"
)

fetch() { # name
  local f="$WORK/$1.tar.xz"
  if [ ! -f "$f" ]; then
    mkdir -p "$WORK"
    # --retry + resume: a stalled connection used to hang the build forever.
    curl -sfL --retry 10 --retry-all-errors --retry-delay 2 --connect-timeout 15 \
      -C - "$DIST/$1.tar.xz" -o "$f"
  fi
  echo "${HASHES[$1]}  $f" | sha256sum -c - >/dev/null
}

rustup_build() { # target
  local target="$1"
  rustup target add "$target"
  RUSTFLAGS="-C linker=rust-lld -C relocation-model=static" \
    cargo build --release --target "$target"
  local out="ghostd/target/$target/release/ghostd"
  cp "$out" "assets/ghostd-${target%%-*}"
  echo "built $out -> assets/ghostd-${target%%-*}"
}

nixos_setup() { # sets SYSROOT, CARGO_DIR
  local tc="$WORK/toolchain"
  mkdir -p "$tc"
  SYSROOT="$tc/rustc-${RUST_VERSION}-x86_64-unknown-linux-gnu/rustc"
  CARGO_DIR="$tc/cargo-${RUST_VERSION}-x86_64-unknown-linux-gnu/cargo"
  # Toolchain already extracted + patchelf'd → reuse (skips the download
  # and the ~1min patchelf pass over hundreds of binaries). The guard is
  # a REAL run test: rustc --version fails if the rpath is broken.
  if [ -x "$SYSROOT/bin/rustc" ] && [ -x "$CARGO_DIR/bin/cargo" ] \
     && "$SYSROOT/bin/rustc" --version >/dev/null 2>&1; then
    echo "toolchain cached at $WORK — skipping extract/patchelf"
    return
  fi
  fetch "rustc-${RUST_VERSION}-x86_64-unknown-linux-gnu"
  fetch "cargo-${RUST_VERSION}-x86_64-unknown-linux-gnu"
  fetch "rust-std-${RUST_VERSION}-x86_64-unknown-linux-gnu"
  tar -xf "$WORK/rustc-${RUST_VERSION}-x86_64-unknown-linux-gnu.tar.xz" -C "$tc"
  tar -xf "$WORK/cargo-${RUST_VERSION}-x86_64-unknown-linux-gnu.tar.xz" -C "$tc"
  tar -xf "$WORK/rust-std-${RUST_VERSION}-x86_64-unknown-linux-gnu.tar.xz" -C "$tc"
  chmod -R u+w "$SYSROOT" "$CARGO_DIR"
  local interp rpath
  interp=$(ls /nix/store/*glibc*/lib/ld-linux-x86-64.so.2 2>/dev/null | head -1)
  # rustc/cargo also dlopen zlib + libstdc++/libgcc_s, which live OUTSIDE
  # the glibc dir — without them the toolchain dies with
  # "libz.so.1: cannot open shared object file".
  local extra=""
  for lib in libz.so.1 libstdc++.so.6 libgcc_s.so.1; do
    extra="$extra:$(find /nix/store -maxdepth 3 -name "$lib" -printf '%h\n' -quit 2>/dev/null)"
  done
  rpath="$(dirname "$interp"):$SYSROOT/lib$extra"
  for f in $(find "$SYSROOT/bin" "$SYSROOT/lib" "$SYSROOT/libexec" \
                  "$SYSROOT/lib/rustlib/x86_64-unknown-linux-gnu/bin" \
                  "$CARGO_DIR" -type f); do
    patchelf --set-interpreter "$interp" "$f" 2>/dev/null || true
    patchelf --add-rpath "$rpath" "$f" 2>/dev/null || true
  done
}

nixos_build() { # target
  local target="$1"
  local std="rust-std-${RUST_VERSION}-$target"
  fetch "$std"
  tar -xf "$WORK/$std.tar.xz" -C "$WORK"
  cp -rL "$WORK/$std/rust-std-$target/lib/rustlib/$target" "$SYSROOT/lib/rustlib/"
  local sc="$SYSROOT/lib/rustlib/$target/lib/self-contained"
  for a in libpthread.a libm.a libdl.a libutil.a librt.a; do
    ln -sf libc.a "$sc/$a"
  done
  ln -sf libunwind.a "$sc/libgcc_s.a"
  export PATH="$SYSROOT/bin:$CARGO_DIR/bin:$PATH"
  export RUSTC="$SYSROOT/bin/rustc"
  export RUSTFLAGS="-C relocation-model=static \
-C linker=$SYSROOT/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-lld \
-L native=$SYSROOT/lib/rustlib/$target/lib/self-contained"
  (cd ghostd && cargo build --release --target "$target" --offline)
  cp "ghostd/target/$target/release/ghostd" "assets/ghostd-${target%%-*}"
  echo "built -> assets/ghostd-${target%%-*}"
}

cd "$(dirname "$0")/.."
if command -v rustup >/dev/null 2>&1; then
  case "${1:-both}" in
    x86_64) rustup_build x86_64-unknown-linux-musl ;;
    aarch64) rustup_build aarch64-unknown-linux-musl ;;
    *) rustup_build x86_64-unknown-linux-musl; rustup_build aarch64-unknown-linux-musl ;;
  esac
else
  echo "no rustup found — using the official-toolchain (NixOS) path"
  command -v patchelf >/dev/null || { echo "need patchelf"; exit 1; }
  nixos_setup
  case "${1:-both}" in
    x86_64) nixos_build x86_64-unknown-linux-musl ;;
    aarch64) nixos_build aarch64-unknown-linux-musl ;;
    *) nixos_build x86_64-unknown-linux-musl; nixos_build aarch64-unknown-linux-musl ;;
  esac
fi
