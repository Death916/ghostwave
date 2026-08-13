{
  description = "ghostwave — terminal emulator + ghostd remote session daemon";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      # Official prebuilt musl std components (version-matched to the
      # nixpkgs rustc below). Contains the musl libc.a + crt objects, so
      # linking is fully self-contained — no C toolchain, no zig.
      rustVersion = "1.95.0";
      rustDate = "2026-04-16";
      stdComponent = target: pkgs.fetchurl {
        url = "https://static.rust-lang.org/dist/${rustDate}/rust-std-${rustVersion}-${target}.tar.xz";
        sha256 =
          if target == "x86_64-unknown-linux-musl" then
            "aee540abf132920f791ef781489851a078d69dff493fb628d49c1d573f92bb3a"
          else
            "f6710416ed9a7d5cf2a15efa761eb79a1deeb43f9961bbe05cc97bec4ef9064a";
      };

      mkGhostd = { target, stdTar }:
        pkgs.rustPlatform.buildRustPackage {
          pname = "ghostd-${target}";
          version = "0.1.0";
          src = ./ghostd;
          cargoLock.lockFile = ./ghostd/Cargo.lock;
          cargoBuildFlags = [ "--target" target ];
          doCheck = false; # cross binaries can't exec on the host
          preBuild = ''
            # Merge the musl std component into a synthetic sysroot on top
            # of the host rustc's rustlib.
            export SYSROOT="$PWD/rust-sysroot"
            mkdir -p "$SYSROOT/lib"
            cp -rL ${pkgs.rustc}/lib/rustlib "$SYSROOT/lib/rustlib"
            tar -xf ${stdTar} -C "$PWD"
            cp -rL "$PWD/rust-std-${rustVersion}-${target}/rust-std-${rustVersion}-${target}/lib/rustlib/${target}" \
                  "$SYSROOT/lib/rustlib/"
            # Link with LLVM's lld (rustc's own rust-lld is not shipped by
            # nixpkgs rustc); link-self-contained pulls musl from the std
            # component — the result is a static musl binary, no gcc.
            export RUSTFLAGS="-C linker=${pkgs.llvmPackages.lld}/bin/lld -C link-self-contained=yes --sysroot=$SYSROOT"
          '';
          meta.mainProgram = "ghostd";
        };
    in {
      packages.x86_64-linux = {
        ghostd-x86_64 = mkGhostd {
          target = "x86_64-unknown-linux-musl";
          stdTar = stdComponent "x86_64-unknown-linux-musl";
        };
        ghostd-aarch64 = mkGhostd {
          target = "aarch64-unknown-linux-musl";
          stdTar = stdComponent "aarch64-unknown-linux-musl";
        };
      };
    };
}
