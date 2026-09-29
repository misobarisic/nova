

.PHONY: apk

# cargo-apk2 (not cargo-apk): Android needs the Java foreground service and
# JobScheduler job compiled to DEX and declared in the manifest. Enter the
# `.#android` dev shell first (`nix develop .#android`).
apk:
	cargo apk2 build --target aarch64-linux-android --no-default-features --lib -p nova
