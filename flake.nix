# cSpell:ignore stdenv
{
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    self,
    nixpkgs,
    fenix,
  }: let
    systems = [ "x86_64-linux" "aarch64-darwin" "aarch64-linux" ];
    mapListToAttrs = list: func:
        builtins.listToAttrs (builtins.map (v: { name = v; value = (func v); }) list);
  in {
    devShells = mapListToAttrs systems (system:
    let
      inherit (pkgs) lib;
      pkgs = import nixpkgs {inherit system;};
      # Unfree-accepting import for the Android SDK/NDK (Google EULA).
      pkgsAndroid = import nixpkgs {
        inherit system;
        config = {
          allowUnfree = true;
          android_sdk.accept_license = true;
        };
      };
      # fenix-provisioned stable toolchain for the host. This replaces the
      # ambient/nixpkgs Rust: entering the shell is enough, no rustup.
      fenixPkgs = fenix.packages.${system};
      host = fenixPkgs.stable;
      hostToolchain = host.withComponents [
        "cargo"
        "rustc"
        "clippy"
        "rustfmt"
        "rust-src"
      ];
      # cargo-apk2 is mzdk100's crate (distinct from nixpkgs' cargo-apk, which
      # cannot compile android/java/ to DEX or declare <service>). It is not in
      # nixpkgs and the upstream repo publishes no releases/tags, so cargo
      # binstall has no prebuilt artifacts to fetch — package it from
      # crates.io. The .crate ships its Cargo.lock, so cargoLock needs no
      # separate hash to maintain.
      cargo-apk2 = pkgs.rustPlatform.buildRustPackage rec {
        pname = "cargo-apk2";
        version = "1.4.1";
        src = pkgs.fetchCrate {
          inherit pname version;
          # Hash of the *unpacked* crate (fetchurl runs with unpack = true),
          # not of the .crate file itself.
          hash = "sha256-OJ34HOTIcmJh/exp3yq6Ir1RvooMSRwf9yF02qSx6yY=";
        };
        cargoLock.lockFile = "${src}/Cargo.lock";
        doCheck = false; # its tests need an Android SDK/device
      };
    in
    {
      default = with pkgs; let
        runtime-libs = [
          fontconfig
          libxkbcommon
          libGL

          libx11
          libxcursor
          libxi
          libxrandr
          vulkan-loader
	  mpv
        ] ++
        (
          lib.optionals pkgs.stdenv.hostPlatform.isLinux
          [
            wayland
          ]
        );
      in
        mkShell {
          nativeBuildInputs = [
            pkg-config
            hostToolchain
            cargo-bloat
            cargo-about
          ] ++ (lib.optional pkgs.stdenv.hostPlatform.isLinux perf);
          hardeningDisable = ["fortify"];
          # ffmpeg-sys-next regenerates its FFI bindings with bindgen at build
          # time: it needs libclang and, inside a nix shell, the libc/kernel
          # headers that gcc finds only through its wrapper.
          env =
            (lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
              LIBCLANG_PATH = "${llvmPackages.libclang.lib}/lib";
              CPATH = "${glibc.dev}/include:${linuxHeaders}/include";
              # mold is the default linker for native GNU/Linux builds only.
              # Target-scoped so Android (NDK clang, built in the separate
              # .#android shell) is left alone.
              "CARGO_TARGET_${lib.toUpper (lib.replaceStrings ["-"] ["_"]
                pkgs.stdenv.hostPlatform.rust.rustcTarget)}_RUSTFLAGS" =
                "-C link-arg=-fuse-ld=mold";
            })
            // (lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin {
              LIBCLANG_PATH = "${llvmPackages.libclang.lib}/lib";
            });
          buildInputs = [
            # Not strictly required, but helps with
            # https://github.com/NixOS/nixpkgs/issues/370494
            rust-jemalloc-sys
            libxkbcommon
            openssl
            libGL
            freetype
            fontconfig
            mpv
            fontconfig
            runtime-libs
            lld
          ] ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux [
            libgbm
            libinput
            # Merge the qt packages together to make a lighter version of qt6.full
            (symlinkJoin {
              name = "qt packages";
              paths = [
                qt6.qtbase
                # Required for 'QT_QPA_PLATFORM=wayland' to work
                qt6.qtwayland
              ];
            })
            seatd
            udev
            mold

            alsa-lib
          ];
          LD_LIBRARY_PATH = lib.makeLibraryPath runtime-libs;
        };
      spelling = with pkgs;
        mkShell {
          buildInputs = [
            (aspellWithDicts (d: [d.en]))
          ];
        };
    }
    // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
      # Android build environment: nix-managed Rust (fenix, stable + the two
      # vendored Android target stds) + SDK + NDK + JDK + cargo-apk. No
      # rustup, no manual `rustup target add` — entering the shell is enough.
      # Enter with: nix develop .#android
      android = let
        androidsdk = (pkgsAndroid.androidenv.composeAndroidPackages {
          platformVersions = [ "34" "35" ];
          buildToolsVersions = [ "34.0.0" "35.0.0" ];
          abiVersions = [ "arm64-v8a" "x86_64" ];
          includeNDK = true;
          ndkVersions = [ "26.1.10909125" ];
          # Emulator binary included; no system images (they add GBs).
          # Add `includeSystemImages = true; systemImageTypes =
          # [ "google_apis_playstore" ];` for a fully hermetic emulator,
          # or use a physical device / Android Studio images instead.
          includeEmulator = true;
        }).androidsdk;
        # Host stable plus a std for every target cargo-apk packages for us
        # (Cargo.toml `build_targets`, matching the vendored libmpv.so ABIs
        # in vendor/android-libs/). armv7/i686 are omitted until their libmpv.so is
        # vendored — linking them would fail at `build.rs`'s warning.
        toolchain = fenixPkgs.combine (
          [
            host.cargo
            host.rustc
            host.clippy
            host.rustfmt
            host.rust-src
          ]
          ++ map (target: fenixPkgs.targets.${target}.stable.rust-std) [
            "aarch64-linux-android"
            "x86_64-linux-android"
            #"armv7-linux-androideabi"
            #"i686-linux-android"
          ]
        );
      in
        pkgsAndroid.mkShell {
          buildInputs = [
            androidsdk
            pkgs.android-tools
            pkgsAndroid.jdk17
            # Packaged by this flake (see the cargo-apk2 derivation above):
            # nixpkgs has no cargo-apk2, and cargo binstall has no prebuilt
            # artifacts to fetch.
            cargo-apk2
            pkgsAndroid.cargo-about
            toolchain
          ];
          # cargo-apk2 does not use Gradle, so no aapt2 override is needed.
          # (If you add a Gradle wrapper later, point it at the nix
          # build-tools aapt2:
          # GRADLE_OPTS="-Dorg.gradle.project.android.aapt2FromMavenOverride=${androidsdk}/libexec/android-sdk/build-tools/34.0.0/aapt2")
          shellHook = ''
            export ANDROID_HOME="${androidsdk}/libexec/android-sdk"
            # First (only) pinned NDK, robust to version drift.
            export ANDROID_NDK_ROOT="$(echo "$ANDROID_HOME"/ndk/* | cut -d' ' -f1)"
            export JAVA_HOME="${pkgsAndroid.jdk17}"
            export PATH="$ANDROID_HOME/platform-tools:$ANDROID_HOME/cmdline-tools/latest/bin:$PATH"
            # NDK clang as the cargo linker for Android targets. The NDK
            # lives in the nix store, so this cannot be checked into
            # .cargo/config.toml — it is exported here instead.
            ndk_bin="$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin"
            export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$ndk_bin/aarch64-linux-android34-clang"
            export CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER="$ndk_bin/armv7a-linux-androideabi34-clang"
            export CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$ndk_bin/x86_64-linux-android34-clang"
            export CARGO_TARGET_I686_LINUX_ANDROID_LINKER="$ndk_bin/i686-linux-android34-clang"
            # cc-rs (aws-lc-sys and friends) needs the NDK C compilers as well.
            # The stdenv exports a bare host `CC`/`CXX`/`AR`, and cc-rs applies
            # that to the cross target too before it ever falls back to looking
            # for `aarch64-linux-android-clang` on PATH (which is not there) —
            # so the target-specific variables have to be set explicitly.
            # Underscored names only: shells reject dashes in variable names.
            export CC_aarch64_linux_android="$ndk_bin/aarch64-linux-android34-clang"
            export CXX_aarch64_linux_android="$ndk_bin/aarch64-linux-android34-clang++"
            export AR_aarch64_linux_android="$ndk_bin/llvm-ar"
            export CC_armv7_linux_androideabi="$ndk_bin/armv7a-linux-androideabi34-clang"
            export CXX_armv7_linux_androideabi="$ndk_bin/armv7a-linux-androideabi34-clang++"
            export AR_armv7_linux_androideabi="$ndk_bin/llvm-ar"
            export CC_x86_64_linux_android="$ndk_bin/x86_64-linux-android34-clang"
            export CXX_x86_64_linux_android="$ndk_bin/x86_64-linux-android34-clang++"
            export AR_x86_64_linux_android="$ndk_bin/llvm-ar"
            export CC_i686_linux_android="$ndk_bin/i686-linux-android34-clang"
            export CXX_i686_linux_android="$ndk_bin/i686-linux-android34-clang++"
            export AR_i686_linux_android="$ndk_bin/llvm-ar"
            # The stdenv exports host include dirs through CPATH; the NDK clang
            # reads it and then picks up the host glibc headers, failing with a
            # missing `gnu/stubs-32.h` while compiling target C (aws-lc-sys,
            # etc.). Android cross builds must not see host headers.
            unset CPATH
            echo "android shell ready: $(cargo --version), $(rustc --version)"
            echo "  ANDROID_HOME=$ANDROID_HOME"
            echo "  NDK: $ANDROID_NDK_ROOT"
            echo "  target stds: $(ls "$(rustc --print sysroot)/lib/rustlib" | grep android | tr '\n' ' ')"
            echo "  build: cargo apk2 build --target aarch64-linux-android --no-default-features --features android --lib"
            echo "  run:   cargo apk2 run   --target aarch64-linux-android --no-default-features --features android --lib"
          '';
        };
      # Windows x86_64 cross-build environment. MPV's Windows development
      # archive is pinned by SHA-256 in vendor/windows-libs/SOURCES; FFmpeg is already
      # included in that libmpv build, so no second FFmpeg bundle is needed.
      # Enter with: nix develop .#windows
      windows = let
        target = "x86_64-pc-windows-gnu";
        mingw = pkgs.pkgsCross.mingwW64;
        toolchain = fenixPkgs.combine (
          [
            host.cargo
            host.rustc
            host.clippy
            host.rustfmt
            host.rust-src
          ]
          ++ [fenixPkgs.targets.${target}.stable.rust-std]
        );
        mpvDevArchive = pkgs.fetchurl {
          url = "https://github.com/shinchiro/mpv-winbuild-cmake/releases/download/20260928/mpv-dev-x86_64-20260928-git-e470f8986e.7z";
          hash = "sha256-gXlddZ4BAW8VUP1xZRoaXVmrXCjvMcC2eTIk6c/zlFk=";
        };
        windowsMpvDev = pkgs.stdenvNoCC.mkDerivation {
          pname = "nova-mpv-win64-dev";
          version = "20260928";
          src = mpvDevArchive;
          dontUnpack = true;
          nativeBuildInputs = [pkgs.p7zip];
          installPhase = ''
            work="$TMPDIR/mpv-unpack"
            mkdir -p "$work" "$out/64"
            7z x -y "$src" -o"$work" >/dev/null
            import_library="$(find "$work" -type f -name libmpv.dll.a -print -quit)"
            if [ -z "$import_library" ]; then
              echo "libmpv.dll.a missing from pinned MPV development archive" >&2
              exit 1
            fi
            cp -R "$(dirname "$import_library")/." "$out/64/"
            test -f "$out/64/libmpv.dll.a"
            test -f "$out/64/libmpv-2.dll"
          '';
        };
        crossGcc = mingw.stdenv.cc;
      in
        pkgs.mkShell {
          nativeBuildInputs = [
            toolchain
            pkgs.pkg-config
            pkgs.cargo-about
            crossGcc
          ];
          buildInputs = [
            mingw.windows.pthreads
          ];
          MPV_SOURCE = "${windowsMpvDev}";
          # The release packager resolves the cross compiler's Nix closure and
          # stages its MinGW runtime DLLs alongside Nova and libmpv.
          MINGW_CC = "${crossGcc}";
          "CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER" =
            "${crossGcc}/bin/${crossGcc.targetPrefix}gcc";
          "CC_x86_64_pc_windows_gnu" =
            "${crossGcc}/bin/${crossGcc.targetPrefix}gcc";
          "CXX_x86_64_pc_windows_gnu" =
            "${crossGcc}/bin/${crossGcc.targetPrefix}g++";
          "AR_x86_64_pc_windows_gnu" =
            "${crossGcc}/bin/${crossGcc.targetPrefix}ar";
          # aws-lc-sys's optional jitterentropy code needs sched.h, unavailable
          # in MinGW. Disable that supplemental entropy source for this target.
          AWS_LC_SYS_NO_JITTER_ENTROPY = "1";
        };
    });
  };
}
