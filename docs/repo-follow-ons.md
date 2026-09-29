# Repository follow-ons

These are planned improvements; they are not implemented by this document
update.

## Release signing

The tag workflow builds release APKs, while Cargo's release-signing metadata
currently points to the tracked `android/keystore/debug.keystore` with the public
`android` password. Before distributing a production release:

- create a private release keystore and store its path/password in GitHub
  Actions secrets;
- configure the release workflow to use those secrets while keeping the
  debug key for local development; and
- plan the signing-certificate transition: APKs signed with a new key generally
  cannot update installs signed with the existing key, so users may need to
  reinstall unless a supported key-rotation path is arranged.

## Pull-request validation

The current Android pull-request job checks the workspace and builds the
Android target, but it does not run the README's full test gate or formatting
checks. Add a validation job for:

- `cargo fmt --all -- --check`;
- `cargo test --workspace --locked`; and
- optionally `cargo clippy --workspace --all-targets --locked` after the
  existing lint baseline has been reviewed.

Keep the Android APK build as a separate check so failures identify the
platform-specific cause.

## APK license and source delivery

`Settings → About` and `THIRD_PARTY_NOTICES.md` now include the build's Rust
dependency license texts and source links, the native media vendor inventory,
and the bundled font license. That satisfies the in-app notice/catalog part;
it does not deliver the corresponding source for a particular APK.

Before publishing, update the release workflow to attach or link the exact
matching Nova source revision and the corresponding native media source/build
materials for each APK, with ABI and hash provenance. Review applicable
GPL/LGPL distribution requirements for the shipped combination, including
any installation information that applies. The current release workflow only
attaches APKs and GitHub-generated notes.
