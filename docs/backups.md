# Backup and import

Open **Settings → Data → Backup and import**. Choose profiles and independent
components, then export a `.nova-backup` file using the system document picker.
Everything personal includes selected credentials, configured addon URLs and
profile PINs in plaintext. Addon package binaries and compatible device
preferences are separate, unchecked options. Downloads, device sync identity,
paired peers, notification receipts and running service jobs are excluded.

To import, choose a file, map each source profile to a new or existing profile,
select components and build the preview. New is the default target, including
for the common `default` source ID. Two sources cannot share one target. A
retired profile ID cannot be restored. Existing target names and the selected
profile remain unchanged. Protected targets use their ordinary management PIN.

Merge inserts missing records and keeps conflicting local values. Enable **Use
backup for conflicts** for a component to choose its archived conflicts.
Replace changes only selected components and deletes records missing from those
components. Empty and omitted components are different. The preview reports
archive/new/identical/conflicting/updated/preserved/deleted records and examples
of conflicting identities. Records include whole order/cursor registers where
those values are atomic. Library title identity includes media type; categories
match their exact stored name. Unknown categories are omitted from imported
assignments and reported. Missing addon packages are reported as requiring
reinstallation; included packages use the ordinary validated package pipeline.

Personal settings, categories, smart collections, and each independent hidden
rail have separate selectors. A settings import therefore cannot overwrite
unselected collections or local filesystem paths. Integrations export durable
configuration and shared account records, including refresh credentials, but
never archived delivery jobs. Import rebuilds tracking materialization and
clears target service reconciliation checkpoints. Release subscriptions and
smart offline policies remain portable while device delivery/download
activation is disabled in the same import transaction. Queue import does not
start a queue run or playback.

Title playback preferences also include the remembered release/source family.
The existing hashed per-title identities and original selection times are
retained; expired or future-clock hints are omitted. The profile snapshot
`stream_family_preferences:v1` and sync domain `stream_family_preferences` mirror
legacy `series-stream-v1.json` files. Cache flushes compare the durable revision
before writing, so a pre-import queued flush cannot overwrite imported hints.

An export captures one consistent database read after accepted writes have
flushed. The archive is ZIP/UTF-8 JSON: `manifest.json`, one versioned `records`
payload per profile/component, and optional `packages/<sha256>.zip` files.
Each declared file has a SHA-256 checksum. Duplicate entries/record identities,
invalid relative paths, missing files, invalid versions, inconsistent counts,
invalid packages and expanded size over 128 MiB are rejected before writes.
Each JSON component is limited to 32 MiB and 200,000 records. Unsupported
optional component schemas are reported and remain unselected.

Preview is bound to the archive checksum and target storage/sync revisions.
Changes during review require a new preview. Affected active services are
checkpointed and quiesced before commit. The record-owner mutex is acquired
before the profile writer mutex; the storage writer is acquired last. Reminder
delivery and offline device mutation guards precede all three during commit so
an old background read-modify-write cannot restore imported device activation.
Import mutates a clone of the sync Store and commits profile creation, selected
snapshots, new local record versions/tombstones, materialized baselines,
recovery data and a publication receipt in one redb batch. The live Store is
replaced only after the commit succeeds. Failed imports cannot leak tentative
values through a later unrelated save. Package files are staged first and are
immutable; unused staged files never create imported membership.

The last three local recovery copies are retained. **Undo last import** builds
a fresh review: fields still equal to the imported result revert, while later
local/peer edits and unrelated new records remain. A component's Use backup
choice can explicitly override those conflicts. Undo creates compensating edits
at new sync versions; it cannot undo a completed external API operation. It
leaves newly created profile identities available rather than retiring them.
**Delete recovery copies** removes the local copies. Recovery copies are local
plaintext data, like the exports.

Desktop exports fsync a temporary file before atomic replacement (including
Windows replacement of an existing destination). Android uses framework SAF
`ACTION_OPEN_DOCUMENT`/`ACTION_CREATE_DOCUMENT`, streams content URIs on a
worker and reports provider cancellation/failure. Provider finalization is not
a database transaction. Android process death during a picker requires a new
picker request; a committed import resumes publication from its durable
receipt. Real provider/device behavior needs Android device validation.

Implementation: `src/app/backup/{archive,adapters,ui,mod}.rs`,
`src/app/documents.rs`, `crates/ui/backup.slint`, and the shared `BackupBridge`.
The sync snapshot decomposition helper accepts several components before one
save. Tests live beside the backend and in `tests/navigation/backup.rs`.
