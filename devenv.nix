# ghostwave dev environment via devenv (optional — shell.nix also works).
#
#   devenv shell        # enter (requires devenv installed)
#
# Same toolchain as shell.nix. `languages.rust` provides cargo/rustc/rustfmt
# on PATH; zig_0_15 is the only non-Rust piece the build needs.
# devenv's `libs` = packages whose lib dirs go on LD_LIBRARY_PATH (the
# runtime dlopen deps for miniquad).

{ pkgs, ... }: {
  name = "ghostwave";

  packages = with pkgs; [
    zig_0_15
    pkg-config
    fontconfig
    jetbrains-mono
    git
    curl
    jq
  ];

  libs = with pkgs; [
    libGL
    wayland
    libxkbcommon
    libx11
    libxcursor
    libxrandr
    libxi
    libxext
    libxcb
  ];

  languages.rust.enable = true;

  enterShell = ''
    echo "ghostwave devenv active"
  '';
}
