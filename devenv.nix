{
  pkgs,
  lib,
  config,
  inputs,
  ...
}:

{
  name = "ghostwave";

  # Rust toolchain managed declaratively via rust-overlay (cached)
  languages.rust = {
    enable = true;
    channel = "stable";
    components = [
      "rustc"
      "cargo"
      "clippy"
      "rustfmt"
      "rust-analyzer"
    ];
  };

  # Packages: Reuses Halloy's cached GUI/graphics libs + Ghostwave specific tools
  packages = with pkgs; [
    zig_0_15
    pkg-config
    fontconfig
    jetbrains-mono
    git
    curl
    jq
    # Graphics & GUI libs (shared with Halloy cache)
    mesa
    libGL
    wayland
    libxkbcommon
    libxcb
    # X11 fallback libs for miniquad
    libx11
    libxcursor
    libxrandr
    libxi
    libxext
  ];

  # Dynamic linker path for runtime dlopen (miniquad)
  env.LD_LIBRARY_PATH = lib.makeLibraryPath (
    with pkgs;
    [
      libGL
      wayland
      libxkbcommon
      libxcb
      libx11
      libxcursor
      libxrandr
      libxi
      libxext
    ]
  );

  enterShell = ''
    echo "ghostwave dev shell active (rust $(rustc --version) / zig $(zig version))"
  '';
}
