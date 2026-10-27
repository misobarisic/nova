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

/// Approximate playable-ahead duration, not a bandwidth guarantee.
pub fn party_buffered_status(status: &str, seconds: f64) -> String {
    let seconds = if seconds.is_finite() {
        seconds.max(0.0)
    } else {
        0.0
    };
    if croatian() {
        format!("{} · {seconds:.1} s učitano", tr(status))
    } else {
        format!("{} · {seconds:.1} s buffered", tr(status))
    }
}

pub fn screenshot_saved(path: &str) -> String {
    if croatian() {
        format!("Snimka zaslona spremljena: {path}")
    } else {
        format!("Screenshot saved: {path}")
    }
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
pub fn tr(english: &str) -> &str {
    if !croatian() {
        return english;
    }
    match english {
        "Check resume position" => "Provjeri položaj nastavka",
        "Loading playback history…" => "Učitavanje povijesti gledanja…",
        "No recorded playback date" => "Nema zabilježenog datuma gledanja",
        "No synopsis is available for the last completed episode." => {
            "Sažetak posljednje dovršene epizode nije dostupan."
        }
        "A completed episode could not be confirmed from playback history." => {
            "Iz povijesti reprodukcije nije moguće potvrditi dovršenu epizodu."
        }
        "This version is too short to replay an ending safely." => {
            "Ova verzija je prekratka za sigurno ponovno gledanje završetka."
        }
        "This download does not match the episode selected for replay." => {
            "Ovo preuzimanje ne odgovara epizodi odabranoj za ponovno gledanje."
        }
        "Ending replays require an in-app source." => {
            "Za ponovno gledanje završetka potreban je izvor unutar aplikacije."
        }
        "Casting reached its buffer limit. Increase Casting remux buffer in Player settings, choose another source or play here." => {
            "Emitiranje je dosegnulo ograničenje međuspremnika. Povećajte Međuspremnik za remuksiranje pri emitiranju u postavkama reproduktora, odaberite drugi izvor ili reproducirajte ovdje."
        }
        "This receiver cannot accept this streaming format. Choose another source or play here." => {
            "Ovaj prijamnik ne podržava ovaj format emitiranja. Odaberite drugi izvor ili reproducirajte ovdje."
        }
        "This receiver does not support the source codecs. Choose another source or play here." => {
            "Ovaj prijamnik ne podržava kodeke izvora. Odaberite drugi izvor ili reproducirajte ovdje."
        }
        "Changing receiver…" => "Promjena prijamnika…",
        "Could not confirm receiver ownership. Retry the connection before changing device." => {
            "Nije potvrđeno vlasništvo nad reprodukcijom na prijamniku. Prije promjene uređaja pokušajte ponovno povezivanje."
        }
        "Sleep timer finished. Receiver stop could not be confirmed." => {
            "Mjerač vremena za spavanje je istekao. Nije potvrđeno zaustavljanje prijamnika."
        }
        "Playback position changed while preparing. Try casting again." => {
            "Položaj reprodukcije promijenio se tijekom pripreme. Ponovno pokušajte emitiranje."
        }
        "The receiver may still be playing. Resume local playback anyway?" => {
            "Prijamnik možda još reproducira. Ipak nastaviti lokalnu reprodukciju?"
        }
        "Resume locally anyway" => "Ipak nastavi lokalno",
        "Retry Stop casting" => "Pokušaj ponovno zaustaviti emitiranje",
        "Volume down" => "Smanji glasnoću",
        "Volume up" => "Povećaj glasnoću",
        "This source cannot resume on the receiver. Play locally or choose another source." => {
            "Ovaj izvor nije moguće nastaviti na prijamniku. Reproducirajte lokalno ili odaberite drugi izvor."
        }
        "Receiver did not confirm the resume position. Local playback was retained." => {
            "Prijamnik nije potvrdio položaj nastavka. Lokalna reprodukcija je zadržana."
        }
        "Selected embedded subtitles cannot be cast. Use external text subtitles or play locally." => {
            "Odabrane ugrađene titlove nije moguće emitirati. Koristite vanjske tekstualne titlove ili reproducirajte lokalno."
        }
        "This receiver cannot display the selected subtitles. Play locally to preserve them." => {
            "Ovaj prijamnik ne može prikazati odabrane titlove. Za njihovo zadržavanje reproducirajte lokalno."
        }
        "Some external subtitles are unavailable on this receiver. Video can continue or you can play locally." => {
            "Neki vanjski titlovi nisu dostupni na ovom prijamniku. Video se može nastaviti ili ga možete reproducirati lokalno."
        }
        "Selected subtitles could not be enabled on this receiver. Play locally to preserve subtitles." => {
            "Odabrane titlove nije moguće uključiti na ovom prijamniku. Za njihovo zadržavanje reproducirajte lokalno."
        }
        "Torrent source is no longer available. Choose the source again." => {
            "Torrent izvor više nije dostupan. Ponovno odaberite izvor."
        }
        "Stop casting before joining a watch party or paired Nova session." => {
            "Prije pridruživanja zajedničkom gledanju ili uparenoj Nova sesiji zaustavite emitiranje."
        }
        "Subtitle" => "Titl",
        "Included with stream" => "Uključeno uz izvor",
        "Cast to a device" => "Emitiraj na uređaj",
        "Connecting to receiver" => "Povezivanje s prijamnikom",
        "Connected to {}" => "Povezano s {}",
        "Audio processing is off." => "Obrada zvuka je isključena.",
        "Audio processing will apply when built-in playback has audio." => {
            "Obrada zvuka primijenit će se kada ugrađeni player reproducira zvuk."
        }
        "Audio processing is active on this device." => "Obrada zvuka aktivna je na ovom uređaju.",
        "Audio processing is unavailable with this player's audio filters. Effects are disabled." => {
            "Obrada zvuka nije dostupna s filtrima ovog playera. Efekti su isključeni."
        }
        "Audio processing could not be disabled. Stop playback and try again." => {
            "Obrada zvuka nije isključena. Zaustavite reprodukciju i pokušajte ponovno."
        }
        "This device" => "Ovaj uređaj",
        "Play on this device" => "Reproduciraj na ovom uređaju",
        "Audio-only receiver" => "Prijamnik podržava samo zvuk",
        "Use Play here to return playback" => {
            "Za povratak reprodukcije odaberite Reproduciraj ovdje"
        }
        "Play here" => "Reproduciraj ovdje",
        "Stop casting" => "Zaustavi emitiranje",
        "Replace playback" => "Zamijeni reprodukciju",
        "This receiver is already in use. Replace its playback?" => {
            "Ovaj je prijamnik već u upotrebi. Zamijeniti reprodukciju?"
        }
        "Receiver IP address" => "IP adresa prijamnika",
        "Enter address" => "Unesi adresu",
        "Search this network once" => "Pretraži ovu mrežu jednom",
        "Cancel search" => "Otkaži pretraživanje",
        "Cancel connection" => "Otkaži povezivanje",
        "No receivers found. Check that both devices are on the same Wi-Fi network." => {
            "Nema pronađenih prijamnika. Provjerite jesu li oba uređaja na istoj Wi-Fi mreži."
        }
        "Try this stream" => "Isprobaj ovaj izvor",
        "Retry connection" => "Pokušaj ponovno povezivanje",
        "Forward 10 seconds" => "Naprijed 10 sekundi",
        "Back 10 seconds" => "Natrag 10 sekundi",
        "Unmute" => "Uključi zvuk",
        "Mute" => "Isključi zvuk",
        "Background casting could not start. Keep Nova open and try again." => {
            "Emitiranje u pozadini nije pokrenuto. Ostavite Novu otvorenu i pokušajte ponovno."
        }
        "Buffering on receiver" => "Učitavanje na prijamniku",
        "Cannot reach receiver. Check Wi-Fi, VPN and firewall settings." => {
            "Nije moguće pristupiti prijamniku. Provjerite Wi-Fi, VPN i vatrozid."
        }
        "Cannot serve media on this network. Check VPN and firewall settings." => {
            "Nije moguće dijeliti medij na ovoj mreži. Provjerite VPN i vatrozid."
        }
        "Casting stopped" => "Emitiranje je zaustavljeno",
        "Choose a source again to transfer this playback." => {
            "Ponovno odaberite izvor za prijenos ove reprodukcije."
        }
        "Connected to receiver" => "Povezano s prijamnikom",
        "Connected. Choose something to watch." => "Povezano. Odaberite što želite gledati.",
        "Connecting to receiver…" => "Povezivanje s prijamnikom…",
        "Connection cancelled" => "Povezivanje je otkazano",
        "Could not confirm receiver stopped. Local playback remains paused. Retry Stop casting." => {
            "Nije potvrđeno zaustavljanje prijamnika. Lokalna reprodukcija ostaje pauzirana. Ponovite Zaustavi emitiranje."
        }
        "Enter a valid receiver IP address." => "Unesite valjanu IP adresu prijamnika.",
        "Leave the watch party or paired Nova session before casting." => {
            "Prije emitiranja napustite zajedničko gledanje ili uparenu Nova sesiju."
        }
        "Media could not be prepared safely. Choose another source or play locally." => {
            "Medij nije moguće sigurno pripremiti. Odaberite drugi izvor ili reproducirajte lokalno."
        }
        "Media preparation exceeded its storage limit. Free space or choose another source." => {
            "Priprema medija premašila je ograničenje prostora. Oslobodite prostor ili odaberite drugi izvor."
        }
        "Media source is unavailable. Refresh sources and try again." => {
            "Izvor medija nije dostupan. Osvježite izvore i pokušajte ponovno."
        }
        "Paused on receiver" => "Pauzirano na prijamniku",
        "Playback changed on another device. Reconnect to continue." => {
            "Reprodukcija je promijenjena s drugog uređaja. Ponovno se povežite za nastavak."
        }
        "Playback finished on receiver" => "Reprodukcija je završena na prijamniku",
        "Playing on receiver" => "Reprodukcija na prijamniku",
        "Preparing media for receiver…" => "Priprema medija za prijamnik…",
        "Receiver changed. Choose the source again." => {
            "Prijamnik je promijenjen. Ponovno odaberite izvor."
        }
        "Receiver could not pause safely. Local playback was retained." => {
            "Prijamnik nije moguće sigurno pauzirati. Lokalna reprodukcija je zadržana."
        }
        "Receiver did not confirm playback. Try another source." => {
            "Prijamnik nije potvrdio reprodukciju. Pokušajte drugi izvor."
        }
        "Receiver did not respond. Check its power and Wi-Fi, then try again." => {
            "Prijamnik ne odgovara. Provjerite napajanje i Wi-Fi pa pokušajte ponovno."
        }
        "Receiver format support is unknown. Try this stream, choose another source or play here." => {
            "Podrška formata na prijamniku nije poznata. Isprobajte izvor, odaberite drugi ili reproducirajte ovdje."
        }
        "Receiver is already in use." => "Prijamnik je već u upotrebi.",
        "Receiver playback stopped. Choose a source to continue." => {
            "Reprodukcija na prijamniku je zaustavljena. Odaberite izvor za nastavak."
        }
        "Receiver returned an invalid response. Try reconnecting." => {
            "Prijamnik je vratio nevaljan odgovor. Pokušajte ponovno povezivanje."
        }
        "Searching for receivers…" => "Traženje prijamnika…",
        "Stop casting or choose Play here before changing receiver." => {
            "Prije promjene prijamnika zaustavite emitiranje ili odaberite Reproduciraj ovdje."
        }
        "This HLS source is unsupported. Choose another source or play locally." => {
            "Ovaj HLS izvor nije podržan. Odaberite drugi izvor ili reproducirajte lokalno."
        }
        "This action is unavailable while casting." => {
            "Ova radnja nije dostupna tijekom emitiranja."
        }
        "This receiver does not support this operation." => {
            "Ovaj prijamnik ne podržava ovu radnju."
        }
        "This source cannot be sent to this receiver without transcoding. Choose another source." => {
            "Ovaj izvor zahtijeva transkodiranje za ovaj prijamnik. Odaberite drugi izvor."
        }
        "This source cannot be shared safely. Choose another source." => {
            "Ovaj izvor nije moguće sigurno dijeliti. Odaberite drugi izvor."
        }
        "Browser links cannot be cast. Choose a direct media source." => {
            "Veze za preglednik nije moguće emitirati. Odaberite izravan medijski izvor."
        }
        "Casting" => "Emitiranje",
        "Unlock on TV." => "Otključajte na TV-u.",
        "Skipped expired or unsupported values" => "Preskočene istekle ili nepodržane vrijednosti",
        "Keep downloaded file" => "Zadrži preuzetu datoteku",
        "Unpin downloaded file" => "Ukloni oznaku zadržavanja datoteke",
        "Backup and import" => "Sigurnosna kopija i uvoz",
        "Data" => "Podaci",
        "Export selected data and restore a backup" => {
            "Izvoz odabranih podataka i vraćanje sigurnosne kopije"
        }
        "Map profiles, select components and review changes before importing." => {
            "Povežite profile, odaberite dijelove i pregledajte promjene prije uvoza."
        }
        "Choose the profiles and personal data to export." => {
            "Odaberite profile i osobne podatke za izvoz."
        }
        "Presets" => "Predlošci",
        "Everything personal" => "Svi osobni podaci",
        "All personal components, including credentials and profile PINs." => {
            "Svi osobni dijelovi, uključujući pristupne podatke i PIN-ove profila."
        }
        "Library and viewing" => "Biblioteka i gledanje",
        "Library, progress, history, collections, bookmarks and queue." => {
            "Biblioteka, napredak, povijest, zbirke, oznake i red gledanja."
        }
        "Configuration" => "Konfiguracija",
        "Personal settings, addons, integrations and profile policy." => {
            "Osobne postavke, dodaci, integracije i pravila profila."
        }
        "Target profiles" => "Odredišni profili",
        "Replace selected components" => "Zamijeni odabrane dijelove",
        "Absent records in selected components will be deleted." => {
            "Zapisi koji nedostaju u odabranim dijelovima bit će izbrisani."
        }
        "Merge inserts missing records and preserves local conflicts." => {
            "Spajanje dodaje zapise koji nedostaju i zadržava lokalne vrijednosti pri sukobu."
        }
        "Components" => "Dijelovi",
        "Use backup for conflicts" => "Pri sukobu koristi sigurnosnu kopiju",
        "Import preview" => "Pregled uvoza",
        "Selected configuration includes addon credentials and profile PINs. Imported edits sync to paired devices. Notification delivery and smart downloads stay off on this device." => {
            "Odabrana konfiguracija uključuje pristupne podatke dodataka i PIN-ove profila. Uvezene promjene sinkroniziraju se s uparenim uređajima. Obavijesti i pametna preuzimanja ostaju isključeni na ovom uređaju."
        }
        "Export selected data" => "Izvezi odabrane podatke",
        "Import reviewed changes" => "Uvezi pregledane promjene",
        "Build import preview" => "Pripremi pregled uvoza",
        "Rebuild import preview" => "Ponovno pripremi pregled uvoza",
        "Choose another backup file" => "Odaberi drugu sigurnosnu kopiju",
        "Choose backup file to import" => "Odaberi sigurnosnu kopiju za uvoz",
        "Undo last import" => "Poništi posljednji uvoz",
        "Delete recovery copies" => "Izbriši kopije za oporavak",
        "Playback progress" => "Napredak reprodukcije",
        "Viewing journal" => "Dnevnik gledanja",
        "Viewing activity" => "Aktivnost gledanja",
        "Hidden Continue rows" => "Skriveni redovi nastavka gledanja",
        "Hidden history rows" => "Skriveni redovi povijesti",
        "Hidden new episodes" => "Skrivene nove epizode",
        "Personal settings and Home" => "Osobne postavke i početna stranica",
        "Title playback preferences" => "Postavke reprodukcije naslova",
        "Addons and configuration" => "Dodaci i konfiguracija",
        "Optional addon package files" => "Neobavezne datoteke paketa dodataka",
        "Integration accounts and mappings" => "Računi i povezivanja integracija",
        "Smart offline policies" => "Pravila pametnih izvanmrežnih preuzimanja",
        "This device's compatible preferences" => "Kompatibilne postavke ovog uređaja",
        "Create new profile" => "Stvori novi profil",
        "Choose at least one profile and component" => "Odaberite barem jedan profil i dio",
        "Preparing backup…" => "Priprema sigurnosne kopije…",
        "Backup exported" => "Sigurnosna kopija je izvezena",
        "Export cancelled" => "Izvoz je otkazan",
        "Choose a backup file…" => "Odaberite sigurnosnu kopiju…",
        "Choose target profiles and components, then rebuild the preview" => {
            "Odaberite odredišne profile i dijelove pa pripremite pregled"
        }
        "Unsupported optional components" => "Nepodržani neobavezni dijelovi",
        "Import cancelled" => "Uvoz je otkazan",
        "Preparing import preview…" => "Priprema pregleda uvoza…",
        "Importing selected data…" => "Uvoz odabranih podataka…",
        "Import complete. Recovery copy saved; Undo is available." => {
            "Uvoz je dovršen. Kopija za oporavak je spremljena; poništavanje je dostupno."
        }
        "Recovery copies deleted" => "Kopije za oporavak su izbrisane",
        "Archive" => "Sigurnosna kopija",
        "Identical" => "Jednako",
        "Conflicts" => "Sukobi",
        "Updates" => "Ažuriranja",
        "Preserved local" => "Zadržano lokalno",
        "Deletions" => "Brisanja",
        "Conflict identities" => "Identiteti sukobljenih zapisa",
        "Review the exact changes, then import" => "Pregledajte točne promjene pa uvezite podatke",
        "Category assignments omitted because their categories are unavailable" => {
            "Dodjele kategorija su izostavljene jer njihove kategorije nisu dostupne"
        }
        "Included" => "Uključeno",
        "Already installed" => "Već instalirano",
        "Reinstall needed" => "Potrebna je ponovna instalacija",
        "Target changed while preview was open. Rebuild the preview before importing." => {
            "Odredište se promijenilo tijekom pregleda. Ponovno pripremite pregled prije uvoza."
        }
        "Device preferences changed while preview was open. Rebuild the preview before importing." => {
            "Postavke uređaja promijenile su se tijekom pregleda. Ponovno pripremite pregled prije uvoza."
        }
        "Choose one distinct target for each backup profile" => {
            "Odaberite zasebno odredište za svaki profil iz sigurnosne kopije"
        }
        "Unlock the target profile before importing" => "Otključajte odredišni profil prije uvoza",
        "There is no local import to undo" => "Nema lokalnog uvoza za poništavanje",
        "Unsupported backup format version" => "Nepodržana verzija sigurnosne kopije",
        "Backup manifest is missing" => "Nedostaje manifest sigurnosne kopije",
        "Duplicate or invalid backup path" => "Dvostruka ili neispravna putanja sigurnosne kopije",
        "Duplicate or invalid backup component" => "Dvostruki ili neispravan dio sigurnosne kopije",
        "Expanded backup exceeds the size limit" => {
            "Raspakirana sigurnosna kopija premašuje ograničenje veličine"
        }
        "Backup exceeds the size limit" => "Sigurnosna kopija premašuje ograničenje veličine",
        "Document exceeds the size limit" => "Dokument premašuje ograničenje veličine",
        "A document picker is already open" => "Odabir dokumenta već je otvoren",
        "Document picker could not be opened" => "Odabir dokumenta nije moguće otvoriti",
        "Selected addon package file is missing" => "Nedostaje odabrana datoteka paketa dodatka",
        "Missing addon package file" => "Nedostaje datoteka paketa dodatka",
        "Addon package checksum failed" => "Kontrolni zbroj paketa dodatka nije valjan",
        "Addon package identity mismatch" => "Identitet paketa dodatka se ne podudara",
        "Smart downloads" => "Pametna preuzimanja",
        "Keep offline" => "Čuvaj izvanmrežno",
        "Offline rule enabled" => "Izvanmrežno pravilo uključeno",
        "Rules sync between profiles. Downloads run only for profiles enabled on this device, on unmetered Wi-Fi or Ethernet." => {
            "Pravila se sinkroniziraju između profila. Preuzimanja se izvode samo za profile uključene na ovom uređaju, putem Wi-Fi ili Ethernet veze bez ograničenja prometa."
        }
        "Pause this profile on this device" => "Pauziraj ovaj profil na ovom uređaju",
        "Enable this profile on this device" => "Uključi ovaj profil na ovom uređaju",
        "Automatic allowance: {} GiB" => "Ograničenje automatskih preuzimanja: {} GiB",
        "Charging required" => "Potrebno je punjenje",
        "Charging optional" => "Punjenje nije obvezno",
        "Open a show and choose Keep offline to add a rule." => {
            "Otvorite seriju i odaberite Čuvaj izvanmrežno da dodate pravilo."
        }
        "Pause rule" => "Pauziraj pravilo",
        "Resume rule" => "Nastavi pravilo",
        "Keep next {}" => "Čuvaj sljedećih {}",
        "Choose another source" => "Odaberi drugi izvor",
        "Choose source" => "Odaberi izvor",
        "Retry / Restore offline" => "Ponovi / vrati izvanmrežno",
        "Specials included" => "Specijali uključeni",
        "Specials excluded" => "Specijali isključeni",
        "Remove unneeded automatic files" => "Ukloni nepotrebne automatske datoteke",
        "Keep watched automatic files" => "Zadrži odgledane automatske datoteke",
        "Remove rule, keep files" => "Ukloni pravilo, zadrži datoteke",
        "Remove rule and unneeded files" => "Ukloni pravilo i nepotrebne datoteke",
        "Waiting for unmetered Wi-Fi or Ethernet" => {
            "Čeka se Wi-Fi ili Ethernet bez ograničenja prometa"
        }
        "Waiting for charging" => "Čeka se punjenje",
        "Automatic storage allowance reached" => "Dosegnuto je ograničenje automatske pohrane",
        "Preserving free storage reserve" => "Čuva se rezerva slobodnog prostora",
        "Execution disabled on this device" => "Izvršavanje je isključeno na ovom uređaju",
        "Size unknown — choose Download manually" => {
            "Veličina nije poznata — odaberite ručno preuzimanje"
        }
        "Paused by you" => "Ručno ste pauzirali",
        "Pending background opportunity" => "Čeka se prilika za rad u pozadini",
        "Choose a source for smart downloads" => "Odaberite izvor za pametna preuzimanja",
        "Removed manually — use Restore offline" => {
            "Ručno uklonjeno — odaberite Vrati izvanmrežno"
        }
        "Download failed — Retry or choose a source" => {
            "Preuzimanje nije uspjelo — ponovite ili odaberite izvor"
        }
        "Smart downloads ready" => "Pametna preuzimanja su spremna",
        "Source removed — choose another source" => "Izvor je uklonjen — odaberite drugi izvor",
        "Source is not allowed" => "Izvor nije dopušten",
        "No matching release — choose another source" => {
            "Nema odgovarajućeg izdanja — odaberite drugi izvor"
        }
        "Choose an exact torrent file for smart downloads" => {
            "Odaberite točnu torrent datoteku za pametna preuzimanja"
        }
        "Choose a downloadable source" => "Odaberite izvor za preuzimanje",
        "Source changed — choose another source" => {
            "Izvor je promijenjen — odaberite drugi izvor"
        }
        "Open an episode and use this source for smart downloads" => {
            "Otvorite epizodu i odaberite ovaj izvor za pametna preuzimanja"
        }
        "Use this source for smart downloads" => "Koristi ovaj izvor za pametna preuzimanja",
        "Automatic storage" => "Automatska pohrana",
        "Reserved" => "Rezervirano",
        "Manually kept" => "Ručno zadržano",
        "ready" => "spremno",
        "downloading" => "preuzima se",
        "Source lookup failed — retry later" => {
            "Pretraživanje izvora nije uspjelo — pokušajte kasnije"
        }
        "Offline settings unavailable" => "Izvanmrežne postavke nisu dostupne",
        "Over allowance" => "Prekoračenje ograničenja",
        "Profiles enabled on this device" => "Profili uključeni na ovom uređaju",
        "Enabled on this device" => "Uključeno na ovom uređaju",
        "Disabled on this device" => "Isključeno na ovom uređaju",

        "Queue item is unavailable on this device. Sync, then retry or choose a source on the TV." => {
            "Sadržaj reda nije dostupan na ovom uređaju. Sinkronizirajte pa pokušajte ponovno ili odaberite izvor na TV-u."
        }
        "Select and unlock the same profile on the playing device." => {
            "Odaberite i otključajte isti profil na uređaju za reprodukciju."
        }

        "Continue queue" => "Nastavi red gledanja",
        "Autoplay next queued item: On" => "Automatski sljedeći sadržaj: uključeno",
        "Autoplay next queued item: Off" => "Automatski sljedeći sadržaj: isključeno",
        "Skip" => "Preskoči",
        "Stop queue" => "Zaustavi red gledanja",
        "CURRENT" => "TRENUTAČNO",
        "Start from this item" => "Počni od ovog sadržaja",
        "Skip queued item" => "Preskoči sadržaj u redu",
        "In progress" => "U tijeku",
        "Queue paused. Choose a source, retry, skip or stop the queue." => {
            "Red gledanja je pauziran. Odaberite izvor, pokušajte ponovno, preskočite ili zaustavite red."
        }
        "Queue playback is unavailable" => "Reprodukcija reda gledanja nije dostupna",
        "Queue is unavailable" => "Red gledanja nije dostupan",
        "No pending queue item" => "Nema sadržaja na čekanju u redu",
        "Title is unavailable" => "Naslov nije dostupan",
        "Queue paused" => "Red gledanja je pauziran",
        "Next queued item" => "Sljedeći sadržaj u redu",
        "Queue playing" => "Reprodukcija reda gledanja",
        "Play now" => "Reproduciraj sada",

        "Local files with unknown source" => "Lokalne datoteke s nepoznatim izvorom",
        "Incorrect PIN. Try again." => "Pogrešan PIN. Pokušajte ponovno.",
        "Unsupported profile policy." => "Nepodržana pravila profila.",
        "Enter 4–8 digits." => "Unesite 4–8 znamenki.",
        "Set a PIN first." => "Najprije postavite PIN.",
        "Enter PIN to continue." => "Unesite PIN za nastavak.",
        "Released today" => "Objavljeno danas",
        "Full season released" => "Cijela sezona objavljena",
        " — paused (On Hold / Dropped)" => " — pauzirano (Na čekanju / Odbačeno)",
        " — paused (not in Library)" => " — pauzirano (nije u zbirci)",
        "Allow notifications in Android settings, then enable delivery again" => {
            "Dopustite obavijesti u postavkama Androida, a zatim ponovno uključite dostavu"
        }
        "Unlock profile management first" => "Najprije otključajte upravljanje profilom",
        "Export options changed; preview this export again" => {
            "Postavke izvoza su promijenjene; ponovno pregledajte izvoz"
        }
        "Preview this export again before saving" => "Ponovno pregledajte izvoz prije spremanja",
        "Selection changed; rebuild the preview before importing" => {
            "Odabir je promijenjen; ponovno pripremite pregled prije uvoza"
        }
        "Recovery copies changed. Review them again before deleting." => {
            "Kopije za oporavak su promijenjene. Ponovno ih pregledajte prije brisanja."
        }
        "There are no recovery copies to delete" => "Nema kopija za oporavak za brisanje",
        "Unreadable recovery copy" => "Nečitljiva kopija za oporavak",
        "Profile changed; preview this export again" => {
            "Profil je promijenjen; ponovno pregledajte izvoz"
        }
        "Enter valid start and end dates" => "Unesite valjane početne i završne datume",
        "Choose an ordered date range of at most two years" => {
            "Odaberite raspon datuma pravilnim redoslijedom, najviše dvije godine"
        }
        "Metadata refresh pending; cached schedule retained" => {
            "Osvježavanje metapodataka je na čekanju; zadržan je spremljeni raspored"
        }
        "Could not read your scene bookmarks. Your saved data has been kept." => {
            "Oznake scena nije moguće pročitati. Spremljeni podaci su sačuvani."
        }
        "Could not save your scene bookmark. Your note has been kept." => {
            "Oznaku scene nije moguće spremiti. Vaša je bilješka sačuvana."
        }

        "Other video codecs" => "Ostali videokodeci",
        "Use language codes such as en, ja, hr." => "Koristite jezične kodove poput en, ja, hr.",
        "Added date unknown" => "Datum dodavanja nije poznat",
        "Untitled saved title" => "Spremljeni naslov bez naziva",
        "Bookmarks could not be loaded. Saved data has been preserved." => {
            "Oznake nije moguće učitati. Spremljeni podaci su sačuvani."
        }
        "Bookmarks could not be saved. Please try again." => {
            "Oznake nije moguće spremiti. Pokušajte ponovno."
        }
        "Enter a runtime from 1 to 1440 minutes, or leave it empty." => {
            "Unesite trajanje od 1 do 1440 minuta ili ostavite prazno."
        }
        "Choose a collection name of up to 80 characters and a genre of up to 80 characters." => {
            "Odaberite naziv zbirke i žanr, svaki do 80 znakova."
        }
        "This collection was removed. Close the editor and create a new collection." => {
            "Ova je zbirka uklonjena. Zatvorite uređivač i stvorite novu zbirku."
        }
        "You can save up to 32 smart collections." => "Možete spremiti do 32 pametne zbirke.",
        "This source no longer matches your stream rules." => {
            "Ovaj izvor više ne odgovara vašim pravilima izvora."
        }
        "Selected sources no longer match your stream rules. Choose streams again." => {
            "Odabrani izvori više ne odgovaraju vašim pravilima. Ponovno odaberite izvore."
        }
        "No addon streams match your rules." => {
            "Nijedan izvor dodatka ne odgovara vašim pravilima."
        }
        "No addon streams match your rules. Change them in Settings → Player." => {
            "Nijedan izvor dodatka ne odgovara vašim pravilima. Promijenite ih u Postavke → Reprodukcija."
        }
        "No smart collections" => "Nema pametnih zbirki",
        "Required audio" => "Obavezan zvuk",
        "Preferred audio" => "Željeni zvuk",
        "Any resolution" => "Sve rezolucije",
        "Any size" => "Sve veličine",
        "Uses preferred audio languages" => "Koristi željene jezike zvuka",
        "Saved custom languages" => "Spremljeni prilagođeni jezici",
        "All video codecs" => "Svi videokodeci",
        "No excluded codecs" => "Nema izuzetih kodeka",
        "No codec preference" => "Nema prednosti kodeka",
        "Excluded resolutions" => "Izuzete rezolucije",
        "Preferred resolutions" => "Željene rezolucije",
        "Only preferred resolutions" => "Samo željene rezolucije",
        "No excluded resolutions" => "Nema izuzetih rezolucija",
        "No resolution preference" => "Nema prednosti rezolucije",
        "Add resolution preference" => "Dodaj željenu rezoluciju",
        "Exclude resolution" => "Izuzmi rezoluciju",
        "Remove excluded resolution" => "Ukloni izuzetu rezoluciju",
        "Use this order when sorting by Resolution. Unlisted resolutions follow the numerical sort direction." => {
            "Primijeni ovaj redoslijed pri sortiranju prema rezoluciji. Ostale rezolucije slijede brojčani smjer sortiranja."
        }
        "Excluded resolutions stay hidden even when they are preferred." => {
            "Izuzete rezolucije ostaju skrivene čak i kada su željene."
        }
        "Hide other advertised resolutions. An empty list allows every resolution; unknown details follow the setting below." => {
            "Sakrij druge navedene rezolucije. Prazan popis dopušta sve rezolucije; za nepoznate pojedinosti vrijedi postavka u nastavku."
        }
        "Uses preferred resolutions first; direction orders the remaining resolutions." => {
            "Najprije koristi željene rezolucije; smjer određuje redoslijed ostalih rezolucija."
        }
        "Codec priority" => "Redoslijed kodeka",
        "Excluded video codecs" => "Izuzeti videokodeci",
        "Profile and device stream preferences" => "Postavke izvora profila i uređaja",
        "Managed in Settings → Player → Stream filters." => {
            "Upravlja se u Postavke → Reprodukcija → Filtri izvora."
        }
        "Only preferred video codecs" => "Samo željeni videokodeci",
        "Audio language" => "Jezik zvuka",
        "Video codec" => "Videokodek",
        "Subtitle language" => "Jezik titlova",
        "Preferred first" => "Željeni prvo",
        "Reverse preference order" => "Obrnuti redoslijed želja",
        "Uncached first" => "Nespremljeni prvo",
        "Cached first" => "Spremljeni prvo",
        "Single episodes first" => "Pojedinačne epizode prvo",
        "Season packs first" => "Paketi sezona prvo",
        "Smallest first" => "Najmanji prvo",
        "Largest first" => "Najveći prvo",
        "Lowest first" => "Najniže prvo",
        "Highest first" => "Najviše prvo",
        "Fewest first" => "Najmanje prvo",
        "Most first" => "Najviše prvo",
        "Unknown details kept" => "Nepoznate pojedinosti zadržane",
        "Unknown details hidden" => "Nepoznate pojedinosti skrivene",
        "Watched" => "Pogledano",
        "Watch history and progress" => "Povijest gledanja i napredak",
        "Playback preferences" => "Postavke reprodukcije",
        "Last stream choices" => "Posljednji odabrani izvori",
        "Other account data" => "Ostali podaci računa",
        "Stored on this device" => "Spremljeno na ovom uređaju",
        "Local sync records unavailable" => "Lokalni zapisi sinkronizacije nisu dostupni",
        "Refresh after local storage becomes available." => {
            "Osvježite kad lokalna pohrana bude dostupna."
        }
        "Unavailable" => "Nedostupno",
        "Episode with no saved title" => "Epizoda bez spremljenog naslova",
        "Unwatched" => "Nije pogledano",
        "Latest activity" => "Posljednja aktivnost",
        "Watch action" => "Radnja označavanja kao pogledano",
        "Unwatch action" => "Radnja označavanja kao nepogledano",
        "Device" => "Uređaj",
        "Exchange recorded" => "Razmjena zabilježena",
        "Delivery unknown" => "Dostava nije poznata",
        "No completed exchange snapshot is recorded for this device. Delivery is unknown." => {
            "Za ovaj uređaj nema snimke dovršene razmjene. Dostava nije poznata."
        }
        "The watch action is newer than the unwatch action. Playback activity alone does not clear watched." => {
            "Oznaka pogledanog novija je od oznake nepogledanog. Sama reprodukcija ne uklanja oznaku pogledanog."
        }
        "The unwatch action wins over the watch action. Equal action clocks favor unwatch." => {
            "Oznaka nepogledanog prevladava nad oznakom pogledanog. Pri jednakim satovima radnji prednost ima nepogledano."
        }
        "No watch action is recorded. Playback activity alone does not mark this episode watched." => {
            "Nema zabilježene oznake pogledanog. Sama reprodukcija ne označava ovu epizodu kao pogledanu."
        }
        "The local view differs from this stored sync state; application may still be pending." => {
            "Lokalni prikaz razlikuje se od spremljenog stanja sinkronizacije; primjena možda još čeka."
        }
        "Includes action registers from confirmed episode aliases." => {
            "Uključuje registre radnji potvrđenih alternativnih oznaka epizode."
        }
        "Some viewing sessions could not be read. Your saved records have been preserved." => {
            "Neka gledanja nije moguće pročitati. Vaši spremljeni zapisi su sačuvani."
        }
        "Your queue could not be loaded. Saved data has been preserved." => {
            "Red gledanja nije moguće učitati. Spremljeni podaci su sačuvani."
        }
        "Your queue could not be saved. Please try again." => {
            "Red gledanja nije moguće spremiti. Pokušajte ponovno."
        }
        "Your queue is full. Remove an item before adding another." => {
            "Red gledanja je pun. Uklonite stavku prije dodavanja nove."
        }
        "Chapter" => "Poglavlje",
        "Open in browser" => "Otvori u pregledniku",
        "Could not open this link in your browser." => {
            "Poveznicu nije moguće otvoriti u pregledniku."
        }
        "Season download planning supports up to 500 episodes." => {
            "Planiranje preuzimanja sezone podržava do 500 epizoda."
        }
        "No eligible episodes to download." => "Nema dostupnih epizoda za preuzimanje.",
        "Select a stream for download" => "Odaberite izvor za preuzimanje",
        "Select stream" => "Odaberi izvor",
        "Unknown size" => "Nepoznata veličina",
        "No matching release. Select a stream for this episode." => {
            "Nema odgovarajućeg izdanja. Odaberite izvor za ovu epizodu."
        }
        "This source does not identify a different episode. Choose another stream." => {
            "Ovaj izvor ne označava drugu epizodu. Odaberite drugi izvor."
        }
        "This download plan is out of date. Cancel and start again." => {
            "Ovaj je plan preuzimanja zastario. Otkažite i počnite ponovno."
        }
        "Not enough free space for the known download size." => {
            "Nema dovoljno slobodnog prostora za poznatu veličinu preuzimanja."
        }
        "Choose another stream" => "Odaberi drugi izvor",
        "Catalogs" => "Katalozi",
        "Subtitles" => "Titlovi",
        "Picture in picture" => "Slika u slici",
        "Play video in a floating window when leaving Nova. Disabled keeps playback paused in the background." => {
            "Reproducirajte video u plutajućem prozoru pri izlasku iz Nove. Kada je isključeno, reprodukcija ostaje pauzirana u pozadini."
        }
        "Stremio official addons" => "Službeni dodaci za Stremio",
        "Enter a public HTTPS addon-directory manifest URL." => {
            "Unesite javni HTTPS URL manifesta direktorija dodataka."
        }
        "Could not load this directory. Retry to try again." => {
            "Nije moguće učitati ovaj direktorij. Pokušajte ponovno."
        }
        "Loading addon directory…" => "Učitavanje direktorija dodataka…",
        "Complete configuration in your browser, then paste the configured install URL into Nova's Installed tab." => {
            "Dovršite konfiguraciju u pregledniku, zatim zalijepite konfigurirani URL za instalaciju na Novinu karticu Instalirano."
        }
        "Could not open the browser. Copy the addon URL and configure it in your browser." => {
            "Nije moguće otvoriti preglednik. Kopirajte URL dodatka i konfigurirajte ga u pregledniku."
        }
        "Addon installation failed. You can retry Install or use its URL in the Installed tab." => {
            "Instalacija dodatka nije uspjela. Pokušajte ponovno ili upotrijebite njegov URL na kartici Instalirano."
        }
        "IMDb rating" => "IMDb ocjena",
        "Runtime" => "Trajanje",
        "Status" => "Status",
        "Country" => "Država",
        "Language" => "Jezik",
        "Watch trailer" => "Pogledaj najavu",
        "Opens in your browser" => "Otvara se u pregledniku",
        "Paused by tracking scope" => "Pauzirano odabirom opsega praćenja",
        "Sync mode" => "Način sinkronizacije",
        "Full library sync" => "Sinkronizacija cijele zbirke",
        "Selected titles" => "Odabrani naslovi",
        "Selected titles mode" => "Način odabranih naslova",
        "Only explicitly enabled titles will sync. Enable a title from its Tracking page." => {
            "Sinkronizirat će se samo izričito omogućeni naslovi. Omogućite naslov na njegovoj stranici Praćenje."
        }
        "Sync only titles you explicitly enable from their Tracking page." => {
            "Sinkronizirajte samo naslove koje izričito omogućite na njihovoj stranici Praćenje."
        }
        "Loading Stremio matches…" => "Učitavanje Stremio podudaranja…",
        "Connect Stremio in Settings → Integrations first." => {
            "Prvo povežite Stremio u Postavke → Integracije."
        }
        "Could not prepare Stremio tracking. Try again." => {
            "Nije moguće pripremiti Stremio praćenje. Pokušajte ponovno."
        }
        "Choose a Stremio match, then review episode alignment." => {
            "Odaberite Stremio podudaranje, a zatim pregledajte povezivanje epizoda."
        }
        "This title is enabled for Stremio sync." => {
            "Za ovaj naslov omogućena je Stremio sinkronizacija."
        }
        "Stremio tracking stopped for this title. Existing history is kept." => {
            "Stremio praćenje ovog naslova je zaustavljeno. Postojeća povijest je sačuvana."
        }
        "Not mapped" => "Nije povezano",
        "Only mapped episodes will sync. Review the match before enabling it." => {
            "Sinkronizirat će se samo povezane epizode. Pregledajte podudaranje prije omogućavanja."
        }
        "Only titles enabled here sync with Stremio." => {
            "Sa Stremiom se sinkroniziraju samo ovdje omogućeni naslovi."
        }
        "Full library sync is enabled. You can also review a specific title's match here." => {
            "Omogućena je sinkronizacija cijele zbirke. Ovdje možete pregledati i podudaranje određenog naslova."
        }
        "Search Stremio titles or enter a catalog ID" => {
            "Pretražite Stremio naslove ili unesite ID kataloga"
        }
        "Find Stremio matches" => "Pronađi Stremio podudaranja",
        "Enable Stremio for this title" => "Omogući Stremio za ovaj naslov",
        "Stop tracking this title" => "Zaustavi praćenje ovog naslova",
        "Stremio tracking" => "Stremio praćenje",
        "Choose what to sync for this title." => "Odaberite što sinkronizirati za ovaj naslov.",
        "Verifying account…" => "Provjera računa…",
        "Enter an API key or access token first." => "Prvo unesite API ključ ili pristupni token.",
        "Sign-in expired. Start again." => "Prijava je istekla. Počnite ponovno.",
        "Waiting for refreshed sign-in from paired devices. Reconnect if this continues." => {
            "Čeka se obnovljena prijava s uparenih uređaja. Ponovno se povežite ako se čekanje nastavi."
        }
        "Service unavailable. Try again." => "Usluga nije dostupna. Pokušajte ponovno.",
        "Sync paused. Select Sync now to resume." => {
            "Sinkronizacija je zaustavljena. Odaberite Sinkroniziraj sada za nastavak."
        }
        "Merging saved titles and watched history…" => {
            "Spajanje spremljenih naslova i povijesti gledanja…"
        }
        "Library and watched history merged" => "Zbirka i povijest gledanja su spojene",
        "Reconnect this account to continue" => "Ponovno povežite ovaj račun za nastavak",
        "Service rate limit reached. Sync will retry." => {
            "Dosegnuto je ograničenje zahtjeva. Sinkronizacija će se ponoviti."
        }
        "Sync interrupted or local save failed. Try Sync now." => {
            "Sinkronizacija je prekinuta ili spremanje nije uspjelo. Pokušajte Sinkroniziraj sada."
        }
        "Unsupported or invalid service data. Some titles remain pending." => {
            "Nepodržani ili nevaljani podaci usluge. Neki naslovi ostaju na čekanju."
        }
        "This operation is unavailable for this service." => {
            "Ova radnja nije dostupna za ovu uslugu."
        }
        "Service unavailable. Sync will retry." => {
            "Usluga nije dostupna. Sinkronizacija će se ponoviti."
        }
        "No confirmed IMDb or TMDB identity. Refresh this title's metadata." => {
            "Nema potvrđenog IMDb ili TMDB identiteta. Osvježite metapodatke naslova."
        }
        "Default" => "Zadano",
        "Accent-tinted" => "Ton naglaska",
        "True black" => "Prava crna",
        "Purple" => "Ljubičasta",
        "Orange" => "Narančasta",
        "Blue" => "Plava",
        "Teal" => "Tirkizna",
        "Rose" => "Ružičasta",
        "Cyan" => "Cijan",
        "Amber" => "Jantarna",
        "Lime" => "Limeta",
        "Stremio" => "Stremio",
        "Sync library and watch progress with Stremio" => {
            "Sinkronizirajte zbirku i napredak gledanja sa Stremiom"
        }
        "Start sync" => "Pokreni sinkronizaciju",
        "Automatic sync" => "Automatska sinkronizacija",
        "Pending mappings" => "Preslikavanja na čekanju",
        "Initial merge" => "Početno spajanje",
        "Open browser" => "Otvori preglednik",
        "Open the link in your browser and enter this code to sign in to Stremio." => {
            "Otvorite poveznicu u pregledniku i unesite ovaj kod za prijavu u Stremio."
        }
        "Merge both libraries first, then keep library changes and watch progress in sync. The latest change wins conflicts." => {
            "Prvo spojite obje zbirke, a zatim sinkronizirajte promjene zbirke i napredak gledanja. U sukobu se primjenjuje najnovija promjena."
        }
        "Some titles need compatible metadata addons or confirmed episode mappings." => {
            "Neki naslovi trebaju kompatibilne dodatke za metapodatke ili potvrđena preslikavanja epizoda."
        }
        "Stremio is disconnected" => "Stremio nije povezan",
        "Connecting to Stremio…" => "Povezivanje sa Stremiom…",
        "Finish sign-in in your browser" => "Dovršite prijavu u pregledniku",
        "Stremio sign-in expired. Connect again." => {
            "Prijava u Stremio je istekla. Ponovno se povežite."
        }
        "Preparing library merge…" => "Priprema spajanja zbirki…",
        "Ready to merge both libraries" => "Sve je spremno za spajanje zbirki",
        "Syncing with Stremio…" => "Sinkronizacija sa Stremiom…",
        "Stremio sync complete" => "Sinkronizacija sa Stremiom je dovršena",
        "Stremio needs reconnection" => "Potrebno je ponovno povezivanje sa Stremiom",
        "Stremio is unavailable. Changes will retry." => {
            "Stremio nije dostupan. Promjene će se ponovno pokušati poslati."
        }
        "Stremio request timed out. Changes will retry." => {
            "Zahtjev Stremiju je istekao. Promjene će se ponovno pokušati poslati."
        }
        "Restoring Stremio connection…" => "Obnavljanje veze sa Stremiom…",
        "Reading Stremio addons…" => "Učitavanje dodataka Stremija…",
        "Reading Stremio library…" => "Učitavanje zbirke Stremija…",
        "Resolving Stremio episode metadata…" => "Učitavanje metapodataka epizoda Stremija…",
        "Saving local changes…" => "Spremanje lokalnih promjena…",
        "Saving merged library on this device…" => "Spremanje spojene zbirke na ovom uređaju…",
        "Uploading changes to Stremio…" => "Slanje promjena Stremiju…",
        "Confirming changes with Stremio…" => "Potvrđivanje promjena sa Stremiom…",
        "Unsupported title identifier" => "Nepodržani identifikator naslova",
        "Episode metadata unavailable" => "Metapodaci epizoda nisu dostupni",
        "Conflicting episode mappings" => "Sukobljena preslikavanja epizoda",
        "No confirmed episode mapping" => "Nema potvrđenog preslikavanja epizoda",
        "No confirmed Stremio catalog identifier" => {
            "Nema potvrđenog identifikatora kataloga Stremija"
        }
        "Conflicting title mapping" => "Sukobljeno preslikavanje naslova",
        "Watched history could not be aligned" => "Nije moguće uskladiti povijest gledanja",
        "Source identifier omitted" => "Identifikator izvora je izostavljen",
        "Stremio returned invalid data" => "Stremio je vratio neispravne podatke",
        "Stremio sync could not be saved" => "Sinkronizaciju sa Stremiom nije moguće spremiti",
        "Stremio sign-in cancelled" => "Prijava u Stremio je otkazana",
        "Automatic Stremio sync is paused" => "Automatska sinkronizacija sa Stremiom je pauzirana",
        "Stremio sync is rate limited. Changes will retry." => {
            "Stremio je ograničio učestalost sinkronizacije. Promjene će se ponovno pokušati poslati."
        }
        "Stremio sync is ready" => "Sinkronizacija sa Stremiom je spremna",

        "Only selected ratings will be saved." => "Spremit će se samo odabrane ocjene.",
        "Close rating prompt" => "Zatvori upit za ocjenu",
        "Timeline" => "Vremenska crta",
        "Finished watching" => "Gledanje završeno",
        "How was it?" => "Kako vam se svidjelo?",
        "Rate your completed release." => "Ocijenite dovršeno izdanje.",
        "Dismiss" => "Zatvori",
        "Save ratings" => "Spremi ocjene",
        "Unrated" => "Bez ocjene",
        "Rating" => "Ocjena",
        "Increase rating" => "Povećaj ocjenu",
        "Decrease rating" => "Smanji ocjenu",
        "Ask for ratings after completion" => "Traži ocjene nakon završetka",

        "Different edition or duration" => "Različito izdanje ili trajanje",
        "This stream cannot seek" => "Ovaj prijenos ne podržava premotavanje",
        "Local stream unavailable" => "Lokalni prijenos nije dostupan",
        "Finished" => "Završeno",
        "Choose the same title and episode" => "Odaberite isti naslov i epizodu",
        "Disconnect remote controls before joining a watch party." => {
            "Prekinite daljinsko upravljanje prije pridruživanja zajedničkom gledanju."
        }
        "Choose what to watch" => "Odaberite što ćete gledati",
        "Choose a stream" => "Odaberite izvor",
        "Waiting for the host to choose" => "Čekanje odabira domaćina",
        "Choose a seekable stream to watch together." => {
            "Za zajedničko gledanje odaberite prijenos koji podržava premotavanje."
        }
        "The watch-party invite is invalid or expired." => {
            "Pozivnica za zajedničko gledanje nije valjana ili je istekla."
        }
        "Could not connect to the watch party. Try again." => {
            "Povezivanje sa zajedničkim gledanjem nije uspjelo. Pokušajte ponovno."
        }
        "Only the host controls the shared timeline." => {
            "Samo domaćin upravlja zajedničkom reprodukcijom."
        }
        "Watch parties use normal speed; audio and subtitles remain your choice." => {
            "Zajedničko gledanje koristi normalnu brzinu; zvuk i titlove birate sami."
        }
        "The watch party has ended." => "Zajedničko gledanje je završilo.",
        "Reconnecting to the host…" => "Ponovno povezivanje s domaćinom…",
        "Waiting for participants to buffer…" => "Čekanje da sudionici učitaju videozapis…",
        "Watching together" => "Zajedničko gledanje",
        "Choose the same episode and edition. Different cuts cannot be aligned automatically." => {
            "Odaberite istu epizodu i izdanje. Različite verzije ne mogu se automatski uskladiti."
        }
        "Ready" => "Spremno",
        "Choosing a stream" => "Odabir prijenosa",
        "Choose an addon ZIP file…" => "Odaberite ZIP datoteku dodatka…",
        "Reading addon ZIP…" => "Čitanje ZIP datoteke dodatka…",
        "Installing addon ZIP…" => "Instaliranje ZIP datoteke dodatka…",
        "Addon installed" => "Dodatak je instaliran",
        "Addon package unavailable on this device" => "Paket dodatka nije dostupan na ovom uređaju",
        "The addon ZIP could not be read" => "Nije moguće pročitati ZIP datoteku dodatka",
        "The addon ZIP exceeds the 8 MiB size limit" => {
            "ZIP datoteka dodatka premašuje ograničenje veličine od 8 MiB"
        }
        "The addon ZIP could not be installed" => "Nije moguće instalirati ZIP datoteku dodatka",
        "The addon file picker could not be opened" => {
            "Nije moguće otvoriti odabir datoteke dodatka"
        }
        "No video frame is available" => "Nema dostupnog kadra videozapisa",
        "Playback changed before the screenshot was saved" => {
            "Reprodukcija se promijenila prije spremanja snimke zaslona"
        }
        "The video frame could not be captured" => "Nije moguće snimiti kadar videozapisa",
        "The screenshot capture timed out" => "Isteklo je vrijeme za snimanje zaslona",
        "The screenshot could not be saved" => "Nije moguće spremiti snimku zaslona",
        "Enter a valid add-on URL." => "Unesite valjan URL dodatka.",
        "Could not save your preferences. Please try again." => {
            "Postavke nije moguće spremiti. Pokušajte ponovno."
        }
        "APIBay (movies and TV)" => "APIBay (filmovi i serije)",
        "The Pirate Bay / APIBay (movies and TV)" => "The Pirate Bay / APIBay (filmovi i serije)",
        "Preferred languages" => "Željeni jezici",
        "Preferred audio languages" => "Željeni jezici zvuka",
        "Preferred audio language" => "Željeni jezik zvuka",
        "Preferred subtitle languages" => "Željeni jezici titlova",
        "Preferred subtitle language" => "Željeni jezik titlova",
        "Preferred language" => "Željeni jezik",
        "Preferred video codecs" => "Željeni videokodeci",
        "Preferred video codec" => "Željeni videokodek",
        "Add preference" => "Dodaj željenu opciju",
        "English" => "Engleski",
        "Japanese" => "Japanski",
        "Croatian" => "Hrvatski",
        "German" => "Njemački",
        "French" => "Francuski",
        "Spanish" => "Španjolski",
        "Italian" => "Talijanski",
        "Portuguese" => "Portugalski",
        "Russian" => "Ruski",
        "Korean" => "Korejski",
        "Chinese" => "Kineski",
        "Arabic" => "Arapski",
        "Hindi" => "Hindski",
        "Dutch" => "Nizozemski",
        "Polish" => "Poljski",
        "Turkish" => "Turski",
        "Swedish" => "Švedski",
        "Norwegian" => "Norveški",
        "Danish" => "Danski",
        "Finnish" => "Finski",
        "Czech" => "Češki",
        "Hungarian" => "Mađarski",
        "Romanian" => "Rumunjski",
        "Ukrainian" => "Ukrajinski",
        "Greek" => "Grčki",
        "Hebrew" => "Hebrejski",
        "Thai" => "Tajski",
        "Vietnamese" => "Vijetnamski",
        "Indonesian" => "Indonezijski",
        "Torrent sources" => "Torrent izvori",
        "Debrid providers" => "Debrid pružatelji",
        "Add source" => "Dodaj izvor",
        "Add provider" => "Dodaj pružatelja",
        "StremThru URL" => "StremThru URL",
        "Preferred quality" => "Željena kvaliteta",
        "Sort order" => "Redoslijed sortiranja",
        "Cached availability" => "Dostupnost u predmemoriji",
        "Resolution" => "Razlučivost",
        "Size" => "Veličina",
        "Season pack" => "Paket sezone",
        "Reported seeders" => "Prijavljeni seederi",
        "Number of seeds" => "Broj seedera",
        "PNG (lossless)" => "PNG (bez gubitaka)",
        "WebP (lossless)" => "WebP (bez gubitaka)",
        "WebP (lossy)" => "WebP (s gubicima)",
        "Minimum seeders" => "Najmanji broj seedera",
        "Playback mode" => "Način reprodukcije",
        "Any" => "Bilo koja",
        "Direct P2P" => "Izravno P2P",
        "Prefer cached TorBox" => "Prednost TorBox predmemoriji",
        "Cached TorBox only" => "Samo TorBox predmemorija",
        "Use TorBox" => "Koristi TorBox",
        "TorBox API key" => "TorBox API ključ",
        "Resolving stream…" => "Priprema streama…",
        "This stream selection is out of date. Reload streams." => {
            "Odabrani stream više nije aktualan. Ponovno učitajte streamove."
        }
        "Select a source in addon configuration." => "Odaberite izvor u postavkama dodatka.",
        "Source unavailable. Other sources are still usable." => {
            "Izvor nije dostupan. Ostali izvori i dalje su dostupni."
        }
        "Source returned an unsupported response." => "Izvor je vratio nepodržan odgovor.",
        "Provider unavailable. Try again later." => {
            "Pružatelj nije dostupan. Pokušajte ponovno kasnije."
        }
        "Enable TorBox and configure its API key to use cached playback." => {
            "Uključite TorBox i unesite API ključ za reprodukciju iz predmemorije."
        }
        "Could not resolve this stream." => "Stream nije moguće pripremiti.",
        "Reload streams and select Download again to refresh this download." => {
            "Ponovno učitajte streamove i odaberite Preuzmi za osvježavanje preuzimanja."
        }
        "Could not download this stream. Retry or check the addon configuration." => {
            "Stream nije moguće preuzeti. Pokušajte ponovno ili provjerite postavke dodatka."
        }
        "Could not resolve this stream. Try another result or check the addon configuration." => {
            "Stream nije moguće pripremiti. Odaberite drugi rezultat ili provjerite postavke dodatka."
        }
        "Configure a TorBox API key first." => "Najprije unesite TorBox API ključ.",
        "The TorBox API key is invalid." => "TorBox API ključ nije valjan.",
        "TorBox rejected the API key or account permissions." => {
            "TorBox je odbio API ključ ili dozvole računa."
        }
        "TorBox is rate limited. Try again later." => {
            "Dosegnuto je ograničenje TorBox zahtjeva. Pokušajte kasnije."
        }
        "TorBox is unavailable. Try again later." => "TorBox nije dostupan. Pokušajte kasnije.",
        "This torrent is no longer cached on TorBox." => "Torrent više nije u TorBox predmemoriji.",
        "TorBox account limits prevent playback." => {
            "Ograničenja TorBox računa sprječavaju reprodukciju."
        }
        "TorBox could not resolve this stream." => "TorBox ne može pripremiti ovaj stream.",
        "Invalid Supernova stream selection." => "Odabrani Supernova stream nije valjan.",
        "TorBox playback is disabled." => "TorBox reprodukcija je isključena.",
        "TorBox could not confirm the cached torrent." => {
            "TorBox ne može potvrditi torrent u predmemoriji."
        }
        "TorBox has not made this cached torrent available yet. Try again later." => {
            "TorBox još nije pripremio torrent iz predmemorije. Pokušajte kasnije."
        }
        "Cannot determine the correct video file." => {
            "Nije moguće odrediti ispravnu videodatoteku."
        }
        "TorBox returned an invalid playback URL." => "TorBox je vratio nevaljan URL reprodukcije.",
        "On" => "Uključeno",
        "Sleep timer unavailable" => "Mjerač spavanja nije dostupan",
        "Episode timer requires series playback" => "Mjerač epizoda zahtijeva reprodukciju serije",
        "Choose a duration timer first" => "Prvo odaberite trajanje mjerača",
        "Sleep timer finished" => "Mjerač spavanja je istekao",
        "Episode timer finished the episode portion" => "Mjerač epizoda završio je dio s epizodama",
        "Off" => "Isključeno",
        "Show skip prompt" => "Prikaži gumb za preskakanje",
        "Auto skip" => "Automatski preskoči",
        "Any (0%)" => "Bilo koja (0%)",
        "Low (25%)" => "Niska (25%)",
        "Balanced (50%)" => "Uravnotežena (50%)",
        "High (80%)" => "Visoka (80%)",
        "Very high (95%)" => "Vrlo visoka (95%)",
        "Automatic" => "Automatski",
        "Unlimited" => "Neograničeno",
        "Image disk-cache limit" => "Ograničenje slikovne predmemorije na disku",
        "Not received yet" => "Još nije primljeno",
        "Configured (hidden)" => "Postavljeno (skriveno)",
        "Not configured" => "Nije postavljeno",
        "Every minute" => "Svake minute",
        "Every 5 minutes" => "Svakih 5 minuta",
        "Every 15 minutes" => "Svakih 15 minuta",
        "Start over" => "Od početka",
        "Ask" => "Pitaj",
        "System default" => "Zadano sustava",
        "Movie" => "Film",
        "TV" => "TV",
        "Metadata and streams" => "Metapodaci i streamovi",
        "Metadata" => "Metapodaci",
        "Streams" => "Streamovi",
        "No capabilities enabled" => "Nijedna mogućnost nije uključena",
        "Season" => "Sezona",
        "Episodes" => "Epizode",
        "Files" => "Datoteke",
        "Recovered files" => "Oporavljene datoteke",
        "Downloads" => "Preuzimanja",
        "Series" => "Serije",
        "Play" => "Reproduciraj",
        "Streams for other sources" => "Streamovi za druge izvore",
        // ---- Anime tracking -------------------------------------------
        "Add tracking or align another release" => "Dodajte praćenje ili uskladite drugo izdanje",
        "Adjust individual assignments below. Leave empty to exclude a source episode." => {
            "Prilagodite pojedinačne dodjele u nastavku. Ostavite prazno za izuzimanje izvorne epizode."
        }
        "Alignment exceeds this release’s episode total." => {
            "Dodjela premašuje broj epizoda ovog izdanja."
        }
        "Alignment preview ready. Unmapped source rows will not update this release." => {
            "Pregled dodjele je spreman. Nedodijeljeni izvorni redci neće ažurirati ovo izdanje."
        }
        "Apply Nova history" => "Primijenite povijest Nove",
        "Cancel" => "Odustani",
        "Cancel sign-in" => "Odustanite od prijave",
        "Change finish date" => "Promijenite datum završetka",
        "Change start date" => "Promijenite datum početka",
        "Check episode alignment, progress, score, and date values." => {
            "Provjerite dodjelu epizoda, napredak, ocjenu i datume."
        }
        "Edit individual episodes" => "Uredi pojedinačne epizode",
        "Check the episode range. Use individual assignments only when the numbering differs." => {
            "Provjerite raspon epizoda. Pojedinačne dodjele koristite samo kada se numeriranje razlikuje."
        }
        "Episodes are still loading. Reload suggestions when they are ready." => {
            "Epizode se još učitavaju. Ponovno učitajte prijedloge kada budu spremne."
        }
        "Ongoing release; only aired episodes are linked." => {
            "Izdanje još izlazi; povezane su samo emitirane epizode."
        }
        "Tracking is ready. Add any missing releases below." => {
            "Praćenje je spremno. U nastavku dodajte izdanja koja nedostaju."
        }
        "Review the suggested releases, then start tracking." => {
            "Pregledajte predložena izdanja pa pokrenite praćenje."
        }
        "Adjust this release, then save the change to your review." => {
            "Prilagodite ovo izdanje pa spremite promjenu za pregled."
        }
        "Release metadata changed. Reload suggestions before linking." => {
            "Podaci o izdanju promijenili su se. Ponovno učitajte prijedloge prije povezivanja."
        }
        "Linked. Saved watched progress is queued." => {
            "Povezano. Spremljeni napredak gledanja čeka slanje."
        }
        "Linked. New watched episodes will update the matching release." => {
            "Povezano. Nove pogledane epizode ažurirat će odgovarajuće izdanje."
        }
        "Check this split" => "Provjerite ovu podjelu",
        "More" => "Više",
        "Review setup" => "Pregled povezivanja",
        "Include watched episodes" => "Uključi pogledane epizode",
        "Start tracking" => "Pokreni praćenje",
        "Adjust" => "Prilagodi",
        "Add missing releases" => "Dodaj izdanja koja nedostaju",
        "Reload suggestions" => "Ponovno učitaj prijedloge",
        "Choose another release" => "Odaberi drugo izdanje",
        "Save adjustment" => "Spremi prilagodbu",
        "Back to review" => "Natrag na pregled",
        "Connect a service in Settings → Integrations to see suggestions." => {
            "Povežite uslugu u Postavkama → Integracije za prikaz prijedloga."
        }
        "No new suggestions. Search a title or enter a tracker ID to link another release." => {
            "Nema novih prijedloga. Pretražite naslov ili unesite ID na usluzi za povezivanje drugog izdanja."
        }
        "Suggested releases. Check the match and confirm episode alignment before linking." => {
            "Predložena izdanja. Provjerite podudaranje i potvrdite dodjelu epizoda prije povezivanja."
        }
        "Source tracker ID matches; confirm episode coverage." => {
            "ID usluge iz izvora podudara se; potvrdite obuhvat epizoda."
        }
        "Official MAL cross-reference matches; confirm episode coverage." => {
            "Službena poveznica na MAL podudara se; potvrdite obuhvat epizoda."
        }
        "Title and year match; confirm release and episode coverage." => {
            "Naslov i godina podudaraju se; potvrdite izdanje i obuhvat epizoda."
        }
        "Title or alias matches; check year, format, and episode coverage." => {
            "Naslov ili alternativni naziv podudaraju se; provjerite godinu, format i obuhvat epizoda."
        }
        "Search result; verify the release and episode coverage." => {
            "Rezultat pretraživanja; provjerite izdanje i obuhvat epizoda."
        }
        "Suggest releases" => "Predloži izdanja",
        "Choose a service and search or enter an anime ID." => {
            "Odaberite uslugu pa pretražite ili unesite ID animea."
        }
        "Close" => "Zatvori",
        "Confirm alignment" => "Potvrdite dodjelu",
        "Confirm exactly which source rows belong to this release. Saved history is excluded until you choose Apply Nova history." => {
            "Potvrdite koji izvorni redci pripadaju ovom izdanju. Spremljena povijest isključena je dok ne odaberete Primijenite povijest Nove."
        }
        "Confirm history update" => "Potvrdite slanje povijesti",
        "Confirm tracking reset" => "Potvrdite poništavanje praćenja",
        "Confirm unlink" => "Potvrdite uklanjanje veze",
        "Conflicting source IDs. Enter a tracker ID or search manually." => {
            "Izvorni ID-ovi nisu usklađeni. Unesite ID usluge praćenja ili pretražite ručno."
        }
        "Connect" => "Povežite",
        "Connect this service in Settings → Integrations first." => {
            "Prvo povežite ovu uslugu u Postavke → Integracije."
        }
        "Shared tracking data needs alignment or a newer Nova version." => {
            "Dijeljeni podaci praćenja zahtijevaju usklađivanje ili noviju verziju Nove."
        }
        "Tracking links and sign-in received from a paired device." => {
            "Veze praćenja i prijava primljeni su s uparenog uređaja."
        }
        "Connected. Sign-in will sync with paired devices." => {
            "Povezano. Prijava će se sinkronizirati s uparenim uređajima."
        }
        "Tracker sign-in" => "Prijava na servis praćenja",
        "Sign-in expired. Please connect again." => "Prijava je istekla. Ponovno se povežite.",
        "Could not receive the browser sign-in. Please connect again." => {
            "Nije moguće primiti prijavu iz preglednika. Ponovno se povežite."
        }
        "Could not keep sign-in active. Please connect again." => {
            "Nije moguće održati prijavu aktivnom. Ponovno se povežite."
        }
        "Sign-in is saved and shared with paired devices. Disconnect removes shared sign-in." => {
            "Prijava je spremljena i dijeli se s uparenim uređajima. Prekid veze uklanja dijeljenu prijavu."
        }
        "Saved sign-in is unreadable. Reconnect this service." => {
            "Spremljena prijava nije čitljiva. Ponovno povežite ovu uslugu."
        }
        "Saved sign-in needs attention. Reconnect this service." => {
            "Spremljena prijava zahtijeva provjeru. Ponovno povežite ovu uslugu."
        }
        "Could not open the sign-in browser. Check your browser settings and reconnect." => {
            "Preglednik za prijavu nije otvoren. Provjerite postavke preglednika i ponovno se povežite."
        }
        "Disconnect" => "Prekinite vezu",
        "Disconnected. Links and queued updates are retained." => {
            "Veza je prekinuta. Veze naslova i ažuriranja u redu su sačuvana."
        }
        "Edit alignment" => "Uredite dodjelu",
        "Edit tracker entry" => "Uredite zapis praćenja",
        "Episode alignment needs review. Queued work is paused." => {
            "Dodjelu epizoda treba pregledati. Ažuriranja u redu su pauzirana."
        }
        "Finish date" => "Datum završetka",
        "AniList token" => "AniList token",
        "Approve Nova in AniList, then copy the displayed token and paste it below." => {
            "Odobrite Novu na AniListu, zatim kopirajte prikazani token i zalijepite ga ispod."
        }
        "Copy the token from AniList and paste it below to finish sign-in." => {
            "Kopirajte token s AniLista i zalijepite ga ispod kako biste dovršili prijavu."
        }
        "Finish sign-in in your browser. Paste the full return URL here if automatic return fails." => {
            "Dovršite prijavu u pregledniku. Ovdje zalijepite cijeli povratni URL ako automatski povratak ne uspije."
        }
        "Full return URL" => "Cijeli povratni URL",
        "Finish in your browser. For the PIN fallback, paste the AniList token here; otherwise paste the full return URL if automatic return fails." => {
            "Dovršite u pregledniku. Za prijavu putem PIN-a ovdje zalijepite AniList token; inače zalijepite cijeli povratni URL ako automatski povratak ne uspije."
        }
        "Finish sign-in" => "Dovršite prijavu",
        "Finish sign-in in your browser." => "Dovršite prijavu u pregledniku.",
        "First source row" => "Prvi izvorni redak",
        "First tracker episode" => "Prva epizoda na usluzi",
        "Last source row" => "Posljednji izvorni redak",
        "Linked. New watched events will update this release; saved history was not uploaded." => {
            "Povezano. Nova gledanja ažurirat će ovo izdanje; spremljena povijest nije poslana."
        }
        "Loading tracker…" => "Učitavanje usluge praćenja…",
        "Loading tracking…" => "Učitavanje praćenja…",
        "MyAnimeList dates are read-only through its API." => {
            "MyAnimeList datumi putem API-ja mogu se samo čitati."
        }
        "Nova history explicitly queued for this release." => {
            "Povijest Nove izričito je dodana u red za ovo izdanje."
        }
        "On hold" => "Na čekanju",
        "Open a title first." => "Prvo otvorite naslov.",
        "PIN token or full return URL" => "PIN token ili cijeli povratni URL",
        "Planning" => "Planirano",
        "Preview alignment" => "Pregledajte dodjelu",
        "Preview and confirm replacement coverage. Previous unsent updates will be discarded." => {
            "Pregledajte i potvrdite zamjensku dodjelu. Prethodna neposlana ažuriranja bit će odbačena."
        }
        "Progress" => "Napredak",
        "Progress (explicit decreases are allowed)" => "Napredak (dopušteno je izričito smanjenje)",
        "Public client ID" => "Javni ID klijenta",
        "Reconnect" => "Ponovno povežite",
        "Refresh / retry" => "Osvježite / pokušajte ponovno",
        "Registered redirect URL" => "Registrirani povratni URL",
        "Repeating" => "Ponovno gledanje",
        "Reset all local links and queued updates? A recovery backup is retained. Nova history and remote tracker lists stay unchanged." => {
            "Poništiti sve lokalne veze i ažuriranja u redu? Sigurnosna kopija za oporavak ostaje sačuvana. Povijest Nove i udaljeni popisi ostaju nepromijenjeni."
        }
        "Reset local tracking" => "Poništite lokalno praćenje",
        "Review the source rows and preview the alignment before confirming." => {
            "Pregledajte izvorne retke i dodjelu prije potvrde."
        }
        "Save tracker edits" => "Spremite izmjene praćenja",
        "Search tracker" => "Pretražite uslugu",
        "Select a release, then confirm its episode alignment." => {
            "Odaberite izdanje pa potvrdite dodjelu epizoda."
        }
        "Select release" => "Odaberite izdanje",
        "Sign-in canceled." => "Prijava je otkazana.",
        "Source episodes changed. Reopen Tracking to review alignment." => {
            "Izvorne epizode su promijenjene. Ponovno otvorite Praćenje za pregled dodjele."
        }
        "Source episodes changed. Review the paused alignment." => {
            "Izvorne epizode su promijenjene. Pregledajte pauziranu dodjelu."
        }
        "Start date" => "Datum početka",
        "This alignment overlaps another active release. Edit that link first." => {
            "Dodjela se preklapa s drugim aktivnim izdanjem. Prvo uredite tu vezu."
        }
        "Title, ID, or official anime URL" => "Naslov, ID ili službeni URL animea",
        "Tracker edits queued. Nova watched history is unchanged." => {
            "Izmjene praćenja dodane su u red. Povijest gledanja Nove nije promijenjena."
        }
        "Tracker entry refreshed. Retry respects service cooldowns." => {
            "Zapis praćenja je osvježen. Ponovni pokušaj poštuje ograničenja usluge."
        }
        "Tracker episode or empty" => "Epizoda na usluzi ili prazno",
        "Tracker progress" => "Napredak na usluzi",
        "Tracker score" => "Ocjena na usluzi",
        "Tracker search" => "Pretraga usluge praćenja",
        "Tracker status" => "Status na usluzi",
        "Tracking" => "Praćenje",
        "Tracking changes could not be saved. Check storage and retry." => {
            "Izmjene praćenja nisu spremljene. Provjerite pohranu i pokušajte ponovno."
        }
        "Tracking data needs recovery. Playback remains available." => {
            "Podatke praćenja treba oporaviti. Reprodukcija je i dalje dostupna."
        }
        "Tracking event data needs recovery." => "Zapise događaja praćenja treba oporaviti.",
        "Tracking reset. The previous local state is retained in a recovery backup." => {
            "Praćenje je poništeno. Prethodno lokalno stanje sačuvano je u sigurnosnoj kopiji za oporavak."
        }
        "Tracking service" => "Usluga praćenja",
        "Unlink" => "Uklonite vezu",
        "Unlink this release? Unsent updates will be discarded when no other source uses it. An update already sent may finish." => {
            "Ukloniti vezu ovog izdanja? Neposlana ažuriranja bit će odbačena kad nijedan drugi izvor ne koristi izdanje. Već poslano ažuriranje može završiti."
        }
        "Unlinked. Local history and remote tracker values are unchanged." => {
            "Veza je uklonjena. Lokalna povijest i udaljene vrijednosti nisu promijenjene."
        }
        "YYYY-MM-DD, partial date, or empty to clear" => {
            "GGGG-MM-DD, djelomičan datum ili prazno za brisanje"
        }
        "Anime progress on MyAnimeList and AniList" => "Napredak animea na MyAnimeListu i AniListu",
        "Connected" => "Povezano",
        "Reconnect to send retained updates" => {
            "Ponovno se povežite za slanje spremljenih ažuriranja"
        }
        "Needs alignment" => "Potrebna je dodjela epizoda",
        "Inactive account" => "Neaktivan račun",
        "Up to date" => "Ažurno",
        "Update queued" => "Ažuriranje je u redu",
        "Sending update" => "Slanje ažuriranja",
        "Waiting to retry" => "Čeka se ponovni pokušaj",
        "Tracker rejected the update; edit the values" => {
            "Usluga je odbila ažuriranje; uredite vrijednosti"
        }
        "Tracker sign-in required. Reconnect this account." => {
            "Potrebna je prijava na uslugu. Ponovno povežite ovaj račun."
        }
        "Tracker unavailable. Queued updates are retained." => {
            "Usluga nije dostupna. Ažuriranja u redu su sačuvana."
        }
        "Tracker rate limit. Updates will retry after the cooldown." => {
            "Ograničenje broja zahtjeva. Ažuriranja će se pokušati poslati nakon čekanja."
        }
        "Tracker rejected the request. Check the entry and values." => {
            "Usluga je odbila zahtjev. Provjerite zapis i vrijednosti."
        }
        "Invalid tracker response. The update remains unconfirmed." => {
            "Nevaljan odgovor usluge. Ažuriranje nije potvrđeno."
        }
        "Check the client registration, tracker ID, or values." => {
            "Provjerite registraciju klijenta, ID na usluzi ili vrijednosti."
        }
        "This tracker does not support that field." => "Ova usluga ne podržava to polje.",
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
        "HTTP 429: download remained rate limited after retries" => {
            "HTTP 429: ograničenje broja zahtjeva ostalo je na snazi nakon ponovnih pokušaja"
        }
        "HTTP 429: server cooldown exceeds the download retry budget; try again later" => {
            "HTTP 429: poslužitelj zahtijeva dulje čekanje od dopuštenog za ponovne pokušaje preuzimanja; pokušajte kasnije"
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
        // ---- Remote playback ------------------------------------------
        "Open Nova on the playing device to connect." => {
            "Otvorite Novu na uređaju za reprodukciju kako biste se povezali."
        }
        "This device is no longer paired." => "Ovaj uređaj više nije uparen.",
        "Playback changed. Refresh the controls and try again." => {
            "Reprodukcija se promijenila. Osvježite kontrole i pokušajte ponovno."
        }
        "Leave remote control before starting playback on this device." => {
            "Izađite iz daljinskog upravljanja prije pokretanja reprodukcije na ovom uređaju."
        }
        "This selection expired. Reload streams." => "Odabir je istekao. Ponovno učitajte zapise.",
        "This control is unavailable on the playing device." => {
            "Ova kontrola nije dostupna na uređaju za reprodukciju."
        }
        "Could not contact the playing device. Try again." => {
            "Nije moguće povezati se s uređajem za reprodukciju. Pokušajte ponovno."
        }
        "Loading streams from the playing device…" => {
            "Učitavanje zapisa s uređaja za reprodukciju…"
        }
        "Screenshot failed on the playing device" => {
            "Snimanje zaslona nije uspjelo na uređaju za reprodukciju"
        }
        "Capturing screenshot…" => "Snimanje zaslona…",
        "Screenshot saved on the playing device" => {
            "Snimka zaslona spremljena je na uređaju za reprodukciju"
        }
        "Loading stream…" => "Učitavanje zapisa…",
        "Playing" => "Reprodukcija",
        "Playback failed" => "Reprodukcija nije uspjela",
        "No active playback" => "Nema aktivne reprodukcije",
        "Resume playback?" => "Nastaviti reprodukciju?",
        "Choose streams for the next episode?" => "Odabrati zapise za sljedeću epizodu?",
        "Available" => "Dostupno",
        "Open Nova to connect" => "Otvorite Novu za povezivanje",
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
        "Unavailable catalog" => "Nedostupan katalog",
        "Background opacity (%)" => "Neprozirnost pozadine (%)",
        "Adds a translucent black backdrop to styled text subtitles on both tracks. Embedded styles and image subtitles may keep their original appearance." => {
            "Dodaje poluprozirnu crnu pozadinu stiliziranim tekstnim titlovima na oba zapisa. Ugrađeni stilovi i slikovni titlovi mogu zadržati izvorni izgled."
        }
        "Catalog grouping" => "Grupiranje kataloga",
        "Infer series grouping" => "Izvedi grupiranje serija",
        "Use explicit season relationships and a confirmed series anchor when a provider has no parent mapping. Episode numbering and progress stay unchanged." => {
            "Koristi izričite veze između sezona i potvrđenu seriju kao uporište kada pružatelj nema podatak o pripadnosti seriji. Brojevi epizoda i napredak ostaju nepromijenjeni."
        }
        "Inferred grouping" => "Izvedeno grupiranje",
        "Catalog overrides" => "Iznimke kataloga",
        "Original" => "Izvorno",
        "No catalog overrides" => "Nema iznimki kataloga",
        "catalog overrides" => "iznimki kataloga",
        "Checking this season's episode mapping…" => "Provjera rasporeda epizoda ove sezone…",
        "This season mapping changed. Open All seasons or choose the catalog card again." => {
            "Raspored ove sezone promijenjen je. Otvorite Sve sezone ili ponovno odaberite karticu kataloga."
        }
        // ---- Catalog / addons ------------------------------------------
        "Saved" => "Spremljeno",
        "Available offline · Play" => "Dostupno izvan mreže · Reproduciraj",
        "Bookmark" => "Oznaka",
        "Search your library, episodes and bookmarks." => "Pretražite zbirku, epizode i oznake.",
        "No results for this selection." => "Nema rezultata za ovaj odabir.",
        "More source pages are available. Continue browsing to load another batch." => {
            "Dostupno je još stranica izvora. Nastavite pregledavanje za učitavanje sljedeće skupine."
        }
        "This catalog page could not be loaded. Continue to retry." => {
            "Ova stranica kataloga nije učitana. Nastavite za ponovni pokušaj."
        }
        "This addon repeated an earlier page." => "Ovaj je dodatak ponovio prethodnu stranicu.",
        "No results for this search." => "Nema rezultata za ovu pretragu.",
        "Type at least 2 characters to search." => "Upišite barem 2 znaka za pretraživanje.",
        "Search movies, shows…" => "Pretražite filmove i serije…",
        "No enabled catalogs support search." => {
            "Nijedan omogućeni katalog ne podržava pretraživanje."
        }
        "Search (e.g. a movie title)" => "Pretražite (npr. naslov filma)",
        "This catalog has no search" => "Ovaj katalog ne podržava pretraživanje",
        "All addons" => "Svi dodaci",
        "All types" => "Sve vrste",
        "All catalogs" => "Svi katalozi",
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
        "▶ Resume" => "▶ Nastavite",
        "Caught up" => "Sve pogledano",
        "New episodes" => "Nove epizode",
        "Watch history" => "Povijest gledanja",
        "Because you watched" => "Zato što ste gledali",
        "Hide recommendation" => "Sakrijte preporuku",
        "Similar shows" => "Slične serije",
        "Similar movies" => "Slični filmovi",
        "Rewatch" => "Ponovno gledanje",
        "Play again" => "Reproducirajte ponovno",
        "Specials" => "Specijali",
        "Episode" => "Epizoda",
        "Resume" => "Nastavite",
        "Start" => "Počnite",
        "Continue" => "Nastavite",
        "Choose an episode" => "Odaberite epizodu",
        "Left" => "Lijevo",
        "Center" => "Sredina",
        "Right" => "Desno",
        "Small" => "Malo",
        "Medium" => "Srednje",
        "Large" => "Veliko",
        "Square" => "Ravno",
        "Subtle" => "Suptilno",
        "Soft" => "Meko",
        "Rounded" => "Zaobljeno",
        "Extra rounded" => "Jako zaobljeno",
        "None" => "Bez",
        "Tight" => "Zbijeno",
        "Compact" => "Kompaktno",
        "Comfortable" => "Udobno",
        "Spacious" => "Prostrano",
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

pub fn detail_trailer_label(number: usize) -> String {
    if croatian() {
        format!("Najava {number}")
    } else {
        format!("Trailer {number}")
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

/// Compact remaining playback time on Home cards.
pub fn minutes_left(minutes: u64) -> String {
    if croatian() {
        format!("još {minutes} min")
    } else {
        format!("{minutes} min left")
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

/// RAM usage describes the torrent buffer only, never disk usage or total RSS.
pub fn torrent_ram_buffer(used: u64, limit: u64, down_bps: u64, peer_count: usize) -> String {
    let mib = 1024.0 * 1024.0;
    format!(
        "RAM {:.0} / {:.0} MiB · ↓ {:.1} MiB/s · {}",
        used as f64 / mib,
        limit as f64 / mib,
        down_bps as f64 / mib,
        peers(peer_count)
    )
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

/// Selected stream context, e.g. `"S1 E1 · Pilot"`, with no duplicate fallback.
pub fn stream_episode_context(identifier: &str, title: &str) -> String {
    if identifier.is_empty() || identifier == title {
        title.to_owned()
    } else if title.is_empty() {
        identifier.to_owned()
    } else {
        format!("{identifier} · {title}")
    }
}

/// Decimal file-size units for addon size pills, e.g. `"1.1 GB"`.
pub fn stream_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
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

/// `"X has no browsable catalogs — …"` (also covers search-only sources).
pub fn no_browsable_catalogs(base: &str) -> String {
    if croatian() {
        format!(
            "{base} nema kataloga za pregledavanje — dodajte dodatak s katalozima (npr. Cinemeta) za pregled."
        )
    } else {
        format!(
            "{base} has no browsable catalogs — add an addon with catalogs (e.g. Cinemeta) to browse."
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

pub fn syncing_addons(pending: usize) -> String {
    if croatian() {
        format!("Sinkronizacija dodataka… (preostalo: {pending})")
    } else {
        format!("Syncing addons… ({pending} remaining)")
    }
}

pub fn addon_sync_waiting(pending: usize) -> String {
    if croatian() {
        format!("Sinkronizacija nije dovršena: dodaci na čekanju ({pending})")
    } else {
        format!("Sync incomplete: waiting for addons ({pending})")
    }
}

pub fn addon_sync_retry(pending: usize, secs: u64) -> String {
    if croatian() {
        format!(
            "Sinkronizacija dodataka nije dovršena: na čekanju ({pending}) · ponovni pokušaj za {secs} s"
        )
    } else {
        format!("Addon sync incomplete: {pending} pending · retrying in {secs}s")
    }
}

pub fn sending_addons(count: usize) -> String {
    if croatian() {
        format!("Slanje dodataka… ({count})")
    } else {
        format!("Sending addons… ({count})")
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

fn month_abbreviation(month: u32) -> &'static str {
    const EN: &[&str] = &[
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    const HR: &[&str] = &[
        "sij", "velj", "ožu", "tra", "svi", "lip", "srp", "kol", "ruj", "lis", "stu", "pro",
    ];
    let index = (month.clamp(1, 12) - 1) as usize;
    if croatian() { HR[index] } else { EN[index] }
}

/// Absolute air date: `"Mar 15, 2024"` / `"15. ožu 2024."`.
pub fn absolute_date(day: u32, month: u32, year: i64) -> String {
    let month = month_abbreviation(month);
    if croatian() {
        format!("{day}. {month} {year}.")
    } else {
        format!("{month} {day}, {year}")
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

/// The prompt names the completed mapping rather than the containing series.
pub fn completion_release_subtitle(episodes: u32) -> String {
    if croatian() {
        let noun = plural(u64::from(episodes), "epizoda", "epizode", "epizoda");
        format!("Povezano izdanje · {episodes} {noun}")
    } else {
        let noun = if episodes == 1 { "episode" } else { "episodes" };
        format!("Mapped release · {episodes} {noun}")
    }
}

pub fn tracking_setup_summary(releases: usize, episodes: usize) -> String {
    if croatian() {
        format!("Izdanja: {releases} · Epizode za povezivanje: {episodes}")
    } else {
        format!("Releases: {releases} · Episodes to link: {episodes}")
    }
}
pub fn tracking_setup_unmapped(unresolved: usize) -> String {
    if unresolved == 0 {
        return String::new();
    }
    if croatian() {
        format!("Nepovezane epizode: {unresolved}")
    } else {
        format!("Unmapped episodes: {unresolved}")
    }
}
pub fn tracking_setup_history(progress: u32) -> String {
    if croatian() {
        format!("Spremljeni napredak gledanja: najmanje {progress}")
    } else {
        format!("Saved watched progress: at least {progress}")
    }
}
pub fn tracking_episode_ranges(numbers: &[u32]) -> String {
    let mut numbers = numbers.to_vec();
    numbers.sort_unstable();
    numbers.dedup();
    let mut ranges = vec![];
    let mut i = 0;
    while i < numbers.len() {
        let first = numbers[i];
        let mut last = first;
        while i + 1 < numbers.len() && numbers[i + 1] == last.saturating_add(1) {
            i += 1;
            last = numbers[i];
        }
        ranges.push(if first == last {
            first.to_string()
        } else {
            format!("{first}–{last}")
        });
        i += 1;
    }
    ranges.join(", ")
}

pub fn tracking_setup_coverage(source: &[u32], target: &[u32]) -> String {
    if croatian() {
        format!(
            "Epizode {} → epizode {} na usluzi",
            tracking_episode_ranges(source),
            tracking_episode_ranges(target)
        )
    } else {
        format!(
            "Episodes {} → tracker episodes {}",
            tracking_episode_ranges(source),
            tracking_episode_ranges(target)
        )
    }
}
pub fn tracking_score_hint(maximum: u32, decimal: bool) -> String {
    if croatian() {
        format!(
            "Ocjena 0–{maximum}{}",
            if decimal {
                " (dopuštena jedna decimala)"
            } else {
                " (cijeli brojevi)"
            }
        )
    } else {
        format!(
            "Score 0–{maximum}{}",
            if decimal {
                " (one decimal allowed)"
            } else {
                " (whole numbers)"
            }
        )
    }
}
pub fn tracking_coverage(count: usize, first: u32, last: u32) -> String {
    if croatian() {
        format!("{count} potvrđenih izvornih redaka → epizode {first}–{last}")
    } else {
        format!("{count} confirmed source rows → episodes {first}–{last}")
    }
}
pub fn tracking_candidate(
    format: &str,
    year: Option<u16>,
    episodes: Option<u32>,
    id: u32,
) -> String {
    let year = year
        .map(|y| y.to_string())
        .unwrap_or_else(|| tr("Unknown").into());
    let episodes = episodes
        .map(|n| n.to_string())
        .unwrap_or_else(|| tr("Unknown").into());
    if croatian() {
        std::format!("{format} · {year} · epizode: {episodes} · ID {id}")
    } else {
        std::format!("{format} · {year} · episodes: {episodes} · ID {id}")
    }
}
pub fn tracking_assignment(source: &str, ordinal: u32) -> String {
    if croatian() {
        format!("{source} → epizoda {ordinal}")
    } else {
        format!("{source} → episode {ordinal}")
    }
}

pub fn tracking_history_preview(progress: u32) -> String {
    if croatian() {
        format!(
            "Spremljena povijest postavit će napredak najmanje na {progress}. Potvrdite slanje ovog ažuriranja."
        )
    } else {
        format!(
            "Saved history will set progress to at least {progress}. Confirm to send this update."
        )
    }
}

/// Cache maintenance progress and final counts, including a stopped sweep.
pub fn cache_rewrite_status(
    processed: usize,
    total: usize,
    converted: usize,
    skipped: usize,
    failed: usize,
    cancelled: bool,
    done: bool,
) -> String {
    if croatian() {
        let state = if !done {
            "Obrada"
        } else if cancelled {
            "Zaustavljeno"
        } else {
            "Dovršeno"
        };
        format!(
            "{state}: {processed}/{total} · pretvoreno {converted}, preskočeno {skipped}, neuspjelo {failed}"
        )
    } else {
        let state = if !done {
            "Processing"
        } else if cancelled {
            "Stopped"
        } else {
            "Complete"
        };
        format!(
            "{state}: {processed}/{total} · converted {converted}, skipped {skipped}, failed {failed}"
        )
    }
}

/// Shared Home and Detail action, with the episode selected by the playback policy.
pub fn watch_action(episode: &str, continuing: bool) -> String {
    let action = tr(if continuing { "Continue" } else { "Start" });
    if episode.is_empty() {
        action.into()
    } else {
        format!("{action} {episode}")
    }
}

pub fn catalog_grouping_mode(mode: nova_config::CatalogGroupingMode) -> &'static str {
    tr(match mode {
        nova_config::CatalogGroupingMode::Original => "Original",
        nova_config::CatalogGroupingMode::Season => "Season",
        nova_config::CatalogGroupingMode::Series => "Series",
    })
}

// Configured addon URLs may contain credentials. Scope comparisons intentionally
// summarize surfaces and modes without serializing any key or raw source URL.
fn catalog_grouping_override_summary(rows: &[nova_config::CatalogGroupingOverride]) -> String {
    if rows.is_empty() {
        return tr("No catalog overrides").into();
    }
    let mut counts = [[0usize; 3]; 2];
    for row in rows {
        counts[match row.key.surface {
            nova_config::CatalogSurface::Home => 0,
            nova_config::CatalogSurface::Discover => 1,
        }][row.mode.index() as usize] += 1;
    }
    let mut values = Vec::new();
    for (surface, modes) in counts.iter().enumerate() {
        for (mode, count) in modes.iter().enumerate().filter(|(_, count)| **count > 0) {
            values.push(format!(
                "{} · {} ×{count}",
                tr(if surface == 0 { "Home" } else { "Discover" }),
                catalog_grouping_mode(nova_config::CatalogGroupingMode::from_index(mode as i32))
            ));
        }
    }
    let noun = if croatian() {
        plural(
            rows.len() as u64,
            "iznimka kataloga",
            "iznimke kataloga",
            "iznimki kataloga",
        )
    } else if rows.len() == 1 {
        "catalog override"
    } else {
        "catalog overrides"
    };
    format!("{} {noun}: {}", rows.len(), values.join("; "))
}

/// Readable values for setting-scope comparisons; identifiers remain untranslated.
pub fn setting_sync_value(field: &str, value: &serde_json::Value) -> String {
    if field == "catalog_grouping_default" {
        return serde_json::from_value::<nova_config::CatalogGroupingMode>(value.clone())
            .map(|mode| catalog_grouping_mode(mode).into())
            .unwrap_or_else(|_| tr("Unsupported value").into());
    }
    if field == "catalog_grouping_overrides" {
        return serde_json::from_value::<Vec<nova_config::CatalogGroupingOverride>>(value.clone())
            .map(|rows| catalog_grouping_override_summary(&rows))
            .unwrap_or_else(|_| tr("Unsupported value").into());
    }

    if field == "stream_filters_sort_order" {
        return serde_json::from_value::<Vec<nova_config::stream_filters::StreamSortRule>>(
            value.clone(),
        )
        .map(|order| stream_sort_summary(&order))
        .unwrap_or_else(|_| tr("Unsupported value").into());
    }
    if field == "stream_filters_require_resolution" {
        return value.as_bool().map_or_else(
            || tr("Unsupported value").into(),
            |enabled| tr(if enabled { "On" } else { "Off" }).into(),
        );
    }
    if matches!(
        field,
        "stream_filters_excluded_resolutions" | "stream_filters_resolution_priority"
    ) {
        let Ok(heights) = serde_json::from_value::<Vec<u16>>(value.clone()) else {
            return tr("Unsupported value").into();
        };
        if heights.iter().any(|height| !(144..=8640).contains(height)) {
            return tr("Unsupported value").into();
        }
        return stream_resolution_list(&heights, field == "stream_filters_resolution_priority");
    }
    if matches!(field, "stream_filters_resolution" | "stream_filters_size") {
        if !value.is_null() && value.as_u64().is_none() {
            return tr("Unsupported value").into();
        }
        return value.as_u64().map_or_else(
            || {
                tr(if field == "stream_filters_resolution" {
                    "Any resolution"
                } else {
                    "Any size"
                })
                .into()
            },
            |limit| {
                if field == "stream_filters_resolution" {
                    format!("{limit}p")
                } else {
                    stream_size(limit)
                }
            },
        );
    }
    if matches!(
        field,
        "stream_filters_allowed_codecs"
            | "stream_filters_excluded_codecs"
            | "stream_filters_codec_priority"
    ) {
        let Ok(codecs) =
            serde_json::from_value::<Vec<nova_config::stream_filters::VideoCodec>>(value.clone())
        else {
            return tr("Unsupported value").into();
        };
        return if codecs.is_empty() {
            tr(match field {
                "stream_filters_allowed_codecs" => "All video codecs",
                "stream_filters_excluded_codecs" => "No excluded codecs",
                _ => "No codec preference",
            })
            .into()
        } else {
            codecs
                .into_iter()
                .map(stream_filter_codec_label)
                .collect::<Vec<_>>()
                .join(if field == "stream_filters_codec_priority" {
                    " → "
                } else {
                    ", "
                })
        };
    }
    if field == "stream_filters_audio" {
        let Some(inherit) = value
            .get("use_preferred_audio_languages")
            .and_then(serde_json::Value::as_bool)
        else {
            return tr("Unsupported value").into();
        };
        let Some(languages) = value
            .get("audio_languages")
            .and_then(serde_json::Value::as_array)
        else {
            return tr("Unsupported value").into();
        };
        let names = languages
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(nova_config::playback_preferences::language_label)
            .collect::<Vec<_>>()
            .join(" → ");
        return if inherit {
            if names.is_empty() {
                tr("Uses preferred audio languages").into()
            } else {
                format!(
                    "{} · {}: {names}",
                    tr("Uses preferred audio languages"),
                    tr("Saved custom languages")
                )
            }
        } else if names.is_empty() {
            tr("Automatic").into()
        } else {
            names
        };
    }
    if field == "stream_filters" {
        return serde_json::from_value::<nova_config::StreamFilterRules>(value.clone())
            .map(|rules| stream_filter_summary(&rules))
            .unwrap_or_else(|_| tr("Unsupported value").into());
    }
    if field == "smart_collections" {
        return serde_json::from_value::<Vec<nova_config::SmartCollection>>(value.clone())
            .map(|rules| {
                if rules.is_empty() {
                    return tr("No smart collections").into();
                }
                let names = rules
                    .iter()
                    .map(|rule| rule.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                if croatian() {
                    format!("{} zbirki · {names}", rules.len())
                } else {
                    format!("{} collections · {names}", rules.len())
                }
            })
            .unwrap_or_else(|_| tr("Unsupported value").into());
    }
    // The sync comparison must preserve the key editor's password masking.
    if field == "tmdb_api_key" {
        return tr(if value.as_str().is_none_or(|key| key.trim().is_empty()) {
            "Not configured"
        } else {
            "Configured (hidden)"
        })
        .into();
    }
    if matches!(
        field,
        "detail_poster_max_side"
            | "detail_backdrop_max_side"
            | "episode_thumbnail_max_side"
            | "player_episode_backdrop_max_side"
    ) && value.is_null()
    {
        return tr("Original").into();
    }
    if let Some(enabled) = value.as_bool() {
        return tr(if enabled { "On" } else { "Off" }).into();
    }
    let number = value
        .as_f64()
        .map(|n| n.to_string())
        .unwrap_or_else(|| value.to_string());
    match field {
        "casting_remux_buffer" => {
            serde_json::from_value::<nova_config::CastingRemuxBuffer>(value.clone())
                .map(|budget| format!("{} MiB", budget.mib()))
                .unwrap_or_else(|_| tr("Unsupported value").into())
        }
        "intro_skip_confidence" => match value.as_str() {
            Some("any") => tr("Any (0%)").into(),
            Some("low") => tr("Low (25%)").into(),
            Some("high") => tr("High (80%)").into(),
            Some("very_high") => tr("Very high (95%)").into(),
            _ => tr("Balanced (50%)").into(),
        },
        "intro_skip_mode" => match value.as_str() {
            Some("off") => tr("Off").into(),
            Some("auto") => tr("Auto skip").into(),
            _ => tr("Show skip prompt").into(),
        },
        "background_style" => match value.as_str() {
            Some("accent_tinted") => tr("Accent-tinted").into(),
            Some("true_black") => tr("True black").into(),
            _ => tr("Default").into(),
        },
        "accent_color" => match value.as_str() {
            Some("orange") => tr("Orange").into(),
            Some("blue") => tr("Blue").into(),
            Some("teal") => tr("Teal").into(),
            Some("rose") => tr("Rose").into(),
            Some("cyan") => tr("Cyan").into(),
            Some("amber") => tr("Amber").into(),
            Some("lime") => tr("Lime").into(),
            _ => tr("Purple").into(),
        },
        "hero_title_alignment" => match value.as_str() {
            Some("left") => tr("Left").into(),
            Some("right") => tr("Right").into(),
            _ => tr("Center").into(),
        },
        "home_backdrop_size" | "detail_backdrop_size" => match value.as_str() {
            Some("small") => tr("Small").into(),
            Some("large") => tr("Large").into(),
            _ => tr("Medium").into(),
        },
        "lru_cache_mb" | "torrent_max_mb" => format!("{number} MB"),
        "torrent_ram_buffer_mb" => format!("{number} MiB"),
        "detail_poster_max_side"
        | "detail_backdrop_max_side"
        | "episode_thumbnail_max_side"
        | "player_episode_backdrop_max_side" => format!("{number} px"),
        "quality"
        | "screenshot_jpeg_quality"
        | "screenshot_webp_quality"
        | "status_bar_gradient" => format!("{}%", value),
        "card_corner_radius" => match value.as_u64() {
            Some(0) => tr("Square").into(),
            Some(4) => tr("Subtle").into(),
            Some(10) => tr("Soft").into(),
            Some(16) => tr("Rounded").into(),
            Some(24) => tr("Extra rounded").into(),
            _ => format!("{number} px"),
        },
        "button_corner_radius" => match value.as_u64() {
            Some(0) => tr("Square").into(),
            Some(4) => tr("Subtle").into(),
            Some(8) => tr("Soft").into(),
            Some(12) => tr("Rounded").into(),
            Some(24) => tr("Extra rounded").into(),
            _ => format!("{number} px"),
        },
        "card_spacing" => match value.as_u64() {
            Some(0) => tr("None").into(),
            Some(4) => tr("Tight").into(),
            Some(8) => tr("Compact").into(),
            Some(16) => tr("Comfortable").into(),
            Some(32) => tr("Spacious").into(),
            _ => format!("{number} px"),
        },
        "screenshot_format" => match value.as_str() {
            Some("png") => tr("PNG (lossless)").into(),
            Some("jpeg") => "JPEG".into(),
            Some("webp_lossless") => tr("WebP (lossless)").into(),
            Some("webp_lossy") => tr("WebP (lossy)").into(),
            _ => value.to_string(),
        },
        "subtitle_style" => {
            let Ok(style) = serde_json::from_value::<
                nova_config::playback_preferences::SubtitleStyle,
            >(value.clone()) else {
                return value.to_string();
            };
            let style = style.normalized();
            let native = if croatian() { "izvorno" } else { "native" };
            let font_size = style
                .font_size
                .map_or_else(|| native.into(), |v| format!("{v} px"));
            let outline = style
                .outline
                .map_or_else(|| native.into(), |v| format!("{v} px"));
            let color = if croatian() {
                match style.color {
                    1 => "žuta",
                    2 => "cijan",
                    _ => "bijela",
                }
            } else {
                match style.color {
                    1 => "yellow",
                    2 => "cyan",
                    _ => "white",
                }
            };
            let embedded = if croatian() {
                if style.override_ass {
                    "zamjena stilova"
                } else {
                    "ugrađeni stilovi"
                }
            } else if style.override_ass {
                "override styles"
            } else {
                "embedded styles"
            };
            if croatian() {
                format!(
                    "Pomak {:+.1} s · veličina {} · margine {} px · položaj {}% · obrub {} · neprozirnost pozadine {}% · {color} · {embedded}",
                    style.delay,
                    font_size,
                    style.margin,
                    style.position,
                    outline,
                    style.background_opacity
                )
            } else {
                format!(
                    "Timing {:+.1} s · size {} · margins {} px · position {}% · outline {} · background opacity {}% · {color} · {embedded}",
                    style.delay,
                    font_size,
                    style.margin,
                    style.position,
                    outline,
                    style.background_opacity
                )
            }
        }
        "playback_speed" => format!("{}×", value),
        "default_audio_language" if value.as_str() == Some("") => tr("Automatic").into(),
        "default_subtitle_language" if value.as_str() == Some("") => tr("Automatic").into(),
        "default_subtitle_language" if value.as_str() == Some("off") => tr("Off").into(),
        "default_audio_language" | "default_subtitle_language" => value
            .as_str()
            .map(|value| {
                nova_config::playback_preferences::language_preferences(value)
                    .iter()
                    .map(|code| nova_config::playback_preferences::language_label(code))
                    .collect::<Vec<_>>()
                    .join(" → ")
            })
            .unwrap_or_else(|| value.to_string()),
        "image_disk_cache_limit_mb" if value.is_null() => tr("Unlimited").into(),
        "image_disk_cache_limit_mb" => format!("{number} MiB"),
        "torrent_down_limit" if value.as_f64() == Some(0.0) => tr("Unlimited").into(),
        "torrent_down_limit" => format!("{number} KB/s"),
        "home_catalog_sources"
        | "home_row_sources"
        | "home_rows"
        | "home_recommendations_hidden" => {
            let count = value.as_array().map_or(0, Vec::len);
            let word = if croatian() {
                plural(count as u64, "katalog", "kataloga", "kataloga")
            } else if count == 1 {
                "catalog"
            } else {
                "catalogs"
            };
            format!("{count} {word}")
        }
        "torrent_dir" if value.as_str() == Some("") => tr("Default (app cache folder)").into(),
        "sync_interval" => match value.as_i64().unwrap_or(1) {
            0 => "30 s",
            1 => tr("Every minute"),
            2 => tr("Every 5 minutes"),
            _ => tr("Every 15 minutes"),
        }
        .into(),
        _ => value
            .as_str()
            .map(|s| {
                match s {
                    "jpeg" => "JPEG",
                    "webp" => "WebP",
                    "en" | "English" => "English",
                    "hr" | "Croatian" => "Hrvatski",
                    "Resume" | "resume" => tr("Resume"),
                    "StartOver" | "start_over" => tr("Start over"),
                    "Ask" | "ask" => tr("Ask"),
                    "system" | "system_default" => tr("System default"),
                    "vlc" => "VLC",
                    "mpv" => "mpv",
                    "hw+" | "hw_plus" => "HW+",
                    "hw" => "HW",
                    "sw" => "SW",
                    _ => s,
                }
                .into()
            })
            .unwrap_or_else(|| value.to_string()),
    }
}

/// Released episodes awaiting an explicit return to viewing.
pub fn new_episodes(count: usize) -> String {
    if croatian() {
        format!(
            "{count} {}",
            plural(
                count as u64,
                "nova epizoda",
                "nove epizode",
                "novih epizoda"
            )
        )
    } else {
        format!(
            "{count} new {}",
            if count == 1 { "episode" } else { "episodes" }
        )
    }
}

/// Release planner day heading, using the active UI language.
pub fn calendar_date(day: u32, month: u32, year: i64) -> String {
    if croatian() {
        format!("{day}. {} {year}.", month_name(month))
    } else {
        format!("{} {day}, {year}", month_name(month))
    }
}

/// A downloaded stack contains episodes, or local files without episode metadata.
pub fn downloads_item_count(count: usize, episodes: bool) -> String {
    let noun = if croatian() {
        if episodes {
            plural(count as u64, "epizoda", "epizode", "epizoda")
        } else {
            plural(count as u64, "datoteka", "datoteke", "datoteka")
        }
    } else if episodes {
        if count == 1 { "episode" } else { "episodes" }
    } else if count == 1 {
        "file"
    } else {
        "files"
    };
    format!("{count} {noun}")
}

pub fn downloads_summary(files: usize, series: usize, bytes: &str) -> String {
    let downloads = if croatian() {
        plural(files as u64, "preuzimanje", "preuzimanja", "preuzimanja")
    } else if files == 1 {
        "download"
    } else {
        "downloads"
    };
    let shows = if croatian() {
        plural(series as u64, "serija", "serije", "serija")
    } else {
        "series"
    };
    format!("{files} {downloads} · {series} {shows} · {bytes}")
}

/// Compact release-planner range, avoiding repeated months and years.
pub fn calendar_week_title(first: (i64, u32, u32), last: (i64, u32, u32)) -> String {
    let (year, month, day) = first;
    let (end_year, end_month, end_day) = last;
    if year != end_year {
        return format!(
            "{} – {}",
            absolute_date(day, month, year),
            absolute_date(end_day, end_month, end_year)
        );
    }
    let name = month_abbreviation(month);
    let end_name = month_abbreviation(end_month);
    if croatian() {
        if month == end_month {
            format!("{day}.–{end_day}. {name} {year}.")
        } else {
            format!("{day}. {name} – {end_day}. {end_name} {year}.")
        }
    } else if month == end_month {
        format!("{name} {day}–{end_day}, {year}")
    } else {
        format!("{name} {day} – {end_name} {end_day}, {year}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn torrent_ram_status_reports_memory_and_localized_peers() {
        with_language(Language::English, || {
            assert_eq!(
                torrent_ram_buffer(12 * 1024 * 1024, 64 * 1024 * 1024, 1024 * 1024, 4),
                "RAM 12 / 64 MiB · ↓ 1.0 MiB/s · 4 peers"
            );
            assert_eq!(
                setting_sync_value("torrent_ram_buffer_mb", &serde_json::json!(128)),
                "128 MiB"
            );
        });
        with_language(Language::Croatian, || {
            assert_eq!(
                torrent_ram_buffer(0, 32 * 1024 * 1024, 0, 1),
                "RAM 0 / 32 MiB · ↓ 0.0 MiB/s · 1 čvor"
            );
        });
    }

    #[test]
    fn resolution_scope_values_keep_exact_order_validate_shapes_and_translate_empty_lists() {
        with_language(Language::English, || {
            assert_eq!(
                setting_sync_value(
                    "stream_filters_excluded_resolutions",
                    &serde_json::json!([240, 800, 8640])
                ),
                "240p, 800p, 8640p"
            );
            assert_eq!(
                setting_sync_value(
                    "stream_filters_resolution_priority",
                    &serde_json::json!([1080, 720, 144])
                ),
                "1080p → 720p → 144p"
            );
            for field in [
                "stream_filters_excluded_resolutions",
                "stream_filters_resolution_priority",
            ] {
                for invalid in [
                    serde_json::Value::Null,
                    serde_json::json!({}),
                    serde_json::json!("1080p"),
                    serde_json::json!(["1080"]),
                    serde_json::json!([800.5]),
                    serde_json::json!([143]),
                    serde_json::json!([8641]),
                    serde_json::json!([-1]),
                    serde_json::json!([null]),
                ] {
                    assert_eq!(
                        setting_sync_value(field, &invalid),
                        "Unsupported value",
                        "{field}: {invalid}"
                    );
                }
            }
            assert_eq!(
                setting_sync_value(
                    "stream_filters_excluded_resolutions",
                    &serde_json::json!([])
                ),
                "No excluded resolutions"
            );
            assert_eq!(
                setting_sync_value("stream_filters_resolution_priority", &serde_json::json!([])),
                "No resolution preference"
            );
            assert_eq!(
                setting_sync_value(
                    "stream_filters_require_resolution",
                    &serde_json::json!(true)
                ),
                "On"
            );
            assert_eq!(
                setting_sync_value(
                    "stream_filters_require_resolution",
                    &serde_json::json!(false)
                ),
                "Off"
            );
            for invalid in [
                serde_json::Value::Null,
                serde_json::json!("true"),
                serde_json::json!(1),
            ] {
                assert_eq!(
                    setting_sync_value("stream_filters_require_resolution", &invalid),
                    "Unsupported value"
                );
            }
        });
        with_language(Language::Croatian, || {
            assert_eq!(
                setting_sync_value(
                    "stream_filters_excluded_resolutions",
                    &serde_json::json!([])
                ),
                "Nema izuzetih rezolucija"
            );
            assert_eq!(
                setting_sync_value("stream_filters_resolution_priority", &serde_json::json!([])),
                "Nema prednosti rezolucije"
            );
            assert_eq!(
                setting_sync_value(
                    "stream_filters_resolution_priority",
                    &serde_json::json!([800, 240])
                ),
                "800p → 240p"
            );
            assert_eq!(
                setting_sync_value(
                    "stream_filters_require_resolution",
                    &serde_json::json!(true)
                ),
                "Uključeno"
            );
        });
    }

    #[test]
    fn resolution_summary_shows_blocks_priority_strictness_and_unknown_policy_in_both_languages() {
        let mut rules = nova_config::StreamFilterRules {
            enabled: true,
            max_height: Some(2160),
            excluded_resolutions: vec![240, 720],
            resolution_priority: vec![1080, 800, 8640],
            require_preferred_resolution: true,
            retain_unknown: false,
            ..Default::default()
        };
        with_language(Language::English, || {
            let summary = stream_filter_summary(&rules);
            for expected in [
                "≤ 2160p",
                "Excluded resolutions: 240p, 720p",
                "Preferred resolutions: 1080p → 800p → 8640p",
                "Only preferred resolutions",
                "Unknown details hidden",
            ] {
                assert!(summary.contains(expected), "{summary}");
            }
            rules.resolution_priority = nova_config::stream_filters::RESOLUTION_CHOICES.to_vec();
            let summary = stream_filter_summary(&rules);
            assert!(summary.contains(
                "240p → 360p → 480p → 576p → 720p → 1080p → 1440p → 2160p → 4320p → 8640p"
            ));
            rules.resolution_priority.clear();
            assert!(
                stream_filter_summary(&rules)
                    .contains("Preferred resolutions: No resolution preference")
            );
        });
        with_language(Language::Croatian, || {
            let summary = stream_filter_summary(&rules);
            for expected in [
                "Izuzete rezolucije: 240p, 720p",
                "Željene rezolucije: Nema prednosti rezolucije",
                "Samo željene rezolucije",
                "Nepoznate pojedinosti skrivene",
            ] {
                assert!(summary.contains(expected), "{summary}");
            }
            rules.enabled = false;
            assert_eq!(stream_filter_summary(&rules), tr("Disabled"));
        });
    }

    #[test]
    fn stream_rule_comparisons_show_independent_values_and_order() {
        with_language(Language::English, || {
            assert_eq!(
                setting_sync_value("stream_filters_resolution", &serde_json::Value::Null),
                "Any resolution"
            );
            assert_eq!(
                setting_sync_value("stream_filters_resolution", &serde_json::json!(1080)),
                "1080p"
            );
            assert_eq!(
                setting_sync_value("stream_filters_resolution", &serde_json::json!("future")),
                "Unsupported value"
            );
            assert_eq!(
                setting_sync_value(
                    "stream_filters_codec_priority",
                    &serde_json::json!(["av1", "hevc"])
                ),
                "AV1 → HEVC"
            );
            assert_eq!(
                setting_sync_value("stream_filters_excluded_codecs", &serde_json::json!([])),
                "No excluded codecs"
            );
            assert_eq!(
                setting_sync_value(
                    "stream_filters_audio",
                    &serde_json::json!({"use_preferred_audio_languages": true, "audio_languages": []})
                ),
                "Uses preferred audio languages"
            );
            assert_eq!(
                setting_sync_value("stream_filters_require_codec", &serde_json::json!(true)),
                "On"
            );
            assert_eq!(
                setting_sync_value(
                    "stream_filters_sort_order",
                    &serde_json::json!([
                        {"criterion":"codec","direction":"desc"},
                        {"criterion":"size","direction":"asc"},
                        {"criterion":"subtitle","direction":"asc"}
                    ])
                ),
                "Video codec: Reverse preference order → Size: Smallest first → Subtitle language: Preferred first"
            );
        });
        with_language(Language::Croatian, || {
            assert_eq!(
                setting_sync_value("stream_filters_allowed_codecs", &serde_json::json!([])),
                "Svi videokodeci"
            );
            assert_eq!(
                setting_sync_value(
                    "stream_filters_audio",
                    &serde_json::json!({"use_preferred_audio_languages": true, "audio_languages": []})
                ),
                "Koristi željene jezike zvuka"
            );
        });
    }

    #[test]
    fn subtitle_style_scope_summary_shows_legacy_default_and_explicit_background_opacity() {
        for (language, label) in [
            (Language::English, "background opacity"),
            (Language::Croatian, "neprozirnost pozadine"),
        ] {
            with_language(language, || {
                for (value, opacity) in [
                    (serde_json::json!({"delay": 0.2}), 0),
                    (serde_json::json!({"background_opacity": 0}), 0),
                    (serde_json::json!({"background_opacity": 100}), 100),
                ] {
                    let summary = setting_sync_value("subtitle_style", &value);
                    assert!(
                        summary.contains(&format!("{label} {opacity}%")),
                        "{summary}"
                    );
                }
            });
        }
    }

    #[test]
    fn inferred_catalog_grouping_scope_values_and_label_are_translated() {
        with_language(Language::English, || {
            assert_eq!(tr("Infer series grouping"), "Infer series grouping");
            assert_eq!(
                setting_sync_value("catalog_grouping_infer_series", &serde_json::json!(true)),
                "On"
            );
            assert_eq!(
                setting_sync_value("catalog_grouping_infer_series", &serde_json::json!(false)),
                "Off"
            );
        });
        with_language(Language::Croatian, || {
            assert_eq!(tr("Infer series grouping"), "Izvedi grupiranje serija");
            assert_eq!(tr("Inferred grouping"), "Izvedeno grupiranje");
            assert_eq!(
                setting_sync_value("catalog_grouping_infer_series", &serde_json::json!(true)),
                "Uključeno"
            );
            assert_eq!(
                setting_sync_value("catalog_grouping_infer_series", &serde_json::json!(false)),
                "Isključeno"
            );
        });
    }

    #[test]
    fn catalog_scope_values_are_translated_and_never_expose_configured_urls() {
        let rows = serde_json::json!([{"addonUrl":"https://secret.example/token/credential", "type":"series", "catalogId":"private-catalog-id", "surface":"home", "mode":"season"}]);
        with_language(Language::English, || {
            assert_eq!(
                setting_sync_value("catalog_grouping_default", &serde_json::json!("series")),
                "Series"
            );
            let summary = setting_sync_value("catalog_grouping_overrides", &rows);
            assert!(summary.contains("Home") && summary.contains("Season"));
            for secret in [
                "secret.example",
                "token",
                "credential",
                "private-catalog-id",
                "https:",
            ] {
                assert!(!summary.contains(secret));
            }
            assert_eq!(
                setting_sync_value("catalog_grouping_overrides", &serde_json::json!([])),
                "No catalog overrides"
            );
            assert_eq!(
                setting_sync_value("catalog_grouping_default", &serde_json::json!("future")),
                "Unsupported value"
            );
        });
        with_language(Language::Croatian, || {
            assert_eq!(
                setting_sync_value("catalog_grouping_default", &serde_json::json!("season")),
                "Sezona"
            )
        });
    }

    #[test]
    fn watch_history_labels_follow_the_selected_language() {
        with_language(Language::English, || {
            assert_eq!(tr("Watch history"), "Watch history");
            assert_eq!(tr("Because you watched"), "Because you watched");
            assert_eq!(tr("Resume"), "Resume");
            assert_eq!(tr("Play again"), "Play again");
        });
        with_language(Language::Croatian, || {
            assert_eq!(tr("Watch history"), "Povijest gledanja");
            assert_eq!(tr("Because you watched"), "Zato što ste gledali");
            assert_eq!(tr("Resume"), "Nastavite");
            assert_eq!(tr("Play again"), "Reproducirajte ponovno");
        });
    }

    #[test]
    fn setting_comparisons_translate_values_and_format_units() {
        for (language, labels) in [
            (
                Language::English,
                [
                    "Any (0%)",
                    "Low (25%)",
                    "Balanced (50%)",
                    "High (80%)",
                    "Very high (95%)",
                ],
            ),
            (
                Language::Croatian,
                [
                    "Bilo koja (0%)",
                    "Niska (25%)",
                    "Uravnotežena (50%)",
                    "Visoka (80%)",
                    "Vrlo visoka (95%)",
                ],
            ),
        ] {
            with_language(language, || {
                for (preset, label) in ["any", "low", "balanced", "high", "very_high"]
                    .into_iter()
                    .zip(labels)
                {
                    assert_eq!(
                        setting_sync_value("intro_skip_confidence", &serde_json::json!(preset)),
                        label
                    );
                }
            });
        }
        for (language, labels) in [
            (Language::English, ["Off", "Show skip prompt", "Auto skip"]),
            (
                Language::Croatian,
                [
                    "Isključeno",
                    "Prikaži gumb za preskakanje",
                    "Automatski preskoči",
                ],
            ),
        ] {
            with_language(language, || {
                for (mode, label) in ["off", "prompt", "auto"].into_iter().zip(labels) {
                    assert_eq!(
                        setting_sync_value("intro_skip_mode", &serde_json::json!(mode)),
                        label
                    );
                }
            });
        }
        with_language(Language::English, || {
            for mib in [32, 64, 128, 256] {
                assert_eq!(
                    setting_sync_value(
                        "casting_remux_buffer",
                        &serde_json::json!(format!("{mib}_mib"))
                    ),
                    format!("{mib} MiB")
                );
            }
            assert_eq!(
                setting_sync_value("casting_remux_buffer", &serde_json::json!("512_mib")),
                "Unsupported value"
            );
            assert_eq!(setting_sync_value("quality", &serde_json::json!(60)), "60%");
            assert_eq!(
                setting_sync_value("language", &serde_json::json!("Croatian")),
                "Hrvatski"
            );
            assert_eq!(
                setting_sync_value("language", &serde_json::json!("English")),
                "English"
            );
            assert_eq!(
                setting_sync_value("torrent_max_mb", &serde_json::json!(20480.0)),
                "20480 MB"
            );
            assert_eq!(
                setting_sync_value("episode_start_behavior", &serde_json::json!("StartOver")),
                "Start over"
            );
            assert_eq!(
                setting_sync_value("default_audio_language", &serde_json::json!("")),
                "Automatic"
            );
            assert_eq!(
                setting_sync_value("default_subtitle_language", &serde_json::json!("off")),
                "Off"
            );
            assert_eq!(
                setting_sync_value("default_audio_language", &serde_json::json!("en")),
                "English (en)"
            );
            assert_eq!(
                setting_sync_value("default_audio_language", &serde_json::json!("ja,en")),
                "Japanese (ja) → English (en)"
            );
        });
        with_language(Language::Croatian, || {
            assert_eq!(
                setting_sync_value("animations", &serde_json::json!(false)),
                "Isključeno"
            );
            assert_eq!(
                setting_sync_value("default_audio_language", &serde_json::json!("")),
                "Automatski"
            );
            assert_eq!(
                setting_sync_value("default_subtitle_language", &serde_json::json!("off")),
                "Isključeno"
            );
            assert_eq!(
                setting_sync_value("default_subtitle_language", &serde_json::json!("hr")),
                "Croatian (hr)"
            );
            assert_eq!(tr("Not received yet"), "Još nije primljeno");
        });
    }

    #[test]
    fn completion_release_counts_use_localized_plural_forms() {
        with_language(Language::English, || {
            assert_eq!(completion_release_subtitle(1), "Mapped release · 1 episode");
            assert_eq!(
                completion_release_subtitle(24),
                "Mapped release · 24 episodes"
            );
        });
        with_language(Language::Croatian, || {
            assert_eq!(
                completion_release_subtitle(1),
                "Povezano izdanje · 1 epizoda"
            );
            assert_eq!(
                completion_release_subtitle(24),
                "Povezano izdanje · 24 epizode"
            );
            assert_eq!(
                completion_release_subtitle(12),
                "Povezano izdanje · 12 epizoda"
            );
        });
    }

    #[test]
    fn tracking_ranges_preserve_gaps_and_summary_hides_zero_warning() {
        assert_eq!(
            tracking_episode_ranges(&[8, 1, 2, 2, 4, 9, 10]),
            "1–2, 4, 8–10"
        );
        assert_eq!(tracking_episode_ranges(&[]), "");
        assert_eq!(tracking_episode_ranges(&[u32::MAX]), u32::MAX.to_string());
        with_language(Language::English, || {
            assert_eq!(tracking_setup_unmapped(0), "");
            assert_eq!(tracking_setup_unmapped(11), "Unmapped episodes: 11");
            assert_eq!(
                tracking_setup_summary(2, 13),
                "Releases: 2 · Episodes to link: 13"
            );
        });
        with_language(Language::Croatian, || {
            assert_eq!(tracking_setup_unmapped(11), "Nepovezane epizode: 11");
        });
    }

    #[test]
    fn croatian_translates_and_english_is_the_source() {
        with_language(Language::English, || {
            assert_eq!(tr("Queued"), "Queued");
            assert_eq!(watch_action("S1 E1", false), "Start S1 E1");
            assert_eq!(watch_action("S1 E4", true), "Continue S1 E4");
            assert_eq!(
                setting_sync_value("card_spacing", &serde_json::json!(8)),
                "Compact"
            );
            assert_eq!(downloading_files(3), "Downloading 3 file(s)");
            assert_eq!(season_label(2), "Season 2");
            assert_eq!(files_label(1), "1 file");
            assert_eq!(relative_days(3), "3 days ago");
            assert_eq!(absolute_date(15, 3, 2024), "Mar 15, 2024");
        });
        with_language(Language::Croatian, || {
            assert_eq!(tr("Queued"), "Čeka");
            assert_eq!(watch_action("S1 E1", false), "Počnite S1 E1");
            assert_eq!(watch_action("S1 E4", true), "Nastavite S1 E4");
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

/// Preview copy keeps the initial merge approval separate from later syncs.
pub fn stremio_preview(titles: usize) -> String {
    if croatian() {
        format!("Spojit će se {titles} naslova. Već pogledane epizode bit će sačuvane.")
    } else {
        format!("{titles} titles will be merged. Existing watched episodes will be kept.")
    }
}

pub fn list_services_pending(titles: usize) -> String {
    if croatian() {
        format!("Neki naslovi čekaju potvrđene identitete ili podatke o epizodama ({titles}).")
    } else {
        format!("Some titles need confirmed identities or episode metadata ({titles}).")
    }
}

pub fn stremio_http_error(status: u16) -> String {
    if croatian() {
        format!("Stremio je vratio HTTP {status}. Promjene će se ponovno pokušati poslati.")
    } else {
        format!("Stremio returned HTTP {status}. Changes will retry.")
    }
}

pub fn stremio_api_error(code: Option<u64>) -> String {
    match (croatian(), code) {
        (true, Some(code)) => format!(
            "Stremio je odbio zahtjev (kôd {code}). Promjene će se ponovno pokušati poslati."
        ),
        (false, Some(code)) => {
            format!("Stremio rejected the request (code {code}). Changes will retry.")
        }
        (true, None) => "Stremio je odbio zahtjev. Promjene će se ponovno pokušati poslati.".into(),
        (false, None) => "Stremio rejected the request. Changes will retry.".into(),
    }
}

pub fn stremio_failure(message: &str, phase: &str) -> String {
    if phase.is_empty() {
        message.to_owned()
    } else {
        format!("{}\n{}", tr(phase), message)
    }
}

/// IDs aid diagnosis, but opaque source IDs can contain configured URLs. Show
/// ordinary catalog/provider identifiers only, never URL/query credentials.
pub fn stremio_pending_title(name: &str, media_type: &str, id: &str, reason: &str) -> String {
    let safe = |value: &str| {
        !value.is_empty()
            && value.len() <= 256
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b":._-".contains(&byte))
    };
    let identity = if safe(media_type) && safe(id) {
        format!("{media_type} · {id}")
    } else {
        tr("Source identifier omitted").into()
    };
    format!("{name}\n{}\n{identity}", tr(reason))
}

/// The code is public and short lived; access/refresh tokens never reach UI text.
pub fn trakt_device_code(uri: &str, code: &str) -> String {
    if croatian() {
        format!("Otvorite {uri} i unesite kôd {code}")
    } else {
        format!("Visit {uri} and enter code {code}")
    }
}

/// Listing-only subtotal; unknown episodes prevent a definite remaining-space claim.
pub fn season_download_summary(known: u64, unknown: usize, free: Option<u64>) -> String {
    let size = nova_download_size(known);
    let mut summary = if croatian() {
        format!("Procijenjena poznata veličina: ~{size} · Nepoznate veličine: {unknown}")
    } else {
        format!("Estimated known size: ~{size} · Unknown sizes: {unknown}")
    };
    if let Some(free) = free {
        let available = nova_download_size(free);
        summary.push_str(&if croatian() {
            format!("\nSlobodan prostor: {available}")
        } else {
            format!("\nFree space: {available}")
        });
        if known > free {
            summary.push_str(if croatian() {
                " · Nedovoljno prostora"
            } else {
                " · Not enough space"
            });
        }
        if unknown == 0
            && let Some(remaining) = free.checked_sub(known)
        {
            let remaining = nova_download_size(remaining);
            summary.push_str(&if croatian() {
                format!(" · Procijenjeno preostalo: {remaining}")
            } else {
                format!(" · Projected remaining: {remaining}")
            });
        }
    } else {
        summary.push_str(if croatian() {
            "\nSlobodan prostor nije dostupan"
        } else {
            "\nFree space unavailable"
        });
    }
    summary
}
fn nova_download_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < 4 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Active viewing time as an unambiguous clock, without language-specific units.
pub fn journal_duration(seconds: f64) -> String {
    let seconds = seconds.max(0.0) as u64;
    format!(
        "{}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

pub fn watch_queue_summary(movies: usize, episodes: usize) -> String {
    if croatian() {
        let films = plural(movies as u64, "film", "filma", "filmova");
        let eps = plural(episodes as u64, "epizoda", "epizode", "epizoda");
        format!("{movies} {films} · {episodes} {eps}")
    } else {
        format!(
            "{movies} {} · {episodes} {}",
            if movies == 1 { "movie" } else { "movies" },
            if episodes == 1 { "episode" } else { "episodes" }
        )
    }
}

pub fn sync_inspector_count(label: &str, count: usize) -> String {
    format!("{}: {count}", tr(label))
}

pub fn sync_inspector_clock(label: &str, ts: u64, counter: u32, device: u64) -> String {
    format!("{}: {ts}:{counter} · {device:016x}", tr(label))
}

pub fn sync_inspector_exchange(at: &str, newer: usize) -> String {
    if croatian() {
        format!("Sat snimke posljednje dovršene razmjene: {at} · {newer} zapisa novijih od snimke")
    } else {
        format!(
            "Last completed exchange snapshot clock: {at} · {newer} records newer than its snapshot"
        )
    }
}

pub fn sync_inspector_inventory(live: usize, deleted: usize, pending: bool) -> String {
    let mut value = if croatian() {
        format!("{live} trenutačnih · {deleted} sačuvanih brisanja")
    } else {
        format!("{live} current · {deleted} deletions retained")
    };
    if pending {
        value.push_str(if croatian() {
            " · čeka primjenu na ovom uređaju"
        } else {
            " · waiting to apply on this device"
        });
    }
    value
}

pub fn stream_rules_hidden(count: usize) -> String {
    if croatian() {
        format!(
            "{count} {}",
            plural(
                count as u64,
                "izvor skriven vašim pravilima",
                "izvora skrivena vašim pravilima",
                "izvora skrivenih vašim pravilima"
            )
        )
    } else {
        format!(
            "{count} {} hidden by your rules",
            if count == 1 { "stream" } else { "streams" }
        )
    }
}

fn stream_filter_codec_label(codec: nova_config::stream_filters::VideoCodec) -> &'static str {
    use nova_config::stream_filters::VideoCodec;
    match codec {
        VideoCodec::Avc => "H.264",
        VideoCodec::Hevc => "HEVC",
        VideoCodec::Av1 => "AV1",
        VideoCodec::Vp9 => "VP9",
        VideoCodec::Other => tr("Other video codecs"),
    }
}

fn stream_sort_summary(order: &[nova_config::stream_filters::StreamSortRule]) -> String {
    use nova_config::stream_filters::{SortCriterion, SortDirection};
    order
        .iter()
        .map(|rule| {
            let criterion = tr(match rule.criterion {
                SortCriterion::Cached => "Cached availability",
                SortCriterion::Resolution => "Resolution",
                SortCriterion::Size => "Size",
                SortCriterion::SeasonPack => "Season pack",
                SortCriterion::Seeders => "Reported seeders",
                SortCriterion::Audio => "Audio language",
                SortCriterion::Codec => "Video codec",
                SortCriterion::Subtitle => "Subtitle language",
            });
            let ascending = rule.direction == SortDirection::Asc;
            let direction = tr(match rule.criterion {
                SortCriterion::Audio | SortCriterion::Codec | SortCriterion::Subtitle => {
                    if ascending {
                        "Preferred first"
                    } else {
                        "Reverse preference order"
                    }
                }
                SortCriterion::Cached => {
                    if ascending {
                        "Uncached first"
                    } else {
                        "Cached first"
                    }
                }
                SortCriterion::SeasonPack => {
                    if ascending {
                        "Single episodes first"
                    } else {
                        "Season packs first"
                    }
                }
                SortCriterion::Size => {
                    if ascending {
                        "Smallest first"
                    } else {
                        "Largest first"
                    }
                }
                SortCriterion::Resolution => {
                    if ascending {
                        "Lowest first"
                    } else {
                        "Highest first"
                    }
                }
                SortCriterion::Seeders => {
                    if ascending {
                        "Fewest first"
                    } else {
                        "Most first"
                    }
                }
            });
            format!("{criterion}: {direction}")
        })
        .collect::<Vec<_>>()
        .join(" → ")
}

fn stream_resolution_list(heights: &[u16], preferred: bool) -> String {
    if heights.is_empty() {
        return tr(if preferred {
            "No resolution preference"
        } else {
            "No excluded resolutions"
        })
        .into();
    }
    heights
        .iter()
        .map(|height| format!("{height}p"))
        .collect::<Vec<_>>()
        .join(if preferred { " → " } else { ", " })
}

pub fn stream_filter_summary(rules: &nova_config::StreamFilterRules) -> String {
    if !rules.enabled {
        return tr("Disabled").into();
    }
    let mut parts = Vec::new();
    if let Some(height) = rules.max_height {
        parts.push(format!("≤ {height}p"));
    }
    if !rules.excluded_resolutions.is_empty() {
        parts.push(format!(
            "{}: {}",
            tr("Excluded resolutions"),
            stream_resolution_list(&rules.excluded_resolutions, false)
        ));
    }
    if !rules.resolution_priority.is_empty() || rules.require_preferred_resolution {
        parts.push(format!(
            "{}: {}",
            tr("Preferred resolutions"),
            stream_resolution_list(&rules.resolution_priority, true)
        ));
    }
    if rules.require_preferred_resolution {
        parts.push(tr("Only preferred resolutions").into());
    }
    if let Some(bytes) = rules.max_bytes {
        parts.push(format!("≤ {}", stream_size(bytes)));
    }
    if !rules.video_codecs.is_empty() {
        parts.push(
            rules
                .video_codecs
                .iter()
                .copied()
                .map(stream_filter_codec_label)
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    if !rules.excluded_video_codecs.is_empty() {
        parts.push(format!(
            "{}: {}",
            tr("Excluded video codecs"),
            rules
                .excluded_video_codecs
                .iter()
                .copied()
                .map(stream_filter_codec_label)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !rules.codec_priority.is_empty() {
        parts.push(format!(
            "{}: {}",
            tr("Codec priority"),
            rules
                .codec_priority
                .iter()
                .copied()
                .map(stream_filter_codec_label)
                .collect::<Vec<_>>()
                .join(" → ")
        ));
    }
    if rules.require_preferred_codec {
        parts.push(tr("Only preferred video codecs").into());
    }
    if rules.use_preferred_audio_languages {
        parts.push(tr("Uses preferred audio languages").into());
        if rules.require_audio_language {
            parts.push(tr("Required audio").into());
        }
    } else if !rules.audio_languages.is_empty() {
        let names = rules
            .audio_languages
            .iter()
            .map(|code| nova_config::playback_preferences::language_label(code))
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!(
            "{}: {names}",
            tr(if rules.require_audio_language {
                "Required audio"
            } else {
                "Preferred audio"
            })
        ));
    }
    if !rules.subtitle_languages.is_empty() {
        let names = rules
            .subtitle_languages
            .iter()
            .map(|code| nova_config::playback_preferences::language_label(code))
            .collect::<Vec<_>>()
            .join(" → ");
        parts.push(format!("{}: {names}", tr("Preferred subtitle languages")));
    }
    parts.push(
        tr(if rules.retain_unknown {
            "Unknown details kept"
        } else {
            "Unknown details hidden"
        })
        .into(),
    );
    parts.push(format!(
        "{}: {}",
        tr("Sort order"),
        stream_sort_summary(&rules.sort_order)
    ));
    parts.join(" · ")
}

pub fn bookmarks_count(count: usize) -> String {
    if croatian() {
        format!(
            "{count} {}",
            plural(count as u64, "oznaka", "oznake", "oznaka")
        )
    } else {
        format!(
            "{count} {}",
            if count == 1 { "bookmark" } else { "bookmarks" }
        )
    }
}

/// Journal dates are explicitly UTC, matching the recap's calendar boundaries.
pub fn journal_date(at: u64, with_time: bool) -> String {
    // Gregorian civil date from Unix days (Howard Hinnant's algorithm).
    let days = (at / 86400).min(i64::MAX as u64 - 719468) as i64 + 719468;
    let era = days / 146097;
    let day_of_era = days - era * 146097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    if with_time {
        format!(
            "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
            at / 3600 % 24,
            at / 60 % 60
        )
    } else {
        format!("{year:04}-{month:02}-{day:02}")
    }
}

/// Receiver-sampled countdown; controllers never invent a deadline.
pub fn sleep_timer_status(mode: u8, seconds: u32, episodes: u8) -> String {
    match mode {
        1 if croatian() => format!("Spavanje za {}:{:02}", seconds / 60, seconds % 60),
        1 => format!("Sleep in {}:{:02}", seconds / 60, seconds % 60),
        2 if croatian() => "Spavanje na kraju sadržaja".into(),
        2 => "Sleep at end of item".into(),
        _ if croatian() => format!("Preostale epizode: {episodes}"),
        _ => format!("Episodes remaining: {episodes}"),
    }
}

/// Device accounting uses GiB consistently; reservations remain visibly separate.
pub fn smart_offline_storage(used: u64, reserved: u64, manual: u64, budget: u64) -> String {
    let gib = 1024.0 * 1024.0 * 1024.0;
    let mut label = format!(
        "{}: {:.2} / {:.0} GiB · {}: {:.2} GiB · {}: {:.2} GiB",
        tr("Automatic storage"),
        used as f64 / gib,
        budget as f64 / gib,
        tr("Reserved"),
        reserved as f64 / gib,
        tr("Manually kept"),
        manual as f64 / gib
    );
    if used > budget {
        label.push_str(&format!(
            " · {}: {:.2} GiB",
            tr("Over allowance"),
            (used - budget) as f64 / gib
        ));
    }
    label
}
pub fn smart_offline_status(ready: usize, active: usize, message: &str) -> String {
    format!(
        "{ready} {} · {active} {} · {}",
        tr("ready"),
        tr("downloading"),
        tr(message)
    )
}

pub fn release_export_preview(
    series: usize,
    releases: usize,
    undated: usize,
    cancelled: usize,
) -> String {
    if croatian() {
        format!(
            "Serije: {series} · Izdanja: {releases} · Nedatirane epizode izostavljene: {undated} · Otkazani zapisi zbog uklanjanja iz odabira ili rasporeda: {cancelled}"
        )
    } else {
        format!(
            "Series: {series} · Releases: {releases} · Undated episodes omitted: {undated} · Cancellation records for scope or schedule removals: {cancelled}"
        )
    }
}
pub fn release_reminder_status(
    subscriptions: usize,
    refreshed: Option<&str>,
    error: Option<&str>,
) -> String {
    let base = if croatian() {
        format!("Pretplaćene serije: {subscriptions}")
    } else {
        format!("Subscribed series: {subscriptions}")
    };
    let refresh = match refreshed {
        Some(at) if croatian() => format!(" · Posljednje osvježavanje: {at}"),
        Some(at) => format!(" · Last refreshed: {at}"),
        None if croatian() => " · Osvježavanje na čekanju".into(),
        None => " · Refresh pending".into(),
    };
    format!(
        "{base}{refresh}{}",
        error.map(|e| format!(" · {e}")).unwrap_or_default()
    )
}

/// An unfinished resume point is intentionally numbering/progress only.
pub fn last_time_partial(episode: &str, fraction: f32) -> String {
    let percent = (fraction.clamp(0.0, 1.0) * 100.0).round() as u32;
    if croatian() {
        format!("Nedovršeno: {episode} · {percent}%")
    } else {
        format!("In progress: {episode} · {percent}%")
    }
}
