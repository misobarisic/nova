Re:ANIME provider behavior is independently ported from the ReAnime source in
[yuzono/anime-extensions](https://github.com/yuzono/anime-extensions/tree/master/src/en/reanime),
including its Apache-2.0-licensed API requests, episode identifiers, FlixCloud
extraction and segment decoding conventions. The upstream Kotlin source is not
included. The upstream license is available at
https://github.com/yuzono/anime-extensions/blob/master/LICENSE.

Playback uses the upstream extension's external decoding service, enc-dec.app.
It receives FlixCloud embed/stream data required for token and playlist decoding.
The Nova host performs bounded local HLS playlist rewriting and segment decoding.
This addon requires a Nova runtime with permissions.flixcloudPlayback support.
Direct MKV download preparation, font installation, and intro/outro markers from
the Android extension are not included.
