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
| Second subtitles | `languages.svg` |
| Scene bookmarks | `bookmark.svg` |
| Try another stream | `refresh-cw.svg` |

Lucide's [official metadata](https://github.com/lucide-icons/lucide/blob/c29c276f16590cf017126671535ad8c792be64d8/icons/eighth-note.json)
lists `music-2` as a deprecated alias of the pictured single flagged note,
`eighth-note`. The former `music-2`, `music`, and `music-note` SVG filenames are
absent at the pinned revision (`music` now aliases the paired `eighth-notes`).
The icons retain their 24×24 view box, 2px stroke, and rounded caps/joins.
`MenuRow.icon` renders them at 24px and recolors them with
`Theme.current.accent_soft`, so they follow Nova's selected theme.
Transport controls and track-picker icons continue using Nova's existing glyphs.

Copyright © 2026 Lucide Icons and Contributors. Distributed under the ISC
License. The complete upstream [LICENSE](LICENSE) also retains its Feather MIT
notice and icon list. Nova includes this text with its bundled licenses.
