{
  description = "ghostwave — terminal emulator + ghostd remote session daemon";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";

  outputs = { self, nixpkgs }:
    let
      # Canonical nixpkgs static-musl Rust build (pkgsMusl). ghostd is pure
      # Rust (nix + libc), so the musl stdenv's C toolchain is barely used;
      # the first build bootstraps the musl stdenv once and is cached after.
      mkGhostd = pkgs:
        pkgs.rustPlatform.buildRustPackage {
          pname = "ghostd";
          version = "0.1.0";
          src = ./ghostd;
          cargoLock.lockFile = ./ghostd/Cargo.lock;
          doCheck = false; # cross binaries can't exec on the build host
          meta.mainProgram = "ghostd";
        };
    in {
      packages.x86_64-linux = {
        ghostd-x86_64 = mkGhostd nixpkgs.legacyPackages.x86_64-linux.pkgsMusl;
        # aarch64 musl cross-compiled from this machine (nixpkgs cross).
        ghostd-aarch64 = mkGhostd nixpkgs.legacyPackages.x86_64-linux.pkgsCross.aarch64-multiplatform.pkgsMusl;
      };
      aarch64-linux = {
        # Native aarch64 build — use on an aarch64 host (e.g. orac).
        ghostd-aarch64 = mkGhostd nixpkgs.legacyPackages.aarch64-linux.pkgsMusl;
      };
    };
}
