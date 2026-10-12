# Supernova

Original Nova provider code, licensed under the workspace GPL-3.0-or-later
license. The bounded Nyaa and AnimeTosho fetching/parsing adapters are adapted from
Comet (GPL-3.0), maintained by its contributors. No Comet source catalog or
server is bundled.

Reference: https://github.com/g0ldyy/comet, commit
`f56677f05ed9a158f9a0338ad2f44d8efde35969`,
`comet/scrapers/nyaa.py` and `comet/scrapers/animetosho.py`.
Upstream license: https://github.com/g0ldyy/comet/blob/f56677f05ed9a158f9a0338ad2f44d8efde35969/LICENSE.

Supernova contacts only sources explicitly selected by the user. The Nyaa
adapter uses public HTML; AnimeTosho's documented Torznab API supplies XML
(`https://animetosho.org/about`), with the main-site JSON feed as a fallback.
Both adapters fetch at most two pages per bounded title query. APIBay supplies a public movie/TV JSON index, with iTorrents
metainfo to validate file identities. No access-control bypass is implemented.
Source availability and permissions vary; enable only sources you may use.

TorBox protocol references: https://api-docs.torbox.app/ and the official
TorBox JavaScript SDK. Account changes occur only on selection and always use
`add_only_if_cached=true` and `seed=3`. Search never creates a transfer.

The TorBox key is an ordinary masked, persisted, synced plugin preference.
TorBox requires the key as an internal `requestdl` query parameter; Nova never
displays or persists that request URL. Resolver tokens and discovery caches
contain neither keys nor expiring playback URLs.
Credential-bearing CDN links are allowed only at the final resolver boundary
under the declared HTTPS CDN/path/token policy. They remain in memory for the
in-app player; Nova does not save, sync, display, or hand them to external players.

Supernova currently explicitly opts into all HTTPS domains (`domains: ["*"]`)
for HTTP and final ephemeral playback. Other providers retain their declared
scopes. Nova's host still validates and pins public DNS addresses and rejects
private/reserved IPs, URL credentials, non-HTTPS requests, and cross-origin
redirects on authenticated or POST requests.
TorBox's documented service-domain references are:
https://support.torbox.app/en/articles/13181686-accessing-torbox-with-nextdns
and https://support.torbox.app/en/articles/12667085-torbox-is-being-blocked-by-my-anti-virus-software.
The credential exception additionally requires HTTPS on port 443, `/dld/`,
exactly one matching `token` query parameter, and no credential elsewhere.
