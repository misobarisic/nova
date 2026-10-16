# Nova patch to tiny-skia 0.12.0

Source: <https://crates.io/crates/tiny-skia/0.12.0>, BSD-3-Clause.

`src/shaders/sweep_gradient.rs`: apply angular scale/bias when **either** term
differs from identity. The upstream `&&` guard skips normalization for partial
sweeps beginning at zero degrees, and for full-period sweeps with a shifted
start. The regression is exercised through the desktop COLRv1 painter in
`vendor/femtovg/src/text/colr.rs`.

This patch does not change features, dependencies, or the public API.
