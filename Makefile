

.PHONY: apk android-build android-build-release android-build-release-universal \
	android-run android-run-release \
	linux-build linux-build-release linux-run linux-run-preview linux-run-release

# Run these inside the appropriate dev shell: `nix develop` for Linux or
# `nix develop .#android` for Android. Override the target for an emulator,
# e.g. `make android-run ANDROID_TARGET=x86_64-linux-android`.
ANDROID_TARGET ?= aarch64-linux-android
# Local release builds stay quick; tagged CI selects cd-release explicitly.
RELEASE_PROFILE ?= release
# Set UPDATE_FEATURES= to build APKs without any GitHub update checks.
UPDATE_FEATURES ?= ,update-check
ANDROID_ARGS = --target $(ANDROID_TARGET) --no-default-features --features android$(UPDATE_FEATURES) --lib -p nova

# cargo-apk2 (not cargo-apk) compiles the Java foreground service and
# JobScheduler job to DEX and declares them in the manifest.
apk: android-build

android-build:
	cargo apk2 build $(ANDROID_ARGS)

android-build-release:
	cargo apk2 build $(ANDROID_ARGS) --profile $(RELEASE_PROFILE)

# Local three-ABI alternative to CI's parallel single-ABI builds. The release
# packager accepts either input to produce the same four APK variants.
android-build-release-universal:
	cargo apk2 build --no-default-features --features android$(UPDATE_FEATURES) --lib -p nova --profile $(RELEASE_PROFILE)

android-run:
	cargo apk2 run $(ANDROID_ARGS)

android-run-release:
	cargo apk2 run $(ANDROID_ARGS) --profile $(RELEASE_PROFILE)

linux-build:
	cargo build

linux-build-release:
	cargo build --profile $(RELEASE_PROFILE)

linux-run:
	cargo run

# Retained preview feature currently uses compiled UI with a build warning.
# Keep this target independent of the optional local nova-kit override.
linux-run-preview:
	cargo run --features live-preview

linux-run-release:
	cargo run --profile $(RELEASE_PROFILE)
