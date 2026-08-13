{
  description = "ghostwave — terminal emulator + ghostd remote session daemon";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";

  outputs = { self, nixpkgs }:
    let
      # Static musl build of the ghostd daemon for a given package set.
      # Pure Rust (nix + libc only) — no C toolchain or zig required.
      mkGhostd = pkgs:
        pkgs.rustPlatform.buildRustPackage {
          pname = "ghostd";
          version = "0.1.0";
          src = ./ghostd;
          cargoLock.lockFile = ./ghostd/Cargo.lock;
          meta.mainProgram = "ghostd";
        };
      musl = pkgs: pkgs.pkgsMusl;
    in {
      packages = {
        x86_64-linux = {
          ghostd-x86_64 = mkGhostd (musl nixpkgs.legacyPackages.x86_64-linux);
          # aarch64 musl built via nixpkgs cross from this machine.
          ghostd-aarch64 = mkGhostd (musl nixpkgs.legacyPackages.x86_64-linux.pkgsCross.aarch64-multiplatform);
        };
        aarch64-linux = {
          ghostd-aarch64 = mkGhostd (musl nixpkgs.legacyPackages.aarch64-linux);
        };
      };
    };
}
