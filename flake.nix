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
    # nixos-unstable, not a stable release: the ALSA client library must be
    # new enough for the PipeWire ALSA plugin on the host. Pinned to 25.05,
    # microphone capture failed on current NixOS with
    # "snd_pcm_open failed ... No such device or address" because
    # libasound_module_pcm_pipewire.so refused to load into the older
    # libasound.
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
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

        outloudSpike = pkgs.rustPlatform.buildRustPackage {
          pname = "outloud-spike";
          version = "0.1.0";
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          # Linux: cpal pulls alsa-sys, which needs pkg-config + alsa headers.
          # makeWrapper: see postFixup below.
          nativeBuildInputs = pkgs.lib.optionals pkgs.stdenv.isLinux [
            pkgs.pkg-config
            pkgs.makeWrapper
          ];
          buildInputs = pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.alsa-lib ];

          # Text delivery on Wayland SHELLS OUT: the synthetic-keys tier runs
          # `wtype` (virtual-keyboard protocol) and the paste fallback runs
          # `wl-copy`/`wl-paste`. Those are looked up on PATH at runtime, so
          # without this the package "works" only on machines that happen to
          # have them installed globally, and degrades to a confusing
          # no-delivery state on any other -- the exact class of bug a Nix
          # package exists to prevent. Wrap the binaries so the tools are
          # always found, while still PREPENDING rather than replacing PATH
          # so a user's own newer wtype wins if they want it.
          postFixup = pkgs.lib.optionalString pkgs.stdenv.isLinux ''
            for _bin in $out/bin/*; do
              wrapProgram "$_bin" \
                --prefix PATH : ${
                  pkgs.lib.makeBinPath [
                    pkgs.wtype
                    pkgs.wl-clipboard
                  ]
                }
            done
          '';

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

        # CUDA-accelerated `outloud` for Linux, built with whisper-rs/cuda.
        # `null` everywhere but x86_64-linux, folded out of `packages` below
        # by `lib.filterAttrs`, so this is the ONLY place a system check
        # needs to live: every other output (`devShells`, `packages.default`)
        # stays a plain per-system value with no CUDA-shaped conditionals
        # anywhere near it.
        #
        # WHY a separate package rather than teaching `packages.default` a
        # `cudaSupport` flag: `packages.default` must stay buildable by
        # `nix build` with the sandbox's normal (network-disabled, no GPU,
        # free-software-only) settings, because that sandboxed build IS the
        # reproducibility check the top-of-file comment promises, and it
        # runs on every system the flake evaluates, including macOS CI. CUDA
        # is NVIDIA's unfree redistributable, x86_64-linux only, and multiple
        # gigabytes once unpacked; making it reachable from the default
        # output would mean either eval-time `allowUnfree`/`cudaSupport`
        # config bleeding into every other package built from this flake
        # (nixpkgs config is a single global knob per `pkgs` instantiation),
        # or a conditional so tangled it stops being auditable. A second
        # `pkgs` import scoped to this one output keeps `packages.default`
        # and `devShells.default` exactly as they were: CPU-only,
        # unfree-free, and buildable everywhere including macOS.
        #
        # Matches whisper.cpp's own upstream CUDA recipe
        # (nixpkgs pkgs/by-name/wh/whisper-cpp/package.nix), which is the
        # only build in nixpkgs solving this exact problem (whisper.cpp +
        # CUDA via cmake) and is exercised by nixpkgs CI: `backendStdenv`
        # (CUDA imposes an upper bound on the host gcc version whisper-rs's
        # own `cmake` crate does not know to enforce), `cuda_nvcc` +
        # `autoAddDriverRunpath` as native inputs, `cccl` (for `<nv/target>`,
        # a header whisper.cpp's CUDA path includes) + `cuda_cudart` +
        # `libcublas` as link inputs, matching exactly what
        # `whisper-rs-sys`'s build.rs links against (cublas, cudart,
        # cublasLt, cuda, culibos -- see crates/asr/Cargo.toml's whisper-cuda
        # feature and its sibling whisper-rs-sys build.rs for the full list).
        outloudCuda =
          if system != "x86_64-linux" then null else
          let
            cudaPkgs = import nixpkgs {
              system = "x86_64-linux";
              overlays = [ rust-overlay.overlays.default ];
              config = {
                allowUnfree = true;
                cudaSupport = true;
                # RTX 5090 is Blackwell, compute capability 12.0 (sm_120).
                # nixos-unstable's default cudaCapabilities list already
                # includes it (checked directly:
                # `cudaPackages.flags.cudaCapabilities` on this pin returns
                # [ "7.5" "8.0" "8.6" "8.9" "9.0" "10.0" "10.3" "12.0" "12.1" ]),
                # so no override is needed here. Pinning explicitly anyway
                # would trade "works for whatever GPUs this nixpkgs pin
                # already targets" for "silently stops matching new
                # hardware next pin bump" -- the wrong trade for a flake
                # nobody is watching daily.
              };
            };
            cudaToolchain =
              cudaPkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
            # `makeRustPlatform` rather than the plain `cudaPkgs.rustPlatform`:
            # this is what actually rebinds the derivation builder to
            # `backendStdenv.mkDerivation`. Setting a `stdenv = ...` attribute
            # inside `buildRustPackage`'s argument set would NOT do that --
            # `rustPlatform.buildRustPackage` is already a function closed
            # over a fixed `stdenv.mkDerivation`, baked in when the platform
            # was constructed, so a same-named attribute in the call site
            # would just be inert. `makeRustPlatform` is nixpkgs' own
            # supported seam for changing which stdenv a Rust build uses.
            cudaRustPlatform = cudaPkgs.makeRustPlatform {
              cargo = cudaToolchain;
              rustc = cudaToolchain;
              stdenv = cudaPkgs.cudaPackages.backendStdenv;
            };
          in
          cudaRustPlatform.buildRustPackage {
            pname = "outloud-cuda";
            version = "0.1.0";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = [
              cudaPkgs.pkg-config
              # `cmake`: what `whisper-rs-sys`'s build.rs shells out to.
              # Absent from the default package's inputs because that build
              # never turns on the `whisper` feature at all.
              cudaPkgs.cmake
              cudaPkgs.cudaPackages.cuda_nvcc
              cudaPkgs.autoAddDriverRunpath
              # `whisper-rs-sys` also runs `bindgen` to generate the FFI
              # layer over whisper.cpp's C API, which needs libclang and a
              # `LIBCLANG_PATH` pointed at it -- the exact failure mode
              # `docs/asr-integration.md` documents for Windows ("Unable to
              # find libclang") and that is just as real on Linux, only
              # undocumented here because nothing in this flake had turned
              # on the `whisper` feature before now. `bindgenHook` is
              # nixpkgs' standard fix: a setup hook that exports
              # `LIBCLANG_PATH` for the duration of the build. Pulled from
              # the ordinary (non-CUDA) `rustPlatform`, not
              # `cudaRustPlatform`: the hook only shells out to `clang`, has
              # nothing to do with the compiler bound to `mkDerivation`, and
              # constructing it from `cudaRustPlatform` before it exists
              # here would be a definition-order cycle.
              cudaPkgs.rustPlatform.bindgenHook
              # makeWrapper: see postFixup below.
              cudaPkgs.makeWrapper
            ];
            buildInputs = [
              cudaPkgs.alsa-lib
              cudaPkgs.cudaPackages.cccl
              cudaPkgs.cudaPackages.cuda_cudart
              cudaPkgs.cudaPackages.libcublas
            ];

            # `-lcuda` is the DRIVER library, not part of the toolkit: it
            # ships with the installed NVIDIA driver and is deliberately
            # absent from anything Nix can vendor. The build still has to
            # LINK against it, which is what the `stubs` output exists for:
            # an ABI-compatible libcuda.so that resolves symbols at link
            # time and is never loaded at runtime. Without this the whole
            # CUDA build compiles -- cmake, nvcc, every .cu kernel -- and
            # then dies on the final link with
            #   rust-lld: error: unable to find library -lcuda
            # which reads like a missing dependency rather than the
            # deliberate toolkit/driver split that it is.
            #
            # `autoAddDriverRunpath` (in nativeBuildInputs) then rewrites the
            # finished binary's RUNPATH to /run/opengl-driver/lib, so at
            # RUNTIME it picks up the real libcuda from the host driver
            # rather than this stub.
            # This nixpkgs keeps the stubs INSIDE cuda_cudart's default
            # output (pkgs/development/cuda-modules/packages/cuda_cudart.nix:
            # "We have stubs but we don't have an explicit stubs output"), so
            # the path is $out/lib/stubs and there is no `.stubs` attribute
            # to reference -- asking for one fails evaluation with
            # "attribute 'stubs' missing".
            NIX_LDFLAGS = "-L${cudaPkgs.lib.getLib cudaPkgs.cudaPackages.cuda_cudart}/lib/stubs";
            # `buildFeatures`/`checkFeatures`, not `cargoBuildFeatures`: the
            # latter is `buildRustPackage`'s INTERNAL derived env-var name
            # (see nixpkgs pkgs/build-support/rust/build-rust-package), and
            # passing it directly is silently accepted as an arbitrary extra
            # derivation attribute rather than an error -- it shows up in
            # `nix derivation show` looking plausible and does precisely
            # nothing. Caught only by inspecting the actual derivation env
            # (`cargoBuildFeatures` came back empty) rather than by the
            # flake evaluating without error, which it did either way.
            buildFeatures = [ "outloud/whisper-cuda" ];
            # The sandbox has no GPU (CUDA's own docs are explicit that the
            # driver's user-mode libraries, libcuda.so included, come from
            # the host driver install and are never part of the CUDA
            # toolkit/redistributables Nix can vendor), so a real whisper.cpp
            # model load and inference pass cannot run here or in CI. The
            # sandboxed build DOES still exercise the entire compile and
            # link step -- cmake configuring GGML_CUDA, nvcc compiling the
            # .cu kernels, and the final binary linking against libcuda,
            # libcudart and libcublas -- which is everything short of
            # touching a physical device. See docs/asr-integration.md and
            # the swarm handoff notes for exactly what still needs a real
            # GPU to confirm.
            doCheck = false;

            # Same Wayland delivery tools as the default package: this is a
            # separate derivation, so it does NOT inherit that postFixup and
            # would otherwise ship a CUDA binary that transcribes perfectly
            # and then cannot type the result anywhere.
            postFixup = ''
              for _bin in $out/bin/*; do
                wrapProgram "$_bin" \\
                  --prefix PATH : ${
                    cudaPkgs.lib.makeBinPath [
                      cudaPkgs.wtype
                      cudaPkgs.wl-clipboard
                    ]
                  }
              done
            '';

            meta = {
              description = "outloud with whisper.cpp CUDA acceleration (NVIDIA, x86_64-linux)";
              license = pkgs.lib.licenses.mit;
              mainProgram = "outloud";
            };
          };
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

        # `filterAttrs` drops `outloud-cuda` entirely on every system but
        # x86_64-linux (it is `null` there, see `outloudCuda` above), rather
        # than exposing a `null`-valued attribute that `nix build .#outloud-cuda`
        # would fail on with a confusing error far from this comment.
        packages = pkgs.lib.filterAttrs (_: v: v != null) {
          default = outloudSpike;
          outloud-cuda = outloudCuda;
        };
      });
}
