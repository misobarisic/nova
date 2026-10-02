# Roboto fonts

`Roboto-Regular.ttf` and `Roboto-Bold.ttf` are the hinted static faces from
[googlefonts/roboto-2](https://github.com/googlefonts/roboto-2/tree/38062f4b4a0be4346d07a928408da21602545e9e/src/hinted),
revision `38062f4b4a0be4346d07a928408da21602545e9e`. Both use the Apache-2.0
license in `LICENSE.txt`, included in Nova's in-app license catalog.

`crates/ui/appwindow.slint` imports both faces and sets Roboto as the window's
default family. `crates/ui/build.rs` embeds them for packaged builds on every
platform. Regular covers body text; bold covers the UI's 600/700 emphasis through
font weight matching. System fallback fonts supply glyphs absent from Roboto,
including emoji and scripts it does not cover. Linux live preview loads the
same imports from the checkout.

Android's mpv subtitle setup also uses the regular face; see
`crates/player/src/lib.rs`.
