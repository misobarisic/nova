# External receiver playback (disabled)

Casting is disabled starting with v0.0.40. Shipped builds hide its buttons,
picker, and remux setting; install no casting callbacks; and cannot hand media
to an external receiver. Android no longer registers its casting foreground
service. Paired Nova playback and watch parties remain available.

The implementation and its isolated tests remain in the repository for future
work. `src/app/casting.rs::ENABLED` disables runtime entry points, and
`CastBridge.available` defaults to false for UI and keyboard/search routing.
There is no user preference or environment override to enable casting. The
following sections document the retained implementation, not released behavior.

Nova can select a Google Cast or UPnP/DLNA video receiver from Home, a title's
Detail page, or the built-in video player. These receivers are independent of
paired Nova devices and library sync. Google Cast uses the pinned oxicast
adapter on desktop and Android; DLNA uses Nova's separate UPnP adapter.

## Choosing an output

The Cast button opens the same device picker in all three places. Selecting a
receiver first checks its control connection; it does not start media or replace
whatever is already playing there. Choose a source normally to start playback
on that output. A busy receiver requires explicit replacement confirmation.

Discovery runs for a bounded period when requested. Refresh searches again;
manual address entry provides a fallback. When local discovery is disabled in
Sync, the picker offers an explicit one-time search without changing that
preference. Devices retain stable selection while discovery results update.
Audio-only receivers cannot be selected for video.

The selected output lasts for the current app session. It is not synced and
does not reconnect or start playback automatically after a restart. Opening or
closing the picker has no effect on receiver playback.

## Transferring and controlling playback

When transferring local playback, Nova keeps the local context until the
receiver confirms the expected media, pause state and resume position. The
local player pauses immediately before remote playback can start. A failed
handoff restores it only after the attempted remote playback is confirmed
stopped. If Stop cannot be confirmed, local playback remains paused and the
interface offers explicit recovery choices.

The controller presents title artwork and actual receiver state rather than
mirroring the video. Pause, seeking, volume, mute and subtitle controls depend
on the receiver and current media plan. Stale observations do not continue
advancing the playhead, mark an episode watched, or advance the queue.

**Stop casting** stops Nova's owned media and releases its relay, RAM buffers
and source leases. It does not start local playback. **Play here**
performs a controlled return to local playback. Another sender taking over the
receiver revokes Nova's control; Nova does not stop or reload that sender's
media. Watch parties and paired-Nova controller sessions remain separate output
modes and cannot be chained through external casting.

## Formats, remuxing and subtitles

Casting never transcodes or encodes audio/video. Nova inspects the selected
source using the existing FFmpeg libraries. A compatible source is relayed
without changing its media bytes. Where only the container or selected audio
track needs changing, Nova can copy compressed packets into fragmented MP4
segments served through a memory-backed HLS presentation.
The receiver still needs to decode those original codecs and profiles.

Remuxing streams through a bounded RAM window; it does not create a full
temporary movie file. **Settings → Player → Casting remux buffer** selects a
32, 64, 128 or 256 MiB retained segment budget for new sessions. The default is
32 MiB on Android and 64 MiB on desktop. Native output and its callback copy
each have an additional fragment cap of 8 MiB at the 32 MiB budget or 16 MiB
at larger budgets; these limits are not a ceiling on total app memory.
An output change can briefly retain both the old and new sessions, with at
most two remux producers. Active HTTP readers count toward retention, and
input/parser buffers are bounded. The producer waits when
the window is full, releases consumed segments as readers finish, and wakes
promptly on cancellation. A fragment that exceeds the limit produces a clear
failure instead of allowing unbounded memory growth.

Streaming remux requires a receiver that accepts this HLS presentation. A
receiver without that support gets another-source/local-playback choices;
there is no silent full-file remux fallback. Initial transfer can seek the
source to a preceding keyframe and preserve its source timeline, but the RAM
window does not offer arbitrary seeking through unproduced or discarded data.
General seeking is disabled for this delivery mode. The playlist receives its
end marker only when the producer finishes.

Preparation buffers 30 seconds of media or reaches the actual end of a shorter
source before starting the receiver. A high-bitrate source may need a larger
buffer setting. Keyframe segments longer than 10 seconds are currently
unsupported; preparation and delivery failures retain explicit local-playback
recovery choices.

H.264/AAC MP4 is the conservative starting profile. Advertised receiver facts
refine compatibility; unknown capability is shown separately from known
incompatibility. An explicit **Try this stream** action can attempt an
unverified format, with the same guarded startup and failure recovery. It does
not bypass malformed-media, encryption or timestamp checks.

Supported delivery paths include finite HTTP media with working byte ranges,
completed files, the selected torrent file, and supported finite HLS
presentations. HLS playlists and their permitted resources are rewritten into
session routes. Live, encrypted and byte-range HLS presentations currently
produce an explicit unsupported-source state. HTTP sources without a usable
stable length/range response also require another source or local playback.

External UTF-8 WebVTT and SRT subtitles can be relayed as WebVTT to capable receivers. Unsupported
or unavailable text tracks show a warning without failing the video. Embedded
subtitle extraction, bitmap subtitle burn-in, dual subtitles, and mpv-specific
appearance/timing features are not provided by external casting. The current
audio selection is retained when its native stream index is available; changing
away from the receiver's default audio can require remuxing. Explicit native
audio-track selection for an existing HLS presentation is currently unsupported.

## Network and platform lifetime

Receivers fetch media from a receiver-route-specific LAN listener with opaque
session URLs. Upstream URLs, headers and local paths stay in Nova. Only the
authorized file/resources are exposed, with bounded readers, GET/HEAD, correct
single byte ranges and Cast CORS headers. Closing the picker does not release
the listener. Shared download/torrent leases protect active source files from
cleanup until the owning casting session ends.

P2P's **No caching** preference means deletion after the final playback reader
releases the torrent. It does not select a RAM-only torrent backend: pieces are
still stored in the existing torrent files during playback. Direct casting
relays those bytes through bounded memory buffers without another full copy;
a required packet-copy remux uses the bounded RAM presentation described above.

Android uses a dedicated non-exported `connectedDevice` foreground service,
with play/pause/stop notification controls and a MediaSession. Discovery owns a
short-lived multicast lock. Relay, preparation, receiver polling and source
ownership run independently of the Activity and local-video PiP. Notification
actions and platform leases carry a session generation so old actions or
cleanup cannot affect a replacement session. The service does not restart
casting after process death.

Both protocols need a usable local network route and receiver access to Nova's
listener. Ethernet and multiple interfaces are supported through route
selection; router isolation, firewall policies, receiver firmware and scoped
IPv6 link-local media URLs can still prevent playback. Discovery success alone
does not prove media delivery is possible. Connection help and retry/local
playback choices remain available.

## Implementation and verification

- `crates/cast`: discovery, Cast/DLNA protocol adapters and receiver ownership.
- `crates/cast-media`: bounded HTTP delivery, finite HLS graph rewriting,
  external WebVTT/SRT conversion routes and media preparation policy.
- `crates/remux`: target-header-compiled FFmpeg C shim, bounded streaming
  packet-copy API and finite-artifact regression helpers.
- `src/app/casting.rs`: destination selection, transfer transactions, progress,
  source leases, recovery and UI projection.
- `crates/ui-shared/cast-bridge.slint`, `crates/ui/casting.slint`: shared device
  picker, controller, accessible focus and themed Cast affordance.
- `src/app/android_cast.rs`, `android/java/dev/misob/nova/NovaCastService.java`:
  Android lifetime, notifications, cancellation and multicast ownership.

Mock receiver, HTTP and packet-identity tests cover the protocol and delivery
contracts. They do not establish compatibility with every television. Physical
Google Cast/DLNA receivers, Android background execution, and Windows firewall
behavior need platform/device verification before interoperability claims.
