# Player settings icons

The video-player settings category rows use unmodified [Lucide](https://lucide.dev/)
SVGs from [lucide-icons/lucide at `c29c276f16590cf017126671535ad8c792be64d8`](https://github.com/lucide-icons/lucide/tree/c29c276f16590cf017126671535ad8c792be64d8/icons).

| Player setting | Upstream icon |
| --- | --- |
| Subtitles | `captions.svg` |
| Audio | `eighth-note.svg` |
| Hardware decoding | `cpu.svg` |
| Playback speed | `gauge.svg` |
| Subtitle appearance and timing | `captions.svg` |
| Intro skipping | `skip-forward.svg` |
| Footer Next episode | `skip-forward.svg` |
| Second subtitles | `languages.svg` |
| Scene bookmarks | `bookmark.svg` |
| Saved-scene play / edit / clear / delete | `play.svg` / `pencil.svg` / `eraser.svg` / `trash.svg` |
| Saved-scene note save / clear / cancel | `check.svg` / `eraser.svg` / `x.svg` |
| Subtitle menu appearance shortcut | `sliders-horizontal.svg` |
| Chapters | `list-video.svg` |
| Try another stream | `refresh-cw.svg` |
| Sleep timer | `moon-star.svg` |

Lucide's [official metadata](https://github.com/lucide-icons/lucide/blob/c29c276f16590cf017126671535ad8c792be64d8/icons/eighth-note.json)
lists `music-2` as a deprecated alias of the pictured single flagged note,
`eighth-note`. The former `music-2`, `music`, and `music-note` SVG filenames are
absent at the pinned revision (`music` now aliases the paired `eighth-notes`).
The same revision's [trash metadata](https://github.com/lucide-icons/lucide/blob/c29c276f16590cf017126671535ad8c792be64d8/icons/trash.json)
lists `trash-2` as a deprecated duplicate alias of `trash`; `trash.svg` is the
unmodified upstream asset used by both saved-scene action surfaces.
The icons retain their 24×24 view box, 2px stroke, and rounded caps/joins.
`MenuRow.icon` renders them at 24px and recolors them with
`Theme.current.accent_soft`, so they follow Nova's selected theme.
The footer Next episode control renders `skip-forward.svg` at 26px in the
theme's bright text color. Other transport controls and track-picker icons use
Nova's existing glyphs.

Copyright © 2026 Lucide Icons and Contributors. Distributed under the ISC
License. The complete upstream [LICENSE](LICENSE) also retains its Feather MIT
notice and icon list. Nova includes this text with its bundled licenses.
