# Bundled UI fonts

`Roboto-Regular.ttf` and `Roboto-Bold.ttf` are the hinted static faces from
[googlefonts/roboto-2](https://github.com/googlefonts/roboto-2/tree/38062f4b4a0be4346d07a928408da21602545e9e/src/hinted),
revision `38062f4b4a0be4346d07a928408da21602545e9e`. Both use the Apache-2.0
license in `LICENSE.txt`, included in Nova's in-app license catalog.

`crates/ui/appwindow.slint` imports both faces and sets Roboto as the window's
default family. `crates/ui/build.rs` embeds them for packaged builds on every
platform. Regular covers body text; bold covers the UI's 600/700 emphasis through
font weight matching. Linux live preview loads the same imports from the
checkout. System fallback still supplies scripts absent from Roboto.

`NotoColorEmoji.ttf` is the unmodified 2D color font from
[googlefonts/noto-emoji](https://github.com/googlefonts/noto-emoji/tree/e20cbc2bbec1926686be9f9bee7d1d2cfa1fea0e/2D/fonts),
revision `e20cbc2bbec1926686be9f9bee7d1d2cfa1fea0e`, licensed under OFL-1.1
in `NotoColorEmoji-LICENSE.txt`. Its SHA-256 is
`15671215ab769fdc7162a045d56fd7d7e477c51b04e6b3c761d914d8fdd6cc44`.
The font and license ship in every packaged build. `crates/ui/src/fonts.rs`
registers the embedded font and appends it to Slint's generic fallback chains
before showing the window. This covers flag pairs and joined emoji without
relying on the desktop or Android system font inventory; Roboto stays the
primary text family. Merely importing/registering the emoji family does not
add it to those fallback chains.

Android's mpv subtitle setup also uses the regular face; see
`crates/player/src/lib.rs`.
