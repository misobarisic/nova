# Where Nova stores files

Nova keeps its durable state and rebuildable cache in separate directories. These paths are private to the current user on desktop and private to the app on Android.

## Platform paths

| Platform | Durable data | Rebuildable cache |
| --- | --- | --- |
| Linux | `$XDG_DATA_HOME/nova`; if unset, `~/.local/share/nova` | `$XDG_CACHE_HOME/nova`; if unset, `~/.cache/nova` |
| macOS | `$XDG_DATA_HOME/nova`; if unset, `~/.local/share/nova` | `$XDG_CACHE_HOME/nova`; if unset, `~/.cache/nova` |
| Windows | `%APPDATA%\nova` (normally `%USERPROFILE%\AppData\Roaming\nova`) | `%LOCALAPPDATA%\nova` (normally `%USERPROFILE%\AppData\Local\nova`) |
| Android | App-private files directory returned by Android (`Context.getFilesDir()`) | App-private cache directory returned by Android (`Context.getCacheDir()`) |

On desktop, if the platform environment variables and home-directory fallback are unavailable, Nova falls back to a `nova` directory under the system temporary directory. On Android, `NOVA_DATA_DIR` is a fallback if the app-private files directory has not been initialized. `NOVA_CACHE_DIR` overrides the cache path; otherwise Nova uses Android's cache directory.

On Android, the application ID is `dev.misob.nova`. Android controls the actual private path; it is commonly under `/data/user/0/dev.misob.nova/`, but the app should use the platform-provided directories rather than hard-code that location.

## Files in those directories

| Location | Contents |
| --- | --- |
| `<data>/nova.redb` | Persistent database for settings, app state and local metadata. Metadata cache records use adaptive zstd level 3 compression; user state stays JSON. |
| `<data>/downloads/` | Downloaded episodes managed by Nova's download coordinator. |
| `<cache>/posters/` | Poster image files fetched from the network. |
| `<cache>/torrents/` | Default torrent workspace and DHT state. If a custom torrent directory is configured, torrent data uses that directory instead. |
| Android `<files>/fonts/` | Subtitle font extracted for mpv. |

Episode lists, detail headers, addon manifests and provider mappings live in `nova.redb` on both platforms, so Android counts them under Data rather than Cache. Existing plain metadata migrates on its next cache write; compression does not immediately reclaim previously allocated database space. Poster image files are already encoded images and are not compressed again.

Here, `<data>` and `<cache>` mean the platform's durable data and cache paths in the table above. Nova may recreate cache contents after deletion; deleting `nova.redb` removes locally stored app state. Synced records may be restored after the app syncs again.

## Windows release bundle

The Windows tag release is a ZIP containing `nova.exe`, `libmpv-2.dll`, the MinGW runtime DLLs required by the build, the app license, and third-party source provenance. Extract the files together and run `nova.exe`; keep the DLLs beside the executable. The executable uses the Windows GUI subsystem and opens the app without a console window.
