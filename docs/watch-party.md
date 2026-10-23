# Watch party

Watch parties share a temporary host-led timeline. Each participant resolves and
plays their own source through their own installed addons, with their own local
profile policy, subtitle/audio choices and playback preferences. A party does not
pair devices or grant access to another participant's addons.

## Recommendations from participants

A source card can show **Watch party recommended** when another connected
participant picked the exact same asset and that asset is already present in the
viewer's own current source results for the party's title and episode. This uses
the existing Recommended caption, accent, star and measured responsive card
layout. It does not fetch, inject, select or start a participant's stream.

Matching uses a bounded SHA-256 fingerprint. A torrent matches by its normalized
infohash and explicit selected file index, across addon owners; an unspecified
file index is insufficient. Direct HTTP(S) sources match only when the complete
URL and request headers agree exactly. Signed URLs with different signatures do
not match. Device-relative file, localhost, loopback and link-local sources do
not match across devices. Shared NAS HTTP(S) endpoints remain eligible when their
full URL and headers agree. Provider resolver tokens, local download paths and release-title or
quality similarity do not prove asset identity and receive no party badge.

Only the current title/episode's broker-confirmed identity is used for source
recommendations. The party's existing explicit private-title compatibility flow
can still align playback, but name/episode-number similarity alone cannot
recommend a source. Profile title/addon restrictions, source rules and the local
result filters continue to determine which rows exist.

Participant updates change the badge on existing rows in place. They do not
refetch or reorder results or move keyboard/TV focus. The ordinary local
Recommended and Last used states remain available when the party reason ends.
The participant's own pick is ignored when labelling their local results.

## Lifetime and protocol

The local adapter retains only the chosen asset fingerprint and typed media
binding for the active party/media generation, independently of the result list
that closes when playback starts. Host selections bind explicitly to the next
media generation. Source changes, different media, disconnection and leaving or
ending the party retire the relevant hints; disconnected participants do not
recommend sources. A new process restores no party or live recommendation state.

Reports and participant snapshots carry only an optional fixed 32-byte digest.
Raw source URLs, signed queries, headers, credentials, provider capabilities and
local file paths are never added to party messages. These postcard schema changes
use `nova/party/3`; all participants need a version supporting that protocol. The
`NVP2` invite-ticket envelope is unchanged because its address/capability shape
has not changed.

The deterministic host state machine and bounded transport tests live in
`crates/party`. Exact asset matching is in `src/app/party_source.rs`; the UI
coordinator and in-place repaint are in `src/app/party.rs`. Coordinator regressions
are in `src/app/party_tests.rs`, and measured phone/desktop/TV recommendation
caption coverage is in `tests/streams_layout/card_layout.rs`.
