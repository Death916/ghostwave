# ghostwave dev shell — plain nix-shell, no devenv required.
#
#   nix-shell           # enter shell
#   cargo build         # first build compiles libghostty-vt via zig (~minutes)
#   cargo run           # launch ghostwave
#
# Notes:
# - zig_0_15 (0.15.2) is REQUIRED by libghostty-vt-sys's vendored build
#   (ghostty 1.3.2-dev pins it; zig 0.16 breaks the build).
# - rustc >= 1.90 required (libghostty-vt uses edition 2024).
# - miniquad (macroquad) dlopens GL/wayland/x11 libs at RUNTIME, so they
#   must be on LD_LIBRARY_PATH. mkShell does not derive that from
#   buildInputs here, so we set it explicitly (nix.dev: any string-coercible
#   attr becomes an env var; makeLibraryPath is the canonical helper).
# - jetbrains-mono is a fallback font; the app embeds its own copy of
#   JetBrainsMono-Medium.ttf in assets/, so fonts are optional.
# - git + curl needed by the vendored build (build.rs clones ghostty once).

{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  packages = with pkgs; [
    cargo
    rustc
    rustfmt
    clippy
    zig_0_15
    pkg-config
    fontconfig
    jetbrains-mono
    git
    curl
    jq
  ];

  # Runtime dlopen libs for miniquad: libGL.so.1/libEGL.so.1 (GL),
  # libwayland-*.so (Wayland backend), libxkbcommon.so.0, libX11.so.6 and
  # friends (X11 fallback backend).
  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (with pkgs; [
    libGL
    wayland
    libxkbcommon
    libx11
    libxcursor
    libxrandr
    libxi
    libxext
    libxcb
  ]);

  shellHook = ''
    echo "ghostwave dev shell active (rust $(rustc --version) / zig $(zig version))"
  '';
}
