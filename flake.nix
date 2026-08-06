{
  # Nix flake: hermetic dev shell + reproducible Linux build.
  #
  # WHY a flake in a repo that mainly ships via GitHub Actions: it is the
  # strongest reproducibility claim we can make. `nix develop` gives every
  # engineer the same compiler, linker, and packaging tools down to the store
  # hash, and `nix build` produces the Linux binary in a sandbox with no
  # network and no host toolchain, which is the environment in which
  # "reproducible" stops being a slogan. It also gets the project into
  # nixpkgs/NixOS distribution basically for free later.
  #
  # Toolchain source: rust-overlay reads rust-toolchain.toml, so the flake
  # and rustup agree on the compiler BY CONSTRUCTION rather than by two pins
  # that drift apart.
  description = "outloud-spike: local edit-by-voice M0 spike";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };
        # The single source of truth for the compiler version is
        # rust-toolchain.toml; see the WHY at the top of that file.
        toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      in
      {
        devShells.default = pkgs.mkShell {
          packages = [
            toolchain
            pkgs.cargo-deny
            pkgs.cargo-audit
          ] ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
            # Linux packaging tools used by scripts/build-linux.sh.
            pkgs.dpkg
            pkgs.rpm
            pkgs.binutils # objdump for scripts/ci-verify-baseline.sh
          ];
        };

        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "outloud-spike";
          version = "0.1.0";
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          # The sandboxed nix build is itself the reproducibility check:
          # no network, no impure env, pinned inputs. macOS-specific FFI in
          # ax-edit compiles to the Unsupported stub on Linux, so this
          # builds everywhere the flake evaluates.
          doCheck = true;
          meta = {
            description = "M0 spike harness for local edit-by-voice";
            license = pkgs.lib.licenses.mit;
            mainProgram = "spike-cli";
          };
        };
      });
}
