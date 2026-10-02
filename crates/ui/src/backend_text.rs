//! Localized text the app backend builds itself, owned by `nova-ui` alongside
//! the Slint UI and its translation catalogs.
//!
//! The `@tr("…")` catalogs only reach `.slint` files, so the strings the Rust
//! side formats — download states, sync status, stream/episode hints, relative
//! dates — are translated here. Fixed strings are keyed by their **English
//! source** ([`tr`]), the same convention as the catalogs; anything with a
//! value interpolated into it gets a small named helper so the sentence can be
//! reordered for the target language.
//!
//! The language is remembered by [`set_language`] — called from the app bridge
//! when it applies Settings → Display — so formatting a status string does not
//! re-read the settings blob. Default is English.
use nova_config::Language;
use std::sync::Mutex;

/// What the UI is currently showing. Process-wide (not per-thread like the
/// Slint catalog state): worker threads format status text too.
static LANGUAGE: Mutex<Language> = Mutex::new(Language::English);

/// Remember the language the UI is showing. Called by the app bridge.
pub fn set_language(language: Language) {
    *LANGUAGE.lock().unwrap() = language;
}

/// Is the UI showing Croatian?
fn croatian() -> bool {
    *LANGUAGE.lock().unwrap() == Language::Croatian
}

/// Run `f` with a language selected and restore the previous value. Useful for
/// language-dependent tests; also serializes tests that share this process-wide
/// language state.
pub fn with_language<T>(language: Language, f: impl FnOnce() -> T) -> T {
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let previous = *LANGUAGE.lock().unwrap();
    set_language(language);
    let out = f();
    set_language(previous);
    out
}

/// Croatian noun/adjective form for a count: 1 → singular (`epizoda`), 2–4 →
/// paucal (`epizode`), everything else — including 11–14, which take the
/// genitive plural despite ending in 1–4 — uses the genitive plural
/// (`epizoda`). `one`/`few`/`many` are the three forms.
fn plural(n: u64, one: &'static str, few: &'static str, many: &'static str) -> &'static str {
    let rem100 = n % 100;
    if (11..=14).contains(&rem100) {
        return many;
    }
    match n % 10 {
        1 => one,
        2..=4 => few,
        _ => many,
    }
}

/// Croatian for a fixed backend-built string; unknown keys stay English.
pub fn tr(english: &'static str) -> &'static str {
    if !croatian() {
        return english;
    }
    match english {
        // ---- Downloads -------------------------------------------------
        "Queued" => "Čeka",
        "Preparing…" => "Priprema…",
        "Downloading" => "Preuzimanje",
        "Paused" => "Pauzirano",
        "Downloaded" => "Preuzeto",
        "Download failed" => "Preuzimanje nije uspjelo",
        "Pause download" => "Pauzirajte preuzimanje",
        "Resume download" => "Nastavite preuzimanje",
        "Retry download" => "Pokušajte ponovno",
        "Play downloaded file" => "Reproducirajte preuzetu datoteku",
        "Remove download" => "Uklonite preuzimanje",
        "Play stream" => "Reproducirajte zapis",
        "Download" => "Preuzmite",
        "Downloaded file is missing." => "Preuzeta datoteka nedostaje.",
        "Download worker stopped unexpectedly." => {
            "Radna dretva za preuzimanje neočekivano se zaustavila."
        }
        "This stream cannot be downloaded here." => "Ovaj se zapis ovdje ne može preuzeti.",
        "HLS, DASH, and YouTube streams cannot be downloaded here." => {
            "HLS, DASH i YouTube zapisi ne mogu se preuzeti ovdje."
        }
        "P2P downloads are disabled or unavailable." => {
            "P2P preuzimanja su onemogućena ili nedostupna."
        }
        // ---- Sync ------------------------------------------------------
        "Syncing…" => "Sinkronizacija…",
        "Add a peer to start syncing" => "Dodajte drugi uređaj da sinkronizacija započne",
        "Not synced yet" => "Još nije sinkronizirano",
        "Never connected" => "Nikad se nije povezao",
        "another device" => "drugi uređaj",
        "Android device" => "Android uređaj",
        "Invite ready — share the code; it expires in 15 minutes." => {
            "Pozivnica je spremna — podijelite kod; istječe za 15 minuta."
        }
        "Connecting to the other device…" => "Povezivanje s drugim uređajem…",
        "Camera scanning is available on Android only." => {
            "Skeniranje kamerom dostupno je samo na Androidu."
        }
        // ---- Detail / streams ------------------------------------------
        "No installed addon provides streams for this type." => {
            "Nijedan instalirani dodatak ne nudi zapise za ovu vrstu."
        }
        "No installed addon provides details for this type." => {
            "Nijedan instalirani dodatak ne nudi pojedinosti za ovu vrstu."
        }
        "Loading streams…" => "Učitavanje zapisa…",
        "No streams found." => "Nema pronađenih zapisa.",
        "This stream cannot be played here." => "Ovaj se zapis ovdje ne može reproducirati.",
        "This stream requires the in-app player, which is unavailable." => {
            "Za ovaj zapis potreban je ugrađeni reproduktor, koji nije dostupan."
        }
        "This download is not complete yet." => "Preuzimanje još nije dovršeno.",
        // ---- Catalog / addons ------------------------------------------
        "No results for this selection." => "Nema rezultata za ovaj odabir.",
        "No results for this search." => "Nema rezultata za ovu pretragu.",
        "Type at least 2 characters to search." => "Upišite barem 2 znaka za pretraživanje.",
        "Search movies, shows…" => "Pretražite filmove i serije…",
        "No enabled catalogs support search." => {
            "Nijedan omogućeni katalog ne podržava pretraživanje."
        }
        "Search (e.g. a movie title)" => "Pretražite (npr. naslov filma)",
        "This catalog has no search" => "Ovaj katalog ne podržava pretraživanje",
        "All addons" => "Svi dodaci",
        "All genres" => "Svi žanrovi",
        "This catalog and genre are already added." => "Ovaj katalog i žanr već su dodani.",
        "No addons installed yet — add one in Settings → Addons." => {
            "Još nema instaliranih dodataka — dodajte ga u Postavke → Dodaci."
        }
        "All addons are disabled — enable one in Settings → Addons." => {
            "Svi dodaci su onemogućeni — omogućite neki u Postavkama → Dodaci."
        }
        "Loading configured add-ons…" => "Učitavanje konfiguriranih dodataka…",
        // ---- Playback / P2P --------------------------------------------
        "P2P streaming is disabled in Settings → P2P." => {
            "P2P prijenos je onemogućen u Postavkama → P2P."
        }
        "P2P engine unavailable." => "P2P modul nije dostupan.",
        "P2P engine could not start." => "P2P modul nije se mogao pokrenuti.",
        "Connecting to peers…" => "Povezivanje s drugim čvorovima…",
        // ---- Episode / season badges -----------------------------------
        "✓ Seen" => "✓ Pogledano",
        "▶ Resume" => "▶ Nastavite",
        "Caught up" => "Sve pogledano",
        "Specials" => "Specijali",
        "Episode" => "Epizoda",
        "Resume" => "Nastavite",
        "Connecting…" => "Povezivanje…",
        // ---- Library buckets and watch statuses ------------------------
        // Display only: the stored value stays the English identifier (the
        // filter comparison and `WatchStatus` variants are keyed on it).
        "Plan to Watch" => "Za pogledati",
        "Watching" => "Gledam",
        "Completed" => "Završeno",
        "On Hold" => "Na čekanju",
        "Dropped" => "Odbačeno",
        // ---- Air-date labels -------------------------------------------
        other => other,
    }
}

/// `"Season 3"` / `"Specials"`.
pub fn season_label(season: u32) -> String {
    if season == 0 {
        return tr("Specials").to_string();
    }
    if croatian() {
        format!("Sezona {season}")
    } else {
        format!("Season {season}")
    }
}

/// Episode-badge suffix for episodes that have not aired yet:
/// `" · 2 unaired"`, or `"2 unaired"` on its own in an otherwise empty badge.
pub fn unaired(count: usize, leading_separator: bool) -> String {
    if croatian() {
        let n = count as u64;
        let noun = plural(n, "neemitirana", "neemitirane", "neemitiranih");
        if leading_separator {
            format!(" · {count} {noun}")
        } else {
            format!("{count} {noun}")
        }
    } else if leading_separator {
        format!(" · {count} unaired")
    } else {
        format!("{count} unaired")
    }
}

/// Episode-badge text for a series with episodes still to watch: `"8 left"`.
pub fn left(remaining: usize) -> String {
    if croatian() {
        let n = remaining as u64;
        format!(
            "još {remaining} {}",
            plural(n, "epizoda", "epizode", "epizoda")
        )
    } else {
        format!("{remaining} left")
    }
}

/// Torrent peer count for the player status line: `"12 peers"`.
pub fn peers(count: usize) -> String {
    if croatian() {
        let n = count as u64;
        format!("{count} {}", plural(n, "čvor", "čvora", "čvorova"))
    } else {
        format!("{count} peers")
    }
}

/// Settings hint after a metadata prefetch finished:
/// `"Prefetch done — cached 12 items."`.
pub fn prefetch_done(cached: usize) -> String {
    if croatian() {
        let n = cached as u64;
        format!(
            "Predmemoriranje dovršeno — spremljeno {cached} {}",
            plural(n, "stavka", "stavke", "stavki")
        )
    } else {
        format!("Prefetch done — cached {cached} item(s).")
    }
}

/// Download notification title: `"Downloading 3 files"`.
pub fn downloading_files(pending: usize) -> String {
    if croatian() {
        let n = pending as u64;
        format!(
            "Preuzimanje {pending} {}",
            plural(n, "datoteke", "datoteke", "datoteka")
        )
    } else {
        format!("Downloading {pending} file(s)")
    }
}

/// `"Searching 3 add-ons for streams…"`.
pub fn searching_streams(addons: usize) -> String {
    if croatian() {
        let n = addons as u64;
        format!(
            "Traženje zapisa u {addons} {}…",
            plural(n, "dodatku", "dodatka", "dodataka")
        )
    } else {
        format!("Searching {addons} add-on(s) for streams…")
    }
}

/// `"Loading episode list from 2 add-ons…"`.
pub fn loading_episodes(addons: usize) -> String {
    if croatian() {
        let n = addons as u64;
        format!(
            "Učitavanje popisa epizoda iz {addons} {}…",
            plural(n, "dodatka", "dodatka", "dodataka")
        )
    } else {
        format!("Loading episode list from {addons} add-on(s)…")
    }
}

/// `"12 streams from 3 add-ons."`.
pub fn streams_found(streams: usize, addons: usize) -> String {
    if croatian() {
        let s = streams as u64;
        let a = addons as u64;
        format!(
            "{streams} {} iz {addons} {}.",
            plural(s, "zapis", "zapisa", "zapisa"),
            plural(a, "dodatka", "dodatka", "dodataka")
        )
    } else {
        format!("{streams} stream(s) from {addons} add-on(s).")
    }
}

/// `"4 streams from this add-on."`.
pub fn streams_found_single(streams: usize) -> String {
    if croatian() {
        let s = streams as u64;
        format!(
            "{streams} {} iz ovog dodatka.",
            plural(s, "zapis", "zapisa", "zapisa")
        )
    } else {
        format!("{streams} stream(s) from this add-on.")
    }
}

/// `"4 streams from Torrentio."` (the addon name is data).
pub fn streams_found_from(streams: usize, addon: &str) -> String {
    if croatian() {
        let s = streams as u64;
        format!(
            "{streams} {} iz dodatka {addon}.",
            plural(s, "zapis", "zapisa", "zapisa")
        )
    } else {
        format!("{streams} stream(s) from {addon}.")
    }
}

/// `"Addon unreachable: timeout"` — the error text itself stays as reported.
pub fn addon_unreachable(error: &str) -> String {
    if croatian() {
        format!("Dodatak nije dostupan: {error}")
    } else {
        format!("Addon unreachable: {error}")
    }
}

/// `"Invalid addon manifest: …"`.
pub fn invalid_manifest(error: &str) -> String {
    if croatian() {
        format!("Neispravan manifest dodatka: {error}")
    } else {
        format!("Invalid addon manifest: {error}")
    }
}

/// `"Install failed (UI thread panicked: …)"`.
pub fn install_failed(detail: &str) -> String {
    if croatian() {
        format!("Instalacija nije uspjela (UI dretva se srušila: {detail})")
    } else {
        format!("Install failed (UI thread panicked: {detail})")
    }
}

/// `"X provides streams but no browsable catalogs — …"`.
pub fn no_browsable_catalogs(base: &str) -> String {
    if croatian() {
        format!(
            "{base} nudi zapise, ali nema kataloga za pregledavanje — dodajte dodatak s katalozima (npr. Cinemeta) za pregled."
        )
    } else {
        format!(
            "{base} provides streams but no browsable catalogs — add an addon with catalogs (e.g. Cinemeta) to browse."
        )
    }
}

/// `"Paired with Kitchen"`.
pub fn paired_with(name: &str) -> String {
    if croatian() {
        format!("Upareno s uređajem {name}")
    } else {
        format!("Paired with {name}")
    }
}

/// `"Removed from Kitchen's sync"`.
pub fn removed_from_sync(name: &str) -> String {
    if croatian() {
        format!("Uklonjeno iz sinkronizacije uređaja {name}")
    } else {
        format!("Removed from {name}'s sync")
    }
}

/// `"Could not create invite: …"`.
pub fn could_not_create_invite(error: &str) -> String {
    if croatian() {
        format!("Nije moguće izraditi pozivnicu: {error}")
    } else {
        format!("Could not create invite: {error}")
    }
}

/// `"Invalid invite: …"`.
pub fn invalid_invite(error: &str) -> String {
    if croatian() {
        format!("Neispravna pozivnica: {error}")
    } else {
        format!("Invalid invite: {error}")
    }
}

/// `"Last sync failed: …"`.
pub fn last_sync_failed(error: &str) -> String {
    if croatian() {
        format!("Posljednja sinkronizacija nije uspjela: {error}")
    } else {
        format!("Last sync failed: {error}")
    }
}

/// `"Last synced 5m ago"`.
pub fn last_synced(ago: &str) -> String {
    if croatian() {
        format!("Posljednja sinkronizacija: {ago}")
    } else {
        format!("Last synced {ago}")
    }
}

/// `"Last connected 2h ago"`.
pub fn last_connected(ago: &str) -> String {
    if croatian() {
        format!("Posljednja veza: {ago}")
    } else {
        format!("Last connected {ago}")
    }
}

/// `"Playing Show S1 E2"` (the display name is data).
pub fn playing(name: &str) -> String {
    if croatian() {
        format!("Reprodukcija: {name}")
    } else {
        format!("Playing {name}")
    }
}

/// `"P2P unavailable: …"`.
pub fn p2p_unavailable(error: &str) -> String {
    if croatian() {
        format!("P2P nije dostupan: {error}")
    } else {
        format!("P2P unavailable: {error}")
    }
}

/// Sync status "last seen" stamp: `"45s ago"` / `"3m ago"` / `"2h ago"`.
pub fn ago(secs: u64) -> String {
    if secs < 60 {
        if croatian() {
            format!("prije {secs} s")
        } else {
            format!("{secs}s ago")
        }
    } else if secs < 3600 {
        let mins = secs / 60;
        if croatian() {
            format!("prije {mins} min")
        } else {
            format!("{mins}m ago")
        }
    } else {
        let hours = secs / 3600;
        if croatian() {
            format!("prije {hours} h")
        } else {
            format!("{hours}h ago")
        }
    }
}

pub fn peer_retry(secs: u64) -> String {
    if croatian() {
        if secs == 0 {
            "Posljednji pokušaj nije uspio".to_string()
        } else {
            format!("Pokušaj nije uspio · novi za {secs} s")
        }
    } else if secs == 0 {
        "Last attempt failed".to_string()
    } else {
        format!("Attempt failed · retry in {secs}s")
    }
}

/// `"128.4 MB · 1,203 files"` (units are international, the count is not).
pub fn files_label(files: usize) -> String {
    if croatian() {
        let n = files as u64;
        format!(
            "{} {}",
            grouped_count(files),
            plural(n, "datoteka", "datoteke", "datoteka")
        )
    } else if files == 1 {
        "1 file".to_string()
    } else {
        format!("{} files", grouped_count(files))
    }
}

/// Thousands-grouped count (`1203` → `"1,203"`) for compact UI readouts.
pub fn grouped_count(n: usize) -> String {
    let digits: Vec<char> = n.to_string().chars().collect();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.iter().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*ch);
    }
    out
}

/// Air-date stamp for an episode: `"3 days ago"`, `"in 2 weeks"`, `"today"`.
/// `diff` is days since the air date (negative = in the future).
pub fn relative_days(diff: i64) -> String {
    if !croatian() {
        return match diff {
            0 => "today".to_string(),
            1 => "yesterday".to_string(),
            2..=6 => format!("{diff} days ago"),
            7..=13 => "last week".to_string(),
            14..=59 => format!("{} weeks ago", diff / 7),
            60..=364 => {
                let months = diff / 30;
                format!("{months} month{} ago", if months == 1 { "" } else { "s" })
            }
            365..=729 => "last year".to_string(),
            d if d >= 730 => {
                let years = d / 365;
                format!("{years} year{} ago", if years == 1 { "" } else { "s" })
            }
            _ => {
                let ahead = -diff;
                if ahead < 7 {
                    format!("in {ahead} days")
                } else if ahead < 60 {
                    let weeks = ahead as u64 / 7;
                    format!("in {weeks} week{}", if weeks == 1 { "" } else { "s" })
                } else {
                    let months = ahead as u64 / 30;
                    format!("in {months} month{}", if months == 1 { "" } else { "s" })
                }
            }
        };
    }
    match diff {
        0 => "danas".to_string(),
        1 => "jučer".to_string(),
        2..=6 => format!("prije {diff} dana"),
        7..=13 => "prošli tjedan".to_string(),
        14..=59 => {
            let w = (diff / 7) as u64;
            format!("prije {w} {}", plural(w, "tjedan", "tjedna", "tjedana"))
        }
        60..=364 => {
            let m = (diff / 30) as u64;
            format!("prije {m} {}", plural(m, "mjesec", "mjeseca", "mjeseci"))
        }
        365..=729 => "prošle godine".to_string(),
        d if d >= 730 => {
            let y = (d / 365) as u64;
            format!("prije {y} {}", plural(y, "godinu", "godine", "godina"))
        }
        _ => {
            let ahead = (-diff) as u64;
            if ahead < 7 {
                format!("za {ahead} {}", plural(ahead, "dan", "dana", "dana"))
            } else if ahead < 60 {
                let w = ahead / 7;
                format!("za {w} {}", plural(w, "tjedan", "tjedna", "tjedana"))
            } else {
                let m = ahead / 30;
                format!("za {m} {}", plural(m, "mjesec", "mjeseca", "mjeseci"))
            }
        }
    }
}

/// Absolute air date: `"Mar 15, 2024"` / `"15. ožu 2024."`.
pub fn absolute_date(day: u32, month: u32, year: i64) -> String {
    const EN: &[&str] = &[
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    const HR: &[&str] = &[
        "sij", "velj", "ožu", "tra", "svi", "lip", "srp", "kol", "ruj", "lis", "stu", "pro",
    ];
    let index = (month.clamp(1, 12) - 1) as usize;
    if croatian() {
        format!("{day}. {} {year}.", HR[index])
    } else {
        format!("{} {day}, {year}", EN[index])
    }
}

/// Full month name for the Home → Upcoming calendar title
/// (`cal_month_title` below): `"February"` / `"veljača"`.
pub fn month_name(month: u32) -> &'static str {
    const EN: &[&str] = &[
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    const HR: &[&str] = &[
        "siječanj",
        "veljača",
        "ožujak",
        "travanj",
        "svibanj",
        "lipanj",
        "srpanj",
        "kolovoz",
        "rujan",
        "listopad",
        "studeni",
        "prosinac",
    ];
    let index = (month.clamp(1, 12) - 1) as usize;
    if croatian() { HR[index] } else { EN[index] }
}

/// Upcoming calendar month title: `"February 2026"` / `"veljača 2026"`.
pub fn cal_month_title(month: u32, year: i64) -> String {
    format!("{} {year}", month_name(month))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn croatian_translates_and_english_is_the_source() {
        with_language(Language::English, || {
            assert_eq!(tr("Queued"), "Queued");
            assert_eq!(downloading_files(3), "Downloading 3 file(s)");
            assert_eq!(season_label(2), "Season 2");
            assert_eq!(files_label(1), "1 file");
            assert_eq!(relative_days(3), "3 days ago");
            assert_eq!(absolute_date(15, 3, 2024), "Mar 15, 2024");
        });
        with_language(Language::Croatian, || {
            assert_eq!(tr("Queued"), "Čeka");
            assert_eq!(tr("Downloaded"), "Preuzeto");
            assert_eq!(tr("Plan to Watch"), "Za pogledati");
            assert_eq!(season_label(0), "Specijali");
            assert_eq!(season_label(2), "Sezona 2");
            // An unknown key stays as written instead of vanishing.
            assert_eq!(tr("Not in the table"), "Not in the table");
        });
    }

    #[test]
    fn croatian_counts_agree_with_the_number() {
        with_language(Language::Croatian, || {
            // 1 → singular, 2-4 → paucal, 5+ (and 11-14) → genitive plural.
            assert_eq!(files_label(1).split(' ').nth(1).unwrap(), "datoteka");
            assert_eq!(tr("Completed"), "Završeno");
            assert_eq!(relative_days(1), "jučer");
            assert_eq!(relative_days(3), "prije 3 dana");
            assert_eq!(relative_days(8), "prošli tjedan");
            assert_eq!(relative_days(21), "prije 3 tjedna");
            assert_eq!(relative_days(90), "prije 3 mjeseca");
            assert_eq!(relative_days(200), "prije 6 mjeseci");
            assert_eq!(relative_days(300), "prije 10 mjeseci");
            assert_eq!(relative_days(400), "prošle godine");
            assert_eq!(relative_days(800), "prije 2 godine");
            assert_eq!(relative_days(-3), "za 3 dana");
            assert_eq!(absolute_date(15, 3, 2024), "15. ožu 2024.");
        });
    }

    #[test]
    fn calendar_month_titles_use_full_names() {
        with_language(Language::English, || {
            assert_eq!(cal_month_title(2, 2026), "February 2026");
            assert_eq!(cal_month_title(13, 2026), "December 2026");
        });
        with_language(Language::Croatian, || {
            assert_eq!(cal_month_title(2, 2026), "veljača 2026");
        });
    }
}
