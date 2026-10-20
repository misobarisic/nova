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
        "Connect Stremio in Settings → Tracking first." => {
            "Prvo povežite Stremio u Postavke → Praćenje."
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
        "Connect a service in Settings → Tracking to see suggestions." => {
            "Povežite uslugu u Postavkama → Praćenje za prikaz prijedloga."
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
        "Connect this service in Settings → Tracking first." => {
            "Prvo povežite ovu uslugu u Postavke → Praćenje."
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

/// Readable values for setting-scope comparisons; identifiers remain untranslated.
pub fn setting_sync_value(field: &str, value: &serde_json::Value) -> String {
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
                    "Pomak {:+.1} s · veličina {} · margine {} px · položaj {}% · obrub {} · {color} · {embedded}",
                    style.delay, font_size, style.margin, style.position, outline
                )
            } else {
                format!(
                    "Timing {:+.1} s · size {} · margins {} px · position {}% · outline {} · {color} · {embedded}",
                    style.delay, font_size, style.margin, style.position, outline
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
        "home_catalog_sources" | "home_row_sources" | "home_rows" => {
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
    fn watch_history_labels_follow_the_selected_language() {
        with_language(Language::English, || {
            assert_eq!(tr("Watch history"), "Watch history");
            assert_eq!(tr("Resume"), "Resume");
            assert_eq!(tr("Play again"), "Play again");
        });
        with_language(Language::Croatian, || {
            assert_eq!(tr("Watch history"), "Povijest gledanja");
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
