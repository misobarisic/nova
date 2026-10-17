# Minimal native mpv/FFmpeg derivations for Android/Windows development shells.
{
  system ? builtins.currentSystem,
  pkgs ?
    let
      lock = builtins.fromJSON (builtins.readFile ../../flake.lock);
      source = builtins.fetchTree lock.nodes.nixpkgs.locked;
    in
    import source {
      inherit system;
      config = {
        allowUnfree = true;
        android_sdk.accept_license = true;
      };
    },
}:
let
  inherit (pkgs) lib;
  sdk =
    (pkgs.androidenv.composeAndroidPackages {
      platformVersions = [
        "34"
        "35"
      ];
      buildToolsVersions = [
        "34.0.0"
        "35.0.0"
      ];
      abiVersions = [
        "arm64-v8a"
        "armeabi-v7a"
        "x86_64"
      ];
      includeNDK = true;
      ndkVersions = [ "26.1.10909125" ];
      includeEmulator = true;
    }).androidsdk;
  ndk = "${sdk}/libexec/android-sdk/ndk/26.1.10909125";
  mingw = pkgs.pkgsCross.mingwW64.stdenv.cc;
  mcfgthreads = pkgs.pkgsCross.mingwW64.windows.mcfgthreads;
  targets = {
    android-arm64 = {
      android = true;
      abi = "arm64-v8a";
      cpu = "aarch64";
      triple = "aarch64-linux-android";
      ffarch = "aarch64";
    };
    android-armv7 = {
      android = true;
      abi = "armeabi-v7a";
      cpu = "arm";
      triple = "armv7a-linux-androideabi";
      ffarch = "arm";
    };
    android-x86_64 = {
      android = true;
      abi = "x86_64";
      cpu = "x86_64";
      triple = "x86_64-linux-android";
      ffarch = "x86_64";
    };
    mingw-x86_64 = {
      android = false;
      cpu = "x86_64";
      triple = "x86_64-w64-mingw32";
      ffarch = "x86_64";
    };
  };
  mkTarget =
    name: target:
    let
      bin = if target.android then "${ndk}/toolchains/llvm/prebuilt/linux-x86_64/bin" else "${mingw}/bin";
      cc = if target.android then "${bin}/${target.triple}26-clang" else "${bin}/${target.triple}-gcc";
      cxx = if target.android then "${bin}/${target.triple}26-clang++" else "${bin}/${target.triple}-g++";
      ar = if target.android then "${bin}/llvm-ar" else "${bin}/${target.triple}-ar";
      ranlib = if target.android then "${bin}/llvm-ranlib" else "${bin}/${target.triple}-ranlib";
      strip = if target.android then "${bin}/llvm-strip" else "${bin}/${target.triple}-strip";
      # NDK r26 needs both flags for LOAD and RELRO alignment on 16 KiB devices.
      pageFlags = lib.optionalString (
        target.android && target.cpu != "arm"
      ) "-Wl,-z,max-page-size=16384 -Wl,-z,common-page-size=16384";
      ndkEgl = pkgs.writeTextDir "lib/pkgconfig/egl.pc" ''
        Name: EGL
        Description: Android NDK system EGL
        Version: 1.5
        Libs: -lEGL
      '';
      native = pkgs.writeText "nova-native-tools-meson.ini" ''
        [binaries]
        c = '${pkgs.stdenv.cc}/bin/cc'
        cpp = '${pkgs.stdenv.cc}/bin/c++'
      '';
      cross = pkgs.writeText "nova-${name}-meson.ini" ''
        [binaries]
        c = '${cc}'
        cpp = '${cxx}'
        ar = '${ar}'
        strip = '${strip}'
        pkg-config = '${pkgs.pkg-config}/bin/pkg-config'
        ${lib.optionalString (!target.android) "windres = '${bin}/${target.triple}-windres'"}
        [host_machine]
        system = '${if target.android then "android" else "windows"}'
        cpu_family = '${target.cpu}'
        cpu = '${target.cpu}'
        endian = 'little'
        [properties]
        needs_exe_wrapper = true
        [built-in options]
        c_args = ['-O2', '-ffunction-sections', '-fdata-sections']
        cpp_args = ['-O2', '-ffunction-sections', '-fdata-sections']
        c_link_args = ['-Wl,--gc-sections'${
          lib.optionalString (!target.android) ", '-L${mcfgthreads}/lib'"
        }]
        cpp_link_args = ['-Wl,--gc-sections', '-static-libstdc++'${
          lib.optionalString (!target.android) ", '-L${mcfgthreads}/lib'"
        }]
      '';
      cmakeCross = pkgs.writeText "nova-${name}-cmake.cmake" (
        if target.android then
          ''
            set(CMAKE_SYSTEM_NAME Android)
            set(CMAKE_ANDROID_NDK "${ndk}")
            set(CMAKE_SYSTEM_VERSION 26)
            set(CMAKE_ANDROID_ARCH_ABI "${target.abi}")
            set(CMAKE_ANDROID_STL_TYPE c++_static)
          ''
        else
          ''
            set(CMAKE_SYSTEM_NAME Windows)
            set(CMAKE_SYSTEM_PROCESSOR AMD64)
            set(CMAKE_C_COMPILER "${cc}")
            set(CMAKE_CXX_COMPILER "${cxx}")
            set(CMAKE_AR "${ar}")
            set(CMAKE_RANLIB "${ranlib}")
            set(CMAKE_RC_COMPILER "${bin}/${target.triple}-windres")
            set(CMAKE_EXE_LINKER_FLAGS_INIT "-L${mcfgthreads}/lib")
            set(CMAKE_SHARED_LINKER_FLAGS_INIT "-L${mcfgthreads}/lib")
          ''
      );
      setup = deps: ''
        # Host stdenv headers/flags must not leak into an NDK or MinGW compile.
        unset CPATH C_INCLUDE_PATH CPLUS_INCLUDE_PATH LIBRARY_PATH
        export NIX_CFLAGS_COMPILE=
        export NIX_LDFLAGS=${
          lib.escapeShellArg (lib.optionalString (!target.android) "-L${mcfgthreads}/lib")
        }
        export CC=${cc} CXX=${cxx} AR=${ar} RANLIB=${ranlib} STRIP=${strip}${
          lib.optionalString (!target.android) "\n      export PATH=${bin}:$PATH"
        }
        export PKG_CONFIG_PATH=
        export PKG_CONFIG_LIBDIR=${
          lib.escapeShellArg (lib.concatMapStringsSep ":" (dep: "${dep}/lib/pkgconfig") deps)
        }
      '';
      base =
        pname: source: extra:
        pkgs.stdenvNoCC.mkDerivation (
          {
            pname = "nova-${pname}-${name}";
            inherit (source) version src;
            strictDeps = true;
            nativeBuildInputs = [
              pkgs.stdenv.cc
              pkgs.gnumake
              pkgs.pkg-config
              pkgs.meson
              pkgs.ninja
              pkgs.cmake
              (pkgs.python3.withPackages (ps: [ ps.jinja2 ]))
              pkgs.perl
              pkgs.nasm
            ];
            dontConfigure = true;
            dontFixup = true; # These libraries are for Android/Windows, never the host.
            enableParallelBuilding = true;
            buildPhase = "";
            installPhase = "";
          }
          // extra
        );
      zlib = base "zlib" pkgs.zlib {
        buildPhase = ''
          ${setup [ ]}
          cmake -S . -B build -G Ninja -DCMAKE_TOOLCHAIN_FILE=${cmakeCross} \
            -DCMAKE_INSTALL_PREFIX=$out -DCMAKE_INSTALL_LIBDIR=lib \
            -DCMAKE_BUILD_TYPE=Release -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
            -DZLIB_BUILD_EXAMPLES=OFF -DZLIB_BUILD_TESTING=OFF
          cmake --build build --target zlibstatic --parallel "$NIX_BUILD_CORES"
        '';
        installPhase = ''
          mkdir -p $out/lib/pkgconfig $out/include
          archive=$(find build -maxdepth 2 -name '*.a' -print -quit)
          test -n "$archive"
          cp "$archive" $out/lib/libz.a
          cp zlib.h build/zconf.h $out/include/
          cat > $out/lib/pkgconfig/zlib.pc <<EOF
          prefix=$out
          libdir=\''${prefix}/lib
          includedir=\''${prefix}/include
          Name: zlib
          Description: zlib (Nova native build)
          Version: ${pkgs.zlib.version}
          Libs: -L\''${libdir} -lz
          Cflags: -I\''${includedir}
          EOF
        '';
      };
      mbedtls = base "mbedtls" pkgs.mbedtls {
        buildPhase = ''
          ${setup [ ]}
          cmake -S . -B build -G Ninja -DCMAKE_TOOLCHAIN_FILE=${cmakeCross} \
            -DCMAKE_INSTALL_PREFIX=$out -DCMAKE_INSTALL_LIBDIR=lib \
            -DCMAKE_BUILD_TYPE=Release -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
            -DENABLE_TESTING=OFF -DENABLE_PROGRAMS=OFF \
            -DUSE_SHARED_MBEDTLS_LIBRARY=OFF -DUSE_STATIC_MBEDTLS_LIBRARY=ON \
            -DMBEDTLS_FATAL_WARNINGS=OFF
          cmake --build build --parallel "$NIX_BUILD_CORES"
        '';
        installPhase = ''
          cmake --install build
          mkdir -p $out/lib/pkgconfig
          cat > $out/lib/pkgconfig/mbedtls.pc <<EOF
          prefix=$out
          libdir=\''${prefix}/lib
          includedir=\''${prefix}/include
          Name: mbedtls
          Description: mbed TLS (Nova native build)
          Version: ${pkgs.mbedtls.version}
          Libs: -L\''${libdir} -lmbedtls -lmbedx509 -lmbedcrypto ${
            lib.optionalString (!target.android) "-lws2_32 -lbcrypt"
          }
          Cflags: -I\''${includedir}
          EOF
        '';
      };
      dav1d = base "dav1d" pkgs.dav1d {
        buildPhase = ''
          ${setup [ ]}
          meson setup build --cross-file=${cross} --native-file=${native} --prefix=$out --libdir=lib \
            --buildtype=release --default-library=static --wrap-mode=nodownload \
            -Denable_tools=false -Denable_tests=false
          ninja -C build -j"$NIX_BUILD_CORES"
        '';
        installPhase = "ninja -C build install";
      };
      ffmpeg = base "ffmpeg" pkgs.ffmpeg {
        buildPhase = ''
          ${setup (
            if target.android then
              [
                zlib
                mbedtls
                dav1d
              ]
            else
              [
                zlib
                dav1d
              ]
          )}
          ./configure --prefix=$out --libdir=$out/lib \
            --enable-cross-compile --target-os=${if target.android then "android" else "mingw32"} \
            --arch=${target.ffarch} --cc="$CC" --cxx="$CXX" --ar="$AR" --ranlib="$RANLIB" --strip="$STRIP" \
            --pkg-config=${pkgs.pkg-config}/bin/pkg-config --pkg-config-flags=--static \
            --enable-shared --disable-static --enable-pic --disable-autodetect ${lib.optionalString target.android "--enable-version3"} \
            --disable-programs --disable-doc --disable-debug --disable-avdevice \
            --disable-everything \
            --enable-decoder=h264,hevc,av1,libdav1d,vp8,vp9,mpeg1video,mpeg2video,mpeg4,vc1,wmv3,theora,mjpeg,png,webp,aac,aac_latm,ac3,eac3,dca,mp3,mp3float,mp2,flac,alac,opus,vorbis,wmav1,wmav2,wmapro,pcm_s16le,pcm_s16be,pcm_s24le,pcm_s24be,pcm_s32le,pcm_s32be,pcm_f32le,pcm_f64le,pcm_u8,ass,ssa,srt,subrip,webvtt,movtext,pgssub,dvdsub,dvbsub,xsub \
            --enable-demuxer=matroska,mov,mpegts,mpegps,avi,asf,hls,aac,ac3,eac3,mp3,ogg,flac,wav,h264,hevc,ivf,ass,srt,webvtt,microdvd,mpl2,subviewer,sami,realtext,subrip,ttml,sup,image2 \
            --enable-parser=h264,hevc,av1,vp8,vp9,aac,aac_latm,ac3,dca,flac,mpegaudio,mpeg4video,mpegvideo,opus,vorbis,vc1,png,webp \
            --enable-bsf=aac_adtstoasc,extract_extradata,h264_mp4toannexb,hevc_mp4toannexb,av1_frame_merge,av1_frame_split,vp9_superframe,vp9_superframe_split \
            --enable-filter=abuffer,abuffersink,buffer,buffersink,aformat,aresample,atempo,anull,null,format,scale,transpose,volume,pan,bwdif,yadif \
            --enable-encoder=png,mjpeg \
            --enable-muxer=mp4,mov,hls,mpegts,matroska,webvtt \
            --enable-zlib ${
              if target.android then "--enable-mbedtls" else "--enable-schannel"
            } --enable-libdav1d \
            --enable-protocol=file,pipe,fd,http,https,tcp,tls,crypto,data \
            ${lib.optionalString target.android "--enable-jni --enable-mediacodec --enable-decoder=h264_mediacodec,hevc_mediacodec,av1_mediacodec,vp8_mediacodec,vp9_mediacodec,mpeg2_mediacodec,mpeg4_mediacodec"} \
            ${
              lib.optionalString (!target.android)
                "--cross-prefix=${bin}/${target.triple}- --enable-d3d11va --enable-dxva2 --enable-hwaccel=h264_d3d11va,hevc_d3d11va,av1_d3d11va,vp9_d3d11va,mpeg2_d3d11va,vc1_d3d11va,wmv3_d3d11va"
            } \
            --extra-cflags="-O2 -ffunction-sections -fdata-sections" \
            --extra-ldflags="-Wl,--gc-sections ${
              if target.android then pageFlags else "-L${mcfgthreads}/lib -static-libgcc"
            }"
          make -j"$NIX_BUILD_CORES"
        '';
        installPhase = ''
          make install
          mkdir -p $out/share/nova-native
          cp ffbuild/config.mak config.h $out/share/nova-native/
          cp COPYING.LGPLv3 LICENSE.md $out/share/nova-native/
        '';
      };
      freetype = base "freetype" pkgs.freetype {
        buildPhase = ''
          ${setup [ ]}
          cmake -S . -B build -G Ninja -DCMAKE_TOOLCHAIN_FILE=${cmakeCross} \
            -DCMAKE_INSTALL_PREFIX=$out -DCMAKE_INSTALL_LIBDIR=lib \
            -DCMAKE_BUILD_TYPE=Release -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
            -DBUILD_SHARED_LIBS=OFF -DFT_DISABLE_ZLIB=TRUE -DFT_DISABLE_BZIP2=TRUE \
            -DFT_DISABLE_PNG=TRUE -DFT_DISABLE_HARFBUZZ=TRUE -DFT_DISABLE_BROTLI=TRUE
          cmake --build build --parallel "$NIX_BUILD_CORES"
        '';
        installPhase = "cmake --install build";
      };
      fribidi = base "fribidi" pkgs.fribidi {
        buildPhase = ''
          ${setup [ ]}
          meson setup build --cross-file=${cross} --native-file=${native} --prefix=$out --libdir=lib \
            --buildtype=release --default-library=static --wrap-mode=nodownload \
            -Ddocs=false -Dbin=false -Dtests=false
          ninja -C build -j"$NIX_BUILD_CORES"
        '';
        installPhase = "ninja -C build install";
      };
      harfbuzz = base "harfbuzz" pkgs.harfbuzz {
        postPatch = "patchShebangs src/*.py";
        buildPhase = ''
          ${setup [ ]}
          meson setup build --cross-file=${cross} --native-file=${native} --prefix=$out --libdir=lib \
            --buildtype=release --default-library=static --wrap-mode=nodownload \
            -Dglib=disabled -Dgobject=disabled -Dcairo=disabled -Dicu=disabled \
            -Dfreetype=disabled -Dsubset=disabled -Draster=disabled -Dvector=disabled \
            -Dtests=disabled -Ddocs=disabled -Dutilities=disabled${
              lib.optionalString (!target.android) " -Dcpp_args='-O2 -I${lib.getDev mcfgthreads}/include'"
            }
          ninja -C build -j"$NIX_BUILD_CORES"
        '';
        installPhase = "ninja -C build install";
      };
      libass = base "libass" pkgs.libass {
        buildPhase = ''
          ${setup [
            freetype
            fribidi
            harfbuzz
          ]}
          export CFLAGS='-O2 -fPIC'
          export LDFLAGS='${lib.optionalString (!target.android) "-L${mcfgthreads}/lib"}'
          ./configure --prefix=$out --libdir=$out/lib \
            --build=${pkgs.stdenv.buildPlatform.config} --host=${target.triple} \
            --enable-static --disable-shared --disable-fontconfig --disable-test${lib.optionalString target.android " --disable-require-system-font-provider"}
          make -j"$NIX_BUILD_CORES"
        '';
        installPhase = "make install";
      };
      libplacebo = base "libplacebo" pkgs.libplacebo {
        postPatch = ''
          substituteInPlace meson.build --replace-warn 'python_env.append' '#'
          ${lib.optionalString (!target.android) ''
            substituteInPlace src/meson.build \
              --replace-fail "find_program('llvm-dlltool', 'dlltool')" "find_program('${bin}/${target.triple}-dlltool')"
          ''}
        '';
        buildPhase = ''
          ${setup [ ]}
          export PYTHONPATH=${pkgs.python3Packages.jinja2}/${pkgs.python3.sitePackages}:${pkgs.python3Packages.markupsafe}/${pkgs.python3.sitePackages}
          meson setup build --cross-file=${cross} --native-file=${native} --prefix=$out --libdir=lib \
            --buildtype=release --default-library=static --wrap-mode=nodownload \
            --auto-features=disabled -Ddemos=false -Dtests=false -Dbench=false \
            -Dc_args='-O2 -I${pkgs.vulkan-headers}/include' \
            -Dcpp_args='-O2 -I${pkgs.fast-float}/include${
              lib.optionalString (!target.android) " -I${lib.getDev mcfgthreads}/include"
            }'
          ninja -C build -j"$NIX_BUILD_CORES"
        '';
        installPhase = "ninja -C build install";
      };
      mpv = base "mpv" pkgs.mpv-unwrapped {
        postPatch =
          "patchShebangs TOOLS/*.py"
          + lib.optionalString (!target.android) ''

            # Meson adds an explicit -lstdc++ for link_language: 'cpp', which
            # defeats GCC's -static-libstdc++. Link the archive explicitly.
            substituteInPlace meson.build \
              --replace-fail "libmpv = library('mpv', sources," \
              ${lib.escapeShellArg "dependencies += cc.find_library('stdc++', static: true)\nlibmpv = library('mpv', sources,"}
          '';
        buildPhase = ''
          ${setup (
            [
              libass
              freetype
              fribidi
              harfbuzz
              libplacebo
              ffmpeg
              zlib
              dav1d
            ]
            ++ lib.optionals target.android [
              mbedtls
              ndkEgl
            ]
          )}
          # Only the embeddable OpenGL renderer is needed; gpu-next/Vulkan,
          # scripting, optical-media access and desktop windows are not used.
          meson setup build --cross-file=${cross} --native-file=${native} --prefix=$out --libdir=lib \
            --buildtype=release --default-library=shared --wrap-mode=nodownload -Dprefer_static=true \
            --auto-features=disabled -Dlibmpv=true -Dcplayer=false -Dtests=false \
            -Dbuild-date=false -Dlua=disabled -Dgl=enabled -Dplain-gl=enabled \
            -Dvector=enabled -Dzlib=enabled${
              lib.optionalString (!target.android) " -Dcpp_args='-O2 -I${lib.getDev mcfgthreads}/include'"
            } \
            ${
              if target.android then
                "-Degl=enabled -Degl-android=enabled -Dopensles=enabled -Dandroid-media-ndk=enabled -Dc_link_args='-Wl,--gc-sections ${pageFlags}' -Dcpp_link_args='-Wl,--gc-sections -static-libstdc++ ${pageFlags}'"
              else
                "-Dwasapi=enabled -Dwin32-threads=enabled -Dgl-win32=enabled -Dd3d-hwaccel=enabled -Dgl-dxinterop=enabled -Dc_link_args='-Wl,--gc-sections -static-libgcc -L${mcfgthreads}/lib'"
            }
          ninja -C build -j"$NIX_BUILD_CORES"${lib.optionalString (!target.android) " -v"}
        '';
        installPhase = ''
          ninja -C build install
          mkdir -p $out/share/nova-native
          cp build/config.h $out/share/nova-native/
          cp Copyright $out/share/nova-native/
        '';
      };
      probe =
        pkgs.runCommand "nova-native-smoke-${name}"
          {
            nativeBuildInputs = [
              pkgs.stdenv.cc
              pkgs.pkg-config
            ];
          }
          ''
            ${setup [
              mpv
              ffmpeg
            ]}
            mkdir -p $out/bin
            "$CC" -O2 -Wall -Wextra -Werror ${pageFlags} ${
              lib.optionalString (!target.android) "-L${mcfgthreads}/lib -static-libgcc"
            } ${./smoke.c} -o $out/bin/smoke${lib.optionalString (!target.android) ".exe"} \
              $(pkg-config --cflags --libs mpv libavformat libavcodec libavutil)
          '';
      bundle =
        pkgs.runCommand "nova-native-media-${name}"
          {
            nativeBuildInputs = [
              pkgs.python3
              pkgs.patchelf
            ];
          }
          ''
            mkdir -p $out/lib $out/include $out/lib/pkgconfig $out/share/nova-native
            cp -r ${ffmpeg}/include/* ${mpv}/include/* $out/include/
            cp ${ffmpeg}/lib/pkgconfig/*.pc ${mpv}/lib/pkgconfig/*.pc $out/lib/pkgconfig/
            chmod u+w $out/lib/pkgconfig/*.pc
            ${
              if target.android then
                ''
                  cp -L ${ffmpeg}/lib/*.so ${mpv}/lib/*.so $out/lib/
                  chmod u+w $out/lib/*.so
                  for library in $out/lib/*.so; do
                    patchelf --remove-rpath "$library"
                    ${strip} --strip-unneeded "$library"
                  done
                  cp ${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt $out/share/nova-native/cacert.pem
                ''
              else
                ''
                  find ${ffmpeg} ${mpv} -type f -name '*.dll' -exec cp {} $out/lib/ \;
                  cp ${ffmpeg}/lib/*.dll.a ${mpv}/lib/*.dll.a $out/lib/
                  # MinGW's thread runtime is the only non-system DLL used by this profile.
                  cp ${mcfgthreads}/bin/libmcfgthread-2.dll $out/lib/
                  chmod u+w $out/lib/*.dll
                  ${strip} --strip-unneeded $out/lib/*.dll
                ''
            }
            cp ${ffmpeg}/share/nova-native/config.h $out/share/nova-native/ffmpeg-config.h
            cp ${ffmpeg}/share/nova-native/config.mak $out/share/nova-native/ffmpeg-config.mak
            cp ${mpv}/share/nova-native/config.h $out/share/nova-native/mpv-config.h
            cp ${mpv}/share/nova-native/Copyright $out/share/nova-native/mpv-Copyright
            cp ${ffmpeg}/share/nova-native/COPYING.LGPLv3 $out/share/nova-native/
            cp ${ffmpeg}/share/nova-native/LICENSE.md $out/share/nova-native/ffmpeg-LICENSE.md
            collect_licenses() {
              package="$1"
              source="$2"
              if test -d "$source"; then
                source_dir="$source"
              else
                source_dir="$TMPDIR/licenses-$package"
                mkdir -p "$source_dir"
                tar -xf "$source" -C "$source_dir" --strip-components=1
              fi
              mkdir -p "$out/share/licenses/$package"
              while IFS= read -r license; do
                # Preserve paths such as FreeType's docs/FTL.TXT and docs/GPLv2.TXT.
                relative="''${license#"$source_dir/"}"
                mkdir -p "$out/share/licenses/$package/$(dirname "$relative")"
                cp "$license" "$out/share/licenses/$package/$relative"
              done < <(find "$source_dir" -maxdepth 2 -type f \
                \( -iname 'copying*' -o -iname '*license*' -o -iname 'ftl.txt' -o -iname '*gpl*.txt' -o -iname '*exception*.txt' \))
            }
            ${lib.concatMapStringsSep "\n"
              (package: "collect_licenses ${lib.escapeShellArg package.pname} ${package.src}")
              (
                [
                  pkgs.ffmpeg
                  pkgs.mpv-unwrapped
                  pkgs.zlib
                  pkgs.dav1d
                  pkgs.freetype
                  pkgs.fribidi
                  pkgs.harfbuzz
                  pkgs.libass
                  pkgs.libplacebo
                  pkgs.fast-float
                ]
                ++ (if target.android then [ pkgs.mbedtls ] else [ mcfgthreads ])
              )
            }
            ${lib.optionalString target.android ''
              mkdir -p $out/share/licenses/cacert
              cp ${
                pkgs.fetchurl {
                  url = "https://www.mozilla.org/media/MPL/2.0/index.815ca599c9df.txt";
                  hash = "sha256-+rPda9qyJvHAhjCx3ZF+Efy07F4eAg4sFvg6ChOGPoU=";
                }
              } $out/share/licenses/cacert/MPL-2.0.txt
            ''}
            # zlib's license text is in its public header.
            cp ${zlib}/include/zlib.h $out/share/licenses/zlib/
            cp ${
              pkgs.writeText "sources.json" (
                builtins.toJSON {
                  inherit name;
                  nixpkgs = (builtins.fromJSON (builtins.readFile ../../flake.lock)).nodes.nixpkgs.locked.rev;
                  versions =
                    lib.mapAttrs (_: package: package.version) {
                      inherit
                        ffmpeg
                        mpv
                        zlib
                        dav1d
                        freetype
                        fribidi
                        harfbuzz
                        libass
                        libplacebo
                        ;
                    }
                    // lib.optionalAttrs target.android { mbedtls = mbedtls.version; };
                  tls =
                    if target.android then "mbedtls (set tls-ca-file / ca_file to cacert.pem)" else "Windows Schannel";
                }
              )
            } $out/share/nova-native/sources.json
            # A copied bundle must not retain the build machine's pkg-config paths.
            python3 - "$out" <<'PY'
            import pathlib, sys
            root = pathlib.Path(sys.argv[1])
            for pc in (root / "lib/pkgconfig").glob("*.pc"):
                lines = []
                for line in pc.read_text().splitlines():
                    key = line.split("=", 1)[0]
                    if key == "prefix": line = "prefix=''${pcfiledir}/../.."
                    elif key == "libdir": line = "libdir=''${prefix}/lib"
                    elif key == "includedir": line = "includedir=''${prefix}/include"
                    elif line.startswith(("Libs.private:", "Requires.private:")): continue
                    lines.append(line)
                pc.write_text("\n".join(lines) + "\n")
            PY
            python3 ${./inspect.py} "$out" --target ${name} \
              --tool ${if target.android then "${bin}/llvm-readelf" else "${bin}/${target.triple}-objdump"} \
              > $out/share/nova-native/validation.txt
            cat $out/share/nova-native/validation.txt
            (cd $out && find lib include share -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS)
          '';
      notices = pkgs.runCommand "nova-native-notices-${name}" { } ''
        printf 'NOVA NATIVE MEDIA DEPENDENCIES\n\n' > "$out"
        cat ${bundle}/share/nova-native/sources.json >> "$out"
        printf '\n\n' >> "$out"
        while IFS= read -r license; do
          printf '\n===== %s =====\n\n' "''${license#${bundle}/share/licenses/}" >> "$out"
          cat "$license" >> "$out"
        done < <(find ${bundle}/share/licenses -type f | sort)
      '';
    in
    {
      inherit
        notices
        ffmpeg
        mpv
        zlib
        mbedtls
        dav1d
        freetype
        fribidi
        harfbuzz
        libass
        libplacebo
        probe
        bundle
        ;
    };
  builds = lib.mapAttrs mkTarget targets;
in
lib.foldlAttrs (
  acc: name: value:
  acc
  // {
    "ffmpeg-${name}" = value.ffmpeg;
    "zlib-${name}" = value.zlib;
    "mbedtls-${name}" = value.mbedtls;
    "dav1d-${name}" = value.dav1d;
    "mpv-${name}" = value.mpv;
    "libplacebo-${name}" = value.libplacebo;
    "libass-${name}" = value.libass;
    "probe-${name}" = value.probe;
    "bundle-${name}" = value.bundle;
    "notices-${name}" = value.notices;
  }
) { } builds
