# Smart downloads

Detail → Keep offline creates a personal show rule. Downloads → Smart downloads
edits that profile's rules and this device's execution settings. Rules sync;
execution is disabled by default on each device. The device profile list can
explicitly enable additional profiles without selecting their UI sessions.
Import restores intent and atomically disables execution for affected profiles.

A new rule keeps three released, unwatched regular episodes, with an unfinished
resume target first and then the established season/episode order. The count is
adjustable from one to ten. Specials are optional; unknown and future release
dates never qualify. Source selection is explicit: open an episode's stream
menu and choose **Use this source for smart downloads**. Subsequent sources must
match the addon, release family and recorded quality facts; arbitrary ranked
fallback is excluded. Existing matching manual and shared jobs occupy slots.
User pauses remain paused. Failed slots have bounded retries and explicit Retry
and Choose source actions.

The device allowance starts at 5 GiB, with a free-space reserve of the greater
of 1 GiB and 5% of the actual download filesystem. The allowance counts physical
automatic files, partial files and outstanding reservations once per physical
artifact/bundle. Manually kept storage is reported separately. Every admission
is serialized with manifest mutation in the existing coordinator, which retains
one active transfer and gives queued manual jobs priority.

Provider selected-file byte hints are estimates. A bounded HTTP HEAD probe can
supply missing single-resource totals; a playlist's byte length is never treated
as its media size. Unknown totals wait for manual download. Known estimates
reserve a 5% margin plus 16 MiB for bundle/metadata overhead, with a hard cutoff.
HTTP and HLS check admission before each body write. Torrent sessions use a
filesystem-storage middleware for every ordinary and vectored piece write,
including shared/already-managed bundles. The middleware serializes admission
and write, accounts allocated Unix blocks (conservative logical sizes elsewhere),
and reserves incoming block rounding plus metadata overhead. Data writes cannot
cross the admitted quota; oversized estimates/piece boundaries pause with partial
data retained. Filesystem metadata and bounded playlist/resume bookkeeping use
the reserved overhead. No estimated total is labeled exact.

Automatic work needs a validated, unmetered Wi-Fi or Ethernet connection;
charging is optional. Android observes default-network/cost and battery changes.
Desktop Linux checks the actual IPv4/IPv6 Internet routes and requires every
routed interface to have allowed NetworkManager link/metering information;
Windows uses the OS connection-cost API through PowerShell. Missing cost information blocks
unattended work. Network, charging and device-policy pauses resume when their
conditions recover; user pauses remain paused. Budget, free-space and changed-source
pauses require **Retry / Restore offline** after adjusting storage, the allowance
or the selected source. Source/settings revisions and profile retirement are
revalidated after lookup and before media writes.

Cleanup removes only automatic media no longer needed by a policy. Another
profile's reference, a manual keep or active downloaded playback protects the
artifact. Per-rule watched retention keeps the policy's completed files.
Removing a rule offers keeping completed files (the default, promoting them to
manual keep) or removing unneeded files. Unfinished automatic work loses that
rule reference and stops if no other owner needs it. A manual download removal suppresses the same
policy/episode until its frontier changes or Restore offline is chosen.

Desktop execution lasts while Nova runs. Android's persisted, constrained
`NovaOfflineJobService` performs bounded metadata refresh, planning and transfers
without an Activity, Slint, Bridge or foreground-service start. It initializes
paths/storage before reads, materializes merged profile records, and reuses the
same process coordinator when one exists. A headless-only coordinator starts
only automatic jobs. `onStopJob` and the 90-second window cancel/checkpoint
workers; later OS windows resume opportunistically. The UI says **Pending
background opportunity** rather than promising a completion time.

Personal intent is an object snapshot at `offline_policies:v1`, decomposed into
`offline_policies` records by JSON typed-title key `[media_type,title_id]`. Device
settings, enabled profile IDs, suppression and retry/freshness receipts are at
`smart_downloads:device:v1`. Manifest `DownloadJob.offline` holds manual keep,
policy owner references, source provenance/revision, reservation, machine pause
and retry state. Legacy jobs default to manual keep. Deferred jobs reconstruct a
fresh provider selection from the typed personal rule; transient resolver tokens
and signed capabilities are never restored as authority after process death.

Automated checks cover admission/restart ownership, network/charging, bounded
retry, HTTP pre-write cancellation and concurrent torrent quota writes. Android
OS/Doze/process-death execution and Windows connection-cost behavior still need
native device validation; no Android build or APK was produced for this work.
