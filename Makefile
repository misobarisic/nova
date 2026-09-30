

.PHONY: apk android-build android-build-release android-build-release-universal \
	android-run android-run-release \
	linux-build linux-build-release linux-run linux-run-preview linux-run-release

# Run these inside the appropriate dev shell: `nix develop` for Linux or
# `nix develop .#android` for Android. Override the target for an emulator,
# e.g. `make android-run ANDROID_TARGET=x86_64-linux-android`.
ANDROID_TARGET ?= aarch64-linux-android
ANDROID_ARGS = --target $(ANDROID_TARGET) --no-default-features --features android --lib -p nova

# cargo-apk2 (not cargo-apk) compiles the Java foreground service and
# JobScheduler job to DEX and declares them in the manifest.
apk: android-build

android-build:
	cargo apk2 build $(ANDROID_ARGS)

android-build-release:
	cargo apk2 build $(ANDROID_ARGS) --release

# Release packaging starts from one universal APK, then derives ABI-specific APKs.
android-build-release-universal:
	cargo apk2 build --no-default-features --features android --lib -p nova --release

android-run:
	cargo apk2 run $(ANDROID_ARGS)

android-run-release:
	cargo apk2 run $(ANDROID_ARGS) --release

linux-build:
	cargo build

linux-build-release:
	cargo build --release

linux-run:
	cargo run

# The `cargo dev` alias enables Slint's interpreter-backed Linux preview.
linux-run-preview:
	cargo dev

linux-run-release:
	cargo run --release
