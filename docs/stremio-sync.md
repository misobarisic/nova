# Stremio account sync

Settings → Integrations connects one Stremio account through a browser/code flow.
Nova verifies the account before showing the initial library merge preview;
Start begins the two-way merge. Settings offers **Full library sync** and
**Selected titles**; existing connections retain full-library mode until an
explicit choice changes it. The choice belongs to the Stremio account and
survives reconnecting, including reconnecting after another account. A new
account keeps the full-library default and still requires the initial Start
action. Stremio addons remain managed in Stremio.
Connecting an account does not install addons in Nova.

After Start is saved, the initial preview becomes Sync now even if the first
pass needs a retry. The status identifies library reads, episode metadata,
local saves, uploads and upload confirmation. Failures retain that phase and
distinguish request timeouts, numeric HTTP/API rejections and local save failures.
Server response text and credentials are not included in statuses or diagnostics.

The initial merge includes saved library titles, watched episodes and resume
positions from both sides. Positive membership and watched history are combined
on first linking. Later changes use acknowledged baselines and field-specific
action times, so adding a title does not give all its old episode progress a new
clock. Explicit removals and unwatches can propagate after the initial merge;
equal-time conflicts favor Stremio. Unknown Stremio library fields survive
updates.

Stremio's empty watched-bitfield sentinel remains valid after episode metadata
becomes available; it represents no watched episodes and does not block resume
import. A missing anchor with nonempty watched history remains pending rather
than discarding unknown history.

The worker runs on startup and while Nova is in the foreground, polling about
every 60 seconds. Local membership and watched changes wake it after a short
five-second debounce. Playback positions are checkpointed at a 30-second
cadence and at playback stop. Pending work and acknowledged baselines persist
locally, so an offline request can be retried after restart.

Reconciliation and result preparation run on the Stremio worker. The event loop
only checks that the native snapshot and sync clock are still current, applies
changed records, and queues their save without waiting for a busy store. The
ordered persistence worker performs serialization, history/activity derivation,
and database writes under the external-origin guards. Delivery is acknowledged
only after its background save succeeds. Unchanged projections do not save or
refresh models. UI updates coalesce per changed pass; unrelated Detail pages are
left alone, and hidden Home/Library models refresh when revealed.

## Selected titles and exclusions

In Selected titles mode, only explicitly enabled native titles are imported or
exported. Cached canonical imports, unrelated native bindings and old pending
uploads do not gain permission merely because they were previously synchronized.
The account's other library titles remain untouched.

Open a saved title's **Tracking → Stremio** page, choose an account-catalog
match and review its episode alignment. Catalog searches use only the connected
account's declared searchable addons; a catalog ID can also be entered directly.
Exact episode IDs and current confirmed aliases prefill matches; other episode
pairs require explicit selection. Duplicate target assignments cannot be enabled.
A movie needs one title match. Choosing a result alone does not upload anything.

**Enable Stremio for this title** saves the reviewed native/remote mapping.
In Selected titles mode, **Include watched episodes** defaults off; existing
history is excluded from new native upload authority unless enabled. Remote
history remains independently observed. Full-library mode authorizes existing
history. **Stop tracking this title** stores a per-title exclusion that takes
precedence in either mode, preserving existing local and remote history. Its
canonical coordination row cannot bypass that exclusion.

Selected mappings retain their approved episode IDs when additional seasons or
episodes appear. New inventory does not extend approval automatically; missing
or replaced approved episode IDs require review. Native metadata is never
replaced by the selected account catalog's inventory.

Mode choices and selections synchronize with paired devices within the Nova
profile. Mode changes and per-title consent are reread at delivery boundaries,
including immediately before local materialization and remote upload. A peer's
exclusion or mode change cancels stale work. Control mailboxes retain the latest
mode/enrolment choice even when ordinary request queues fill during HTTP.

## Identity and episode coverage

Library identity uses media type plus the exact catalog ID. Source-specific
Nova titles can use current confirmed metadata aliases; matching title text or
season/episode numbers alone never establishes a connection. Ambiguous aliases
and unsupported identities remain pending rather than uploading guessed
progress.

Existing Nova entries have independent bindings to a remote title. A canonical
card and multiple source seasons can therefore share Stremio's title without
discarding each other's mappings or baselines. Each binding maps only proven
episodes and owns its initial history union. Membership remains a shared
title-level action, while watched episodes retain independent field clocks.
When confirmed native sources already represent an imported series, the account's
canonical row coordinates the merge without creating another saved title or
copying history to a second identity. This check includes current confirmed
catalog aliases even when the account also accepts the saved source's native ID;
the preferred export ID does not limit which proven representations it covers.
Conflicting parents within the same catalog namespace and stale aliases do not
establish that relationship. An explicitly existing canonical title is
retained. Continue Watching and New episodes also collapse existing duplicate
cards only when their selected episode shares a current confirmed catalog ID;
all saved entries and history remain intact, and different episodes/seasons or
movies are never combined by title text.

Series watched bitfields require the complete ordered metadata video list.
`stremio_metadata.rs` fetches metadata only from descriptors already installed
on the connected Stremio account, respecting their declared resource types and
ID prefixes. Requests run serially within a 20-second total budget, visit at
most eight accepting addons, and limit response bodies to 2 MiB. Account
credentials are not attached to addon requests. Returned metadata must have the
requested type and ID, and unique episode IDs.

When an existing Nova entry lacks current catalog aliases, the worker also
refreshes its exact native metadata from enabled, available Nova sources. It
uses the ordinary metadata broker to establish confirmed aliases, with one
20-second fetch/enrichment budget and at most eight accepting sources. Failed
native refreshes cool down for five minutes; Sync now bypasses that cooldown.
The account's descriptors remain the authority for decoding the remote bitmap.
The pending list includes each unresolved entry's reason and ordinary native
type/ID. Configured URLs and query-bearing identifiers are omitted.

The account's episode inventory decodes its watched bitmap. Nova's existing
episode inventory stays intact, including when both sides share an IMDb series
ID but use different episode IDs. Exact episode IDs or current confirmed aliases
map progress onto existing Nova episodes; series identity alone does not prove
episode identity. Mapping snapshots refresh when native episode IDs/aliases or
the adapter version change, so previous snapshots do not delay these corrections
until the normal six-hour metadata refresh.

A confirmed mapping may cover just one native season of a larger remote series.
The adapter retains the complete remote video inventory and maps only confirmed
native episodes. Unmapped remote watched bits remain intact. Missing metadata
or an unusable watched-bitfield anchor leaves progress pending. Library
membership can still be imported while series progress waits for metadata.
Movies can resolve their title/resume identity without a metadata request.

## Persistence and paired devices

The account connection is shared through Nova's existing device sync, along
with source/title action state. A paired device verifies the shared credentials
before using them. Disconnect publishes a connection tombstone. Stremio applies
run under external-origin guards so downloaded history does not become a new
local action and echo back to the service.

The Stremio `authKey` is stored as plaintext JSON in Nova's database, like the
existing tracker credentials, and travels to paired devices through encrypted
Nova sync. The `stremio` sync domain contains a version-1 `connection` record with the
account, credential and an explicit sign-in epoch. Only successful browser
sign-in replaces this record; disconnect tombstones it. Session choices use
separate records keyed by that epoch: `session:<JSON(epoch)>:automatic` is a
boolean LWW preference, while `session:<JSON(epoch)>:started` and
`session:<JSON(epoch)>:bootstrap` are monotonic true-only markers. A stale device
changing automatic sync cannot restart the initial merge or revive a replaced
sign-in session. Native actions use
`event:<JSON(account,type,native ID)>:<field>`; merged remote-title state uses
`title:<JSON(account,remote ID)>:<field>`. Each field is an independent record:
`saved`, `resume`, or `watched:<JSON episode ID>`. Separate field records preserve
concurrent changes to different episodes across paired devices. The independent
`account:<JSON account>:mode` record stores `full_library` or `selected_titles`.
Existing active `session:<JSON(epoch)>:mode` values migrate before credentials
are replaced or disconnected, and account preferences take precedence over
late legacy session updates. Reconnecting the same account keeps its mode;
credential epochs still invalidate old requests and preference commands.
Account-bound `selection:<JSON account>:<JSON(native type,native ID)>` records
store explicit enable/exclusion, the reviewed episode pairs, inventory snapshots,
history cutoff and consent revision. `excluded_remote:<JSON(account,type,remote
ID)>:<JSON(native type,native ID)>` is an atomic exclusion index that also gates
canonical imports. Local title snapshots default their optional `selection`
field for legacy data.
`title:<JSON(account,remote ID)>:remote` stores the acknowledged server
`_mtime` milliseconds and canonical per-field clocks as a JSON tuple. Paired
devices reuse those original action times only when the server revision and
field values still match, so a successful export does not become a newer action
on another device.

Device-local `stremio:state:v1:<JSON account>:<JSON remote ID>` records retain
the raw title, mapping snapshot, reconciliation baselines, desired result and
pending delivery marker. Optional `accepted_upload` records a fingerprint of an
accepted PUT even if the issuing profile was switched while HTTP was blocked;
delivery remains pending until verification. Already-issued verification also
checkpoints its original durable owner, while cancellation prevents fresh API
requests. A defaulted per-title `progress_merged` marker defers
initial watched-history union until compatible metadata becomes available,
independently of the account's membership bootstrap. These local rows are not
a second shared credential store.

Canonical local rows keep the JSON remote-ID suffix. Native bindings use a JSON
`(remote ID, native type, native ID)` tuple suffix under the same prefix and a
defaulted `binding` field in the row. Older single-source rows migrate atomically
to binding rows, preserving pending work and baselines before canonical rows can
reuse their old key. Shared field records remain keyed by remote title/episode.

## Where to look

- `crates/stremio/src/api.rs`: account/code flow and bounded official API calls.
- `crates/stremio/src/library.rs`: raw library items, watched bitfields and progress projection.
- `crates/stremio/src/lib.rs`: pure timestamped field reconciliation.
- `src/app/stremio.rs`: account actor, batched UI publication and persisted work.
- `src/app/stremio_selection.rs`: mode policy, explicit title review and durable authorization; `stremio_selection_tests.rs` covers stale consent, exclusions and preserved season coverage.
- `src/app/stremio_apply.rs`: background materialization, optimistic UI acceptance and durable receipts.
- `src/app/persistence.rs`: ordered local snapshots and external record patches.
- `src/app/stremio_events.rs`: atomic native action clocks and origin guards.
- `src/app/stremio_metadata.rs`: descriptor routing and confirmed episode mapping.
- `src/app/stremio{_tests,_ingress_tests,_session_tests}.rs`: reconciliation, durable ingress and shared-session regressions.
- `crates/ui/stremio.slint`: Settings submenu.
