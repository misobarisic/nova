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
      # Pinned native release inputs; Nix verifies and caches each archive.
      nativeMedia = import ./nix/native-media { inherit system; pkgs = pkgsAndroid; };
      androidMedia = pkgs.runCommand "nova-android-media" { } ''
        mkdir -p "$out"
        ln -s ${nativeMedia.bundle-android-arm64}/lib "$out/arm64-v8a"
        ln -s ${nativeMedia.bundle-android-armv7}/lib "$out/armeabi-v7a"
        ln -s ${nativeMedia.bundle-android-x86_64}/lib "$out/x86_64"
      '';
      windowsMedia = pkgs.runCommand "nova-windows-media" { } ''
        mkdir -p "$out"
        ln -s ${nativeMedia.bundle-mingw-x86_64}/lib "$out/64"
        ln -s ${nativeMedia.bundle-mingw-x86_64}/share/nova-native "$out/metadata"
        ln -s ${nativeMedia.bundle-mingw-x86_64}/share/licenses "$out/licenses"
        cp ${nativeMedia.bundle-mingw-x86_64}/share/nova-native/sources.json "$out/SOURCES"
      '';
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
      # cargo-apk2 compiles android/java/ to DEX and declares <service>, unlike
      # nixpkgs' cargo-apk. Pin upstream's merged Android TV banner support.
      # The repository omits Cargo.lock, so keep the CLI-only dependency lock
      # here and exclude example apps from dependency resolution and builds.
      cargo-apk2 = pkgs.rustPlatform.buildRustPackage rec {
        pname = "cargo-apk2";
        version = "1.4.2";
        src = pkgs.fetchFromGitHub {
          owner = "mzdk100";
          repo = "cargo-apk2";
          rev = "32b91331ff2371f3f06505ea6ceea3743e423a1c";
          hash = "sha256-Sp6oc3Ee1cjpABCpV3Pj2Hmj26A+4WYDeElUCMbuBjc=";
        };
        patches = [
          ./android/cargo-apk2-readonly-libs.patch
          ./android/cargo-apk2-uncompressed-libs.patch
          ./android/cargo-apk2-version-code.patch
          ./android/cargo-apk2-pip.patch
        ];
        cargoLock.lockFile = ./android/cargo-apk2-Cargo.lock;
        postPatch = ''
          sed -i '/^    ".*-examples",$/d' Cargo.toml
          cp ${./android/cargo-apk2-Cargo.lock} Cargo.lock
          # cargo-apk2 normally reads runtime_libs only from Cargo.toml. The
          # immutable Nix input must supply the same directory to link and APK.
          substituteInPlace cargo-apk2/src/manifest.rs \
            --replace-fail 'runtime_libs: metadata.runtime_libs,' \
              'runtime_libs: std::env::var_os("NOVA_ANDROID_LIBS").map(std::path::PathBuf::from).or(metadata.runtime_libs),'
        '';
        cargoBuildFlags = [ "-p" "cargo-apk2" ];
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
          ] ++ (lib.optionals pkgs.stdenv.hostPlatform.isLinux [
            perf
            cargo-sweep
          ]);
          hardeningDisable = ["fortify"];
          # ffmpeg-sys-next regenerates its FFI bindings with bindgen at build
          # time: it needs libclang and, inside a nix shell, the libc/kernel
          # headers that gcc finds only through its wrapper.
          env =
            (lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
              LIBCLANG_PATH = "${llvmPackages.libclang.lib}/lib";
              CPATH = "${glibc.dev}/include:${linuxHeaders}/include";
              # Headless GL tests opt in to the matching Mesa EGL driver;
              # ordinary desktop runs retain the system's driver selection.
              NOVA_TEST_EGL_VENDOR_LIBRARY_FILENAMES = "${mesa}/share/glvnd/egl_vendor.d/50_mesa.json";
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
      # Android build environment: nix-managed Rust (fenix, stable + the three
      # Android target stds) + SDK + NDK + JDK + cargo-apk. No
      # rustup, no manual `rustup target add` — entering the shell is enough.
      # Enter with: nix develop .#android
      android = let
        androidsdk = (pkgsAndroid.androidenv.composeAndroidPackages {
          platformVersions = [ "34" "35" ];
          buildToolsVersions = [ "34.0.0" "35.0.0" ];
          abiVersions = [ "arm64-v8a" "armeabi-v7a" "x86_64" ];
          includeNDK = true;
          ndkVersions = [ "26.1.10909125" ];
          # Emulator binary included; no system images (they add GBs).
          # Add `includeSystemImages = true; systemImageTypes =
          # [ "google_apis_playstore" ];` for a fully hermetic emulator,
          # or use a physical device / Android Studio images instead.
          includeEmulator = true;
        }).androidsdk;
        # Host stable plus a std for every target cargo-apk packages for us
        # (Cargo.toml `build_targets`, matching the packaged ABIs). i686 is
        # omitted because the native recipe does not build that ABI.
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
            "armv7-linux-androideabi"
            "x86_64-linux-android"
            #"i686-linux-android"
          ]
        );
      in
        pkgsAndroid.mkShell {
          buildInputs = [
            androidsdk
            androidMedia
            pkgs.android-tools
            pkgsAndroid.jdk17
            # Packaged by this flake (see the cargo-apk2 derivation above):
            # nixpkgs has no cargo-apk2, and cargo binstall has no prebuilt
            # artifacts to fetch.
            cargo-apk2
            pkgsAndroid.cargo-about
            pkgs.llvmPackages.libclang
            # ARMv7 Skia has no upstream binary cache: its source build
            # synchronizes dependencies and invokes Python/GN/Ninja.
            pkgs.python3
            pkgs.gn
            pkgs.ninja
            pkgs.git
            pkgs.curl
            toolchain
          ];
          NOVA_ANDROID_LIBS = "${androidMedia}";
          NOVA_NATIVE_CA_BUNDLE = "${nativeMedia.bundle-android-arm64}/share/nova-native/cacert.pem";
          NOVA_NATIVE_NOTICES = "${nativeMedia.notices-android-arm64}";
          # cargo-apk2 does not use Gradle, so no aapt2 override is needed.
          # (If you add a Gradle wrapper later, point it at the nix
          # build-tools aapt2:
          # GRADLE_OPTS="-Dorg.gradle.project.android.aapt2FromMavenOverride=${androidsdk}/libexec/android-sdk/build-tools/34.0.0/aapt2")
          shellHook = ''
            export ANDROID_HOME="${androidsdk}/libexec/android-sdk"
            # First (only) pinned NDK, robust to version drift.
            export ANDROID_NDK_ROOT="$(echo "$ANDROID_HOME"/ndk/* | cut -d' ' -f1)"
            # rust-skia's source build reads ANDROID_NDK directly.
            export ANDROID_NDK="$ANDROID_NDK_ROOT"
            # Use Nix-native build tools rather than downloaded host binaries.
            export SKIA_GN_COMMAND="${pkgs.gn}/bin/gn"
            export SKIA_NINJA_COMMAND="${pkgs.ninja}/bin/ninja"
            export JAVA_HOME="${pkgsAndroid.jdk17}"
            export LIBCLANG_PATH="${pkgs.llvmPackages.libclang.lib}/lib"
            export PATH="$ANDROID_HOME/platform-tools:$ANDROID_HOME/cmdline-tools/latest/bin:$PATH"
            # NDK clang as the cargo linker for Android targets. The NDK
            # lives in the nix store, so this cannot be checked into
            # .cargo/config.toml — it is exported here instead.
            ndk_bin="$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin"
            # Bindgen loads host libclang directly, bypassing the NDK compiler
            # wrappers. Point each Android target at the NDK sysroot so QuickJS
            # bindings use Android headers instead of the host glibc headers.
            # API 26 matches Cargo.toml's min_sdk_version and cargo-apk2.
            ndk_sysroot="$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/sysroot"
            export BINDGEN_EXTRA_CLANG_ARGS_aarch64_linux_android="--sysroot=$ndk_sysroot --target=aarch64-linux-android26"
            export BINDGEN_EXTRA_CLANG_ARGS_armv7_linux_androideabi="--sysroot=$ndk_sysroot --target=armv7a-linux-androideabi26"
            export BINDGEN_EXTRA_CLANG_ARGS_x86_64_linux_android="--sysroot=$ndk_sysroot --target=x86_64-linux-android26"
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
      # Windows x86_64 cross-build environment. Native media libraries come
      # from the pinned nova-vendor release in nix/native-media.
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
            windowsMedia
          ];
          MPV_SOURCE = "${windowsMedia}";
          NOVA_NATIVE_NOTICES = "${nativeMedia.notices-mingw-x86_64}";
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
