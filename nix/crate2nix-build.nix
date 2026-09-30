{ system ? "x86_64-linux" }:

let
  # Reuse the repository's locked nixpkgs and Fenix inputs so this benchmark
  # uses the same stable Rust compiler as the existing Cargo CI check.
  flake = builtins.getFlake (toString ../.);
  pkgs = import flake.inputs.nixpkgs { inherit system; };
  toolchain = flake.inputs.fenix.packages.${system}.stable;

  buildRustCrateForPkgs = rustPkgs: rustPkgs.buildRustCrate.override {
    rustc = toolchain.rustc;
    cargo = toolchain.cargo;
    defaultCrateOverrides = rustPkgs.defaultCrateOverrides // {
      nova = attrs: {
        nativeBuildInputs = (attrs.nativeBuildInputs or []) ++ [
          toolchain.cargo
          rustPkgs.cargo-about
          rustPkgs.pkg-config
          rustPkgs.mold
        ];
        extraRustcOpts = (attrs.extraRustcOpts or []) ++ [ "-Clink-arg=-fuse-ld=mold" ];
        buildInputs = (attrs.buildInputs or []) ++ [
          rustPkgs.fontconfig
          rustPkgs.libxkbcommon
          rustPkgs.libGL
          rustPkgs.libx11
          rustPkgs.libxcursor
          rustPkgs.libxi
          rustPkgs.libxrandr
          rustPkgs.vulkan-loader
          rustPkgs.mpv
          rustPkgs.wayland
          rustPkgs.libgbm
          rustPkgs.libinput
          rustPkgs.alsa-lib
          rustPkgs.openssl
        ];
      };

      libmpv2-sys = attrs: {
        nativeBuildInputs = (attrs.nativeBuildInputs or []) ++ [ rustPkgs.pkg-config ];
        buildInputs = (attrs.buildInputs or []) ++ [ rustPkgs.mpv ];
      };

      tikv-jemalloc-sys = attrs: {
        # Seed jemalloc's configure cache for the GNU strerror_r signature
        # provided by the pinned x86_64 Linux/glibc build environment.
        je_cv_strerror_r_returns_char_with_gnu_source = "yes";
      };

      av-scenechange = attrs: {
        # This build script forwards Cargo's encoded rustflags to dependents;
        # crate2nix does not set it when compiling the crate derivation.
        CARGO_ENCODED_RUSTFLAGS = "";
      };

      yeslogic-fontconfig-sys = attrs: {
        nativeBuildInputs = (attrs.nativeBuildInputs or []) ++ [ rustPkgs.pkg-config ];
        buildInputs = (attrs.buildInputs or []) ++ [ rustPkgs.fontconfig ];
      };

      ffmpeg-sys-next = attrs: {
        LIBCLANG_PATH = "${rustPkgs.llvmPackages.libclang.lib}/lib";
        CPATH = "${rustPkgs.glibc.dev}/include:${rustPkgs.linuxHeaders}/include";
        nativeBuildInputs = (attrs.nativeBuildInputs or []) ++ [
          rustPkgs.llvmPackages.libclang
        ];
      };
    };
  };

  cargoNix = import ./Cargo.nix {
    inherit pkgs buildRustCrateForPkgs;
    # Match the project's default Cargo dev profile, not a release build.
    release = false;
  };
in {
  # Keep every workspace member's derivation output reachable so the cache
  # contains the per-crate artifacts used by the workspace build.
  workspace = cargoNix.allWorkspaceMembers;
}
