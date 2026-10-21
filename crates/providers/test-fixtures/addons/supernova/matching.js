(() => {
  "use strict";
  const S = globalThis.Supernova;
  S.normal = value => String(value || "").normalize("NFKC").toLowerCase().replace(/[^\p{L}\p{N}]+/gu, " ").trim();
  const stripGroup = value => String(value || "").replace(/^(?:\s*\[[^\]]{1,120}\]\s*)+/, "");
  // Match the canonical title first, retaining the release suffix in its
  // original form. Punctuation and spacing may differ, but title numbers do
  // not become episode ranges (Steins;Gate 0 - 01) or another show's title.
  S.releaseSuffix = (title, lookup) => {
    const expected = S.normal(lookup.title);
    if (expected.length < 2) return null;
    const actual = stripGroup(title).normalize("NFKC").trimStart();
    const pattern = "^" + expected.split(" ").join("[^\\p{L}\\p{N}]*") + "(?![\\p{L}\\p{N}])";
    const match = actual.match(new RegExp(pattern, "iu"));
    if (!match) return null;
    const suffix = actual.slice(match[0].length);
    const numberSuffix = suffix.replace(/^[\s._-]+/, "");
    const nextNumber = numberSuffix.match(/^(\d+)(?=$|[^\p{L}\p{N}])/u);
    if (nextNumber && !/^(?:19|20)\d{2}$/.test(nextNumber[1])) {
      // A bare extra number normally identifies a sequel. The only exception
      // is a padded episode explicitly aligned by the app, with no later
      // episode claim competing with that number.
      const remainder = numberSuffix.slice(nextNumber[0].length);
      const beginsRange = /^\s*[-~]\s*(?:E)?\d/.test(remainder);
      if (lookup.mediaType !== "series" || (nextNumber[1].length < 2 && !/^\s*-/.test(suffix) && !beginsRange)
          || Number(nextNumber[1]) <= 0 || (Number(nextNumber[1]) !== lookup.absoluteEpisode && !beginsRange)
          || /\b(?:episode|ep|S\d{1,2}[ ._-]*E|E\d|\d{1,2}x)/i.test(remainder)) return null;
    }
    return suffix;
  };
  S.titleMatches = (title, lookup) => S.releaseSuffix(title, lookup) !== null;
  const supplementary = /(?:^|[\s/._-])(?:samples?|trailers?|extras?|featurettes?|interviews?|commentary|bonus|bts|behind[ ._-]*the[ ._-]*scenes|deleted[ ._-]*scenes|making[ ._-]*of)(?:[\s/._-]|$)/i;
  S.episodeMatches = (title, lookup, verifiedRoot = false) => {
    const suffix = S.releaseSuffix(title, lookup);
    const text = suffix === null ? String(title || "") : suffix;
    if (supplementary.test(text) || /\b(?:batch|complete|collection|season\s*pack|ova|specials?)\b/i.test(text) || /\d\s*[-~]\s*\d/.test(text)) return false;
    // Combined episode labels must not authorize one episode just because the
    // first marker matches. Treat a second marker or joined number as a pack.
    const markers = text.match(/\b(?:S\d{1,2}[ ._-]*E\d{1,4}|\d{1,2}x\d{1,4})(?!\d)/gi) || [];
    if (markers.length > 1 || /\bS\d{1,2}[ ._-]*E\d{1,4}(?:[ ._-]*E\d|[ ._-]*[+&,/][ ._-]*(?:E)?\d|[ ._-]+(?:and|to)[ ._-]+\d)/i.test(text)
        || /\b\d{1,2}x\d{1,4}(?:[ ._-]*x\d|[ ._-]*[+&,/][ ._-]*(?:\d{1,2}x)?\d|[ ._-]+(?:and|to)[ ._-]+\d)/i.test(text)) return false;
    const numbered = text.match(/\bS(\d{1,2})[ ._-]*E(\d{1,4})(?!\d|[ ._-]*E\d)/i) || text.match(/\b(\d{1,2})x(\d{1,4})(?!\d)/i);
    if (numbered) return Number(numbered[1]) === lookup.season && Number(numbered[2]) === lookup.episode;
    // Absolute numbering is only evidence when Nova supplied verified alignment.
    if (!Number.isSafeInteger(lookup.absoluteEpisode) || lookup.absoluteEpisode <= 0) return false;
    const explicitAbsolute = text.match(/(?:\s-\s|^\s*-\s*|\b(?:episode|ep)\s*(?:E)?|\bE)(\d{1,4})(?:v\d)?(?=\s|[.\[_(]|$)/i);
    const absolute = explicitAbsolute || ((suffix !== null || verifiedRoot) && text.match(/^\s*[._-]?\s*(\d{1,4})(?:v\d)?(?=\s|[.\[_(]|$)/i));
    if (!absolute || Number(absolute[1]) !== lookup.absoluteEpisode) return false;
    const season = text.match(/\b(?:season\s*|S)(\d{1,2})(?!\d)/i);
    return !season || Number(season[1]) === lookup.season;
  };
  S.matches = (record, lookup) => {
    if (!S.hash(record.infoHash) || !S.titleMatches(record.title, lookup)) return false;
    if (record.imdb && /^tt\d+$/.test(lookup.mediaId) && record.imdb !== lookup.mediaId) return false;
    if (lookup.mediaType === "movie") {
      if (supplementary.test(record.title) || /\bS\d{1,2}[ ._-]*E\d|\b(?:collection|complete)\b/i.test(record.title)) return false;
      const requestedYear = String(lookup.year || "").match(/\b(?:19|20)\d{2}\b/);
      const releaseYear = S.releaseSuffix(record.title,lookup).match(/\b(?:19|20)\d{2}\b/);
      if (requestedYear && releaseYear && requestedYear[0] !== releaseYear[0]) return false;
      return !!((requestedYear && releaseYear) || (record.imdb && record.imdb === lookup.mediaId));
    }
    const requestedYear = String(lookup.year || "").match(/\b(?:19|20)\d{2}\b/);
    const releaseYear = S.releaseSuffix(record.title,lookup).match(/\b(?:19|20)\d{2}\b/);
    if (requestedYear && releaseYear && requestedYear[0] !== releaseYear[0]) return false;
    return lookup.season > 0 && lookup.episode > 0 && S.episodeMatches(record.title, lookup);
  };
  // Discovery may admit a season inventory, but it never assigns a file. A
  // hashed metainfo/account file list must independently prove the episode.
  S.candidateMatches = (record, lookup) => {
    if (lookup.mediaType === "movie") return S.matches(record,lookup);
    if (!S.hash(record.infoHash) || !S.titleMatches(record.title,lookup)
        || !(lookup.season > 0 && lookup.episode > 0)) return false;
    if (record.imdb && /^tt\d+$/.test(lookup.mediaId) && record.imdb !== lookup.mediaId) return false;
    const suffix = S.releaseSuffix(record.title,lookup);
    if (supplementary.test(suffix)) return false;
    const requestedYear = String(lookup.year || "").match(/\b(?:19|20)\d{2}\b/);
    const releaseYear = suffix.match(/\b(?:19|20)\d{2}\b/);
    if (requestedYear && releaseYear && requestedYear[0] !== releaseYear[0]) return false;
    const seasons = [...suffix.matchAll(/\b(?:(?:season\s*|S)(\d{1,2})(?!\d)|(\d{1,2})x\d{1,4})/gi)].map(match=>Number(match[1] || match[2]));
    if (seasons.some(season=>season !== lookup.season)) return false;
    const ranges = [...suffix.matchAll(/(?:E|episodes?\s*|(?:^|[\s[(])\s*-?\s*)(\d{1,4})\s*[-~]\s*(?:E)?(\d{1,4})(?!\d)/gi)]
      .map(match=>[Number(match[1]),Number(match[2])]).filter(([start,end])=>start < 1900 || end < 1900);
    if (ranges.length) {
      const number = seasons.length ? lookup.episode : lookup.absoluteEpisode;
      if (Number.isSafeInteger(number) && ranges.some(([start,end])=>start <= 0 || end < start || number < start || number > end)) return false;
      return true;
    }
    // A single-episode claim for another episode is not a generic inventory.
    // Title-only season packs may proceed to strict file inspection.
    const declared = suffix.match(/\b(?:S\d{1,2}[ ._-]*E|\d{1,2}x)(\d{1,4})(?!\d)/i);
    if (declared && !/\b(?:batch|complete|collection)\b|[+&,]/i.test(suffix)
        && Number(declared[1]) !== lookup.episode) return false;
    const absolute = suffix.match(/(?:^\s*-\s*|\b(?:episode|ep)\s*(?:E)?|\bE)(\d{1,4})(?:v\d)?(?=\s|[.\[_(]|$)/i);
    if (absolute && !/\b(?:batch|complete|collection)\b|[+&,]/i.test(suffix)
        && Number.isSafeInteger(lookup.absoluteEpisode) && Number(absolute[1]) !== lookup.absoluteEpisode) return false;
    return true;
  };
  S.chooseFile = (files, record, lookup, torbox, verifiedContainer = null) => {
    if (!Array.isArray(files) || files.length > 512 || !S.candidateMatches(record, lookup)) return null;
    const videos = files.filter(file => {
      const path = String(file.path || file.name || "");
      if (!/\.(?:mkv|mp4|avi|mov|m4v|webm|ts)$/i.test(path) || supplementary.test(path)) return false;
      const name = path.split("/").pop();
      if (lookup.mediaType === "movie") {
        // Only strip an uploader prefix proven by the verified release label,
        // rather than accepting arbitrary title mentions in another video.
        const group = String(record.title).match(/-([A-Za-z0-9_]{1,40})(?:\.[A-Za-z0-9]{2,5})?$/);
        const prefix = group ? group[1].toLowerCase() + "-" : null;
        const candidate = prefix && name.toLowerCase().startsWith(prefix) ? name.slice(prefix.length) : name;
        const requestedYear = String(lookup.year || "").match(/\b(?:19|20)\d{2}\b/);
        const fileSuffix = S.releaseSuffix(candidate,lookup);
        const fileYear = fileSuffix && fileSuffix.match(/\b(?:19|20)\d{2}\b/);
        return S.titleMatches(candidate,lookup) && (!requestedYear || (fileYear && requestedYear[0] === fileYear[0]));
      }
      // A uniquely matching episode number in a different show's file is
      // insufficient. Permit bare-number filenames only inside the verified
      // torrent root; otherwise the video itself must identify the title.
      const bareNumber = /^(?:S\d{1,2}[ ._-]*E\d{1,4}|(?:episode|ep)\s*\d{1,4}|\d{1,4})\.[^.]+$/i.test(name);
      const root = path.includes("/") ? path.slice(0,path.lastIndexOf("/")) : "";
      const rootProof = (root && S.candidateMatches({...record,title:root},lookup))
        || (!root && typeof verifiedContainer === "string" && S.candidateMatches({...record,title:verifiedContainer},lookup));
      const named = S.candidateMatches({...record,title:name},lookup);
      if (!named && !(bareNumber && rootProof)) return false;
      const container = root || verifiedContainer || "";
      const relative = bareNumber && rootProof && /\b(?:season\s*|S)\d{1,2}(?!\d)/i.test(container);
      const episodeLookup = relative ? {...lookup,absoluteEpisode:lookup.episode} : lookup;
      return S.episodeMatches(name, episodeLookup, !!(bareNumber && rootProof));
    });
    if (videos.length !== 1) return null;
    const file = videos[0];
    const id = torbox ? S.number(file.id) : S.number(file.index);
    return id === null ? null : {...file, selectedId:id};
  };
  S.queries = (lookup, anime) => {
    // Comet's Nyaa/AnimeTosho fetchers start with canonical title discovery;
    // requesting only SxxExx hides ordinary anime releases and season labels.
    const title = String(lookup.title || "").replace(/[\r\n\t]/g, " ").slice(0, 200);
    if (lookup.mediaType === "movie") {
      const year = String(lookup.year || "").match(/\b(?:19|20)\d{2}\b/);
      return year ? [title, `${title} ${year[0]}`] : [title];
    }
    const numbered = `${title} S${String(lookup.season).padStart(2,"0")}E${String(lookup.episode).padStart(2,"0")}`;
    const episode = anime && Number.isSafeInteger(lookup.absoluteEpisode) && lookup.absoluteEpisode > 0 ?
      `${title} ${String(lookup.absoluteEpisode).padStart(2,"0")}` : numbered;
    return [...new Set([title, episode])].slice(0,2);
  };
  S.SORT_DEFAULT = ["cached","resolution","size","pack","seeders","language","codec","subtitle"].map(key=>({key,direction:"desc"}));
  S.sortOrder = settings => {
    if (settings.streamPreferences) return settings.streamPreferences.sort_order || [];
    const order = settings["sort.order"];
    const valid = Array.isArray(order) && order.length >= 5 && order.length <= S.SORT_DEFAULT.length &&
      new Set(order.map(item=>item && item.key)).size === order.length &&
      order.every(item=>item && S.SORT_DEFAULT.some(known=>known.key === item.key) && ["asc","desc"].includes(item.direction)) &&
      S.SORT_DEFAULT.slice(0,5).every(known=>order.some(item=>item.key === known.key));
    // Adding criteria must not discard a saved permutation or its directions.
    const resolved=valid ? [...order,...S.SORT_DEFAULT.filter(known=>!order.some(item=>item.key === known.key))] : S.SORT_DEFAULT;
    return resolved;
  };
  const releaseLanguages = {
    en:"eng|english", ja:"jpn|japanese", hr:"hrv|croatian", de:"deu|ger|german",
    fr:"fra|fre|french", es:"spa|spanish", it:"ita|italian", pt:"por|portuguese",
    ru:"rus|russian", ko:"kor|korean", zh:"zho|chi|chinese|mandarin|cantonese",
    ar:"ara|arabic", hi:"hin|hindi", nl:"nld|dut|dutch", pl:"pol|polish",
    tr:"tur|turkish", sv:"swe|swedish", no:"nor|norwegian", da:"dan|danish",
    fi:"fin|finnish", cs:"ces|cze|czech", hu:"hun|hungarian", ro:"ron|rum|romanian",
    uk:"ukr|ukrainian", el:"ell|gre|greek", he:"heb|hebrew", th:"tha|thai",
    vi:"vie|vietnamese", id:"ind|indonesian"
  };
  const languageAliases = Object.values(releaseLanguages).join("|");
  // Each lookup may rank hundreds of records across several source waves.
  // Compile the Unicode patterns once, rather than once per row/language.
  const languagePatterns = new Map(Object.entries(releaseLanguages).map(([code,aliases])=>[code,
    new RegExp("(?:^|[^\\p{L}\\p{N}])(" + aliases + ")(?=$|[^\\p{L}\\p{N}])", "giu")]));
  const languageList = "[ ._:-]*(?:[/,;+&][ ._:-]*(?:" + languageAliases + ")[ ._:-]*)*";
  const audioSuffixPattern = new RegExp("^" + languageList + "audio\\b", "i");
  const subtitleSuffixPattern = new RegExp("^" + languageList + "(?:subs?|subtitles?)\\b", "i");
  const subtitlePrefixPattern = new RegExp("\\b(?:subs?|subtitles?)[ ._:-]*(?:(?:" + languageAliases + ")[ ._:-]*[/,;+&][ ._:-]*)*$", "i");
  const languageClaims = (text,subtitle) => {
    const labels = String(text || "").normalize("NFKC");
    if (!labels) return [];
    return Object.keys(releaseLanguages).filter(code=>{
      const pattern = languagePatterns.get(code);
      pattern.lastIndex=0;
      let match;
      while ((match = pattern.exec(labels))) {
        const before = labels.slice(0,pattern.lastIndex - match[1].length).split(/[\[\]\n|]/).pop();
        const after = labels.slice(pattern.lastIndex);
        const audioSuffix = audioSuffixPattern.test(after);
        const subtitleSuffix = subtitleSuffixPattern.test(after);
        const subtitlePrefix = subtitlePrefixPattern.test(before);
        const isSubtitle = !audioSuffix && (subtitleSuffix || subtitlePrefix);
        // Generic release language labels remain audio hints; subtitles need an
        // explicit subtitle marker, and unnamed Multi-Subs proves no language.
        if (isSubtitle === subtitle) return true;
      }
      return false;
    });
  };
  S.releaseLanguages = text => languageClaims(text,false);
  S.releaseSubtitleLanguages = text => languageClaims(text,true);
  const preferredLanguageRank = (languages,preferred) => {
    if (!Array.isArray(preferred) || preferred.length === 0) return null;
    const found = new Set(languages);
    const index = preferred.findIndex(code=>typeof code === "string" && found.has(code));
    return index < 0 ? null : -index;
  };
  S.languageRank = (texts,settings,metadata) => {
    const preferred = settings["language.prefs"];
    if (!Array.isArray(preferred) || !preferred.length) return null;
    return preferredLanguageRank(metadata && Array.isArray(metadata.audioLanguages) ? metadata.audioLanguages : texts.flatMap(S.releaseLanguages),preferred);
  };
  S.subtitleRank = (texts,settings,metadata) => {
    const preferred = settings["subtitle.prefs"];
    if (!Array.isArray(preferred) || !preferred.length) return null;
    return preferredLanguageRank(metadata && Array.isArray(metadata.subtitleLanguages) ? metadata.subtitleLanguages : texts.flatMap(S.releaseSubtitleLanguages),preferred);
  };
  const displayedLanguages = line => Object.keys(releaseLanguages).filter(code=>
    new RegExp("(?:^|[^\\p{L}\\p{N}])(?:" + code + "|" + releaseLanguages[code] + ")(?=$|[^\\p{L}\\p{N}])", "iu").test(line));
  const releaseCodecPatterns = [["avc","h[ .]?264|x264|avc"],["hevc","h[ .]?265|x265|hevc"],["av1","av1"],["vp9","vp[ -]?9"],["other","vp[ -]?8|mpeg[ -]?2|mpeg[ -]?4 visual|xvid|divx|h[ .]?263|vc[ -]?1|wmv3|prores"]]
    .map(([codec,pattern])=>[codec,new RegExp("(?:^|[^\\p{L}\\p{N}])(?:" + pattern + ")(?=$|[^\\p{L}\\p{N}])", "iu")]);
  S.releaseVideoCodec = text => {
    const codecs = new Set();
    for (const value of Array.isArray(text) ? text : [text]) {
      const original = String(value || "").normalize("NFKC");
      if (!original) continue;
      // Leading bracket groups and trailing release groups are identities,
      // rather than codec metadata (a group may itself contain "AV1").
      const labels = (stripGroup(original) || original).replace(/-[\p{L}\p{N}]+\s*$/u, "");
      for (const [codec,pattern] of releaseCodecPatterns) {
        if (pattern.test(labels)) codecs.add(codec);
      }
    }
    // Conflicting title/file labels cannot establish a codec preference.
    return codecs.size === 1 ? [...codecs][0] : null;
  };
  S.codecRank = (texts,settings,metadata) => {
    const preferred = settings["codec.prefs"];
    if (!Array.isArray(preferred) || preferred.length === 0) return null;
    const codec = metadata && Object.prototype.hasOwnProperty.call(metadata,"videoCodec") ? metadata.videoCodec : S.releaseVideoCodec(texts);
    if (!["avc","hevc","av1"].includes(codec)) return null;
    const index = preferred.indexOf(codec);
    return index < 0 ? null : -index;
  };
  S.resolution = title => {
    const text=String(title || "");
    // Exact advertised tokens only: preserve nonstandard heights and reject
    // conflicting claims rather than rounding or picking the first label.
    const heights=[...text.matchAll(/(?<![a-z\d])(?<!\b\d{1,3}[.,])([1-9]\d{2,3})p(?![a-z\d])/gi)].map(match=>Number(match[1]));
    if (/\b(?:4k|uhd)\b/i.test(text)) heights.push(2160);
    if (/\b8k\b/i.test(text)) heights.push(4320);
    const unique=[...new Set(heights)];
    return unique.length===1 && S.validResolutionHeight(unique[0]) ? unique[0] : null;
  };
  const recordFactSnapshots = new WeakMap();
  S.recordFacts = record => {
    // Discovery merges replace evidence/probe objects. Proof and peer upgrades
    // mutate the listed scalars; every input is part of this dirty signature.
    const inputs=[record.title,record.fileName,record.fileIdx,record.sizeBytes,record.seeders,record.sourceMetadata,record.trackMetadata];
    const prior=recordFactSnapshots.get(record);
    if (prior && inputs.every((value,index)=>value===prior.inputs[index])) return prior.facts;
    const tracks=S.validTrackMetadata(record.trackMetadata) && record.trackMetadata.fileName===record.fileName && record.trackMetadata.fileSize===record.sizeBytes ? record.trackMetadata : null;
    const metadata=S.recordMetadata(record,tracks), texts=[record.title,record.fileName || ""];
    const pack=S.pack(record.title);
    const facts={
      height:Object.prototype.hasOwnProperty.call(metadata,"resolution") ? metadata.resolution : texts.map(S.resolution).find(Boolean) || null,
      // Before file proof this can be a season-pack total, not episode bytes.
      bytes:S.number(record.fileIdx) === null ? null : S.number(record.sizeBytes),
      codec:Object.prototype.hasOwnProperty.call(metadata,"videoCodec") ? metadata.videoCodec : S.releaseVideoCodec(texts),
      audio_languages:Array.isArray(metadata.audioLanguages) ? metadata.audioLanguages : [...new Set(texts.flatMap(S.releaseLanguages))],
      subtitle_languages:Array.isArray(metadata.subtitleLanguages) ? metadata.subtitleLanguages : [...new Set(texts.flatMap(S.releaseSubtitleLanguages))],
      cached:null,season_pack:pack === null ? null : !!pack,seeders:S.number(record.seeders),
    };
    recordFactSnapshots.set(record,{inputs,facts});
    return facts;
  };
  S.rowFacts = row => {
    if (S.preferenceFacts.has(row)) return S.preferenceFacts.get(row);
    if (row.streamFacts) return row.streamFacts;
    const token=row.resolver && typeof row.resolver.token==="string" ? S.json(row.resolver.token) : null;
    const lines=String(row.description || "").split("\n"),release=lines.find(line=>line.startsWith("🗂️ ")),filename=lines.find(line=>line.startsWith("📁 "));
    const title=token && token.title || (release ? release.slice("🗂️ ".length) : filename ? filename.slice(3) : "");
    const fileName=token && token.fileName || "";
    const video=lines.find(line=>line.startsWith("🎞️ ")) || "";
    const codecLabel=video.split(" · ").map(value=>value.replace(/^🎞️ /,"")).find(label=>["AVC","HEVC","AV1","VP9","Other"].includes(label));
    const audio=lines.find(line=>line.startsWith("🌐 ")),subtitles=lines.find(line=>line.startsWith("💬 "));
    const facts={height:S.resolution(row.title) || S.resolution(title),bytes:S.number(row.size),
      codec:/\bCodec unknown\b/i.test(video) ? null : codecLabel ? {AVC:"avc",HEVC:"hevc",AV1:"av1",VP9:"vp9",Other:"other"}[codecLabel] : S.releaseVideoCodec([title,fileName]),
      audio_languages:audio ? displayedLanguages(audio) : [...new Set([title,fileName].flatMap(S.releaseLanguages))],
      subtitle_languages:subtitles ? displayedLanguages(subtitles) : [...new Set([title,fileName].flatMap(S.releaseSubtitleLanguages))],
      cached:!!row.resolver,season_pack:S.pack(title) === null ? null : !!S.pack(title),seeders:S.number(row.seeders)};
    S.preferenceFacts.set(row,facts);
    return facts;
  };
  S.validPreferenceFacts = facts => !!facts && typeof facts==="object" && !Array.isArray(facts)
    && Object.keys(facts).every(key=>["height","bytes","codec","audio_languages","subtitle_languages","cached","season_pack","seeders"].includes(key))
    && (facts.height===null || S.validResolutionHeight(facts.height))
    && (facts.bytes===null || S.number(facts.bytes)!==null) && (facts.seeders===null || S.number(facts.seeders)!==null)
    && [null,"avc","hevc","av1","vp9","other"].includes(facts.codec)
    && [null,true,false].includes(facts.cached) && [null,true,false].includes(facts.season_pack)
    && [facts.audio_languages,facts.subtitle_languages].every(list=>Array.isArray(list) && list.length<=32 && list.every(code=>typeof code==="string" && code.length<=64 && /^[a-z\d-]+$/i.test(code)));
  S.pack = title => {
    if (typeof title !== "string" || !title.trim()) return null;
    return /\b(?:batch|complete|collection|season[ ._-]*pack)\b|\b(?:S\d{1,2}[ ._-]*E\d{1,4}[ ._-]*[-~+&,][ ._-]*(?:E)?\d|episodes?[ ._-]*\d+[ ._-]*[-~][ ._-]*\d+)/i.test(title)
      || (/\b(?:season[ ._-]*|S)\d{1,2}(?!\d)/i.test(title) && !/\bS\d{1,2}[ ._-]*E\d|\b\d{1,2}x\d{1,4}/i.test(title)) ? 1 : 0;
  };
  const compareText = (a,b) => a < b ? -1 : a > b ? 1 : 0;
  S.sortCompare = (a,b,order,metrics,identity) => {
    for (const criterion of order) {
      const av = metrics(a,criterion.key), bv = metrics(b,criterion.key);
      // Missing reported metadata never becomes attractive when reversed.
      if (av === null && bv !== null) return 1;
      if (bv === null && av !== null) return -1;
      if (av !== null && av !== bv) return (av < bv ? -1 : 1) * (criterion.direction === "asc" ? 1 : -1);
    }
    return compareText(identity(a),identity(b));
  };
  // One sort snapshot owns its metrics: later metadata upgrades build fresh
  // values, while comparisons reuse each row/key instead of parsing repeatedly.
  const memoizedMetrics = calculate => {
    const values = new Map();
    return (row,key) => {
      if (!values.has(row)) values.set(row,new Map());
      const cached = values.get(row);
      if (!cached.has(key)) cached.set(key,calculate(row,key));
      return cached.get(key);
    };
  };
  S.sortRows = (rows,settings) => {
    if (settings.streamPreferences) {
      rows=rows.filter(row=>S.acceptPreferences(S.rowFacts(row),settings,true));
      // The host owns every criterion and direction. Addon-local preferences
      // must not reappear as hidden secondary ranking when shared metrics tie.
      return S.sortByPreferences(rows,settings,S.rowFacts,row=>row.id);
    }
    const order = S.sortOrder(settings);
    const preferenceKeys = {language:"language.prefs",subtitle:"subtitle.prefs",codec:"codec.prefs"};
    const facts = new Map();
    const metrics = memoizedMetrics((row,key)=>{
    if (preferenceKeys[key] && !(Array.isArray(settings[preferenceKeys[key]]) && settings[preferenceKeys[key]].length)) return null;
    if (!facts.has(row)) {
      const token = row.resolver && typeof row.resolver.token === "string" ? S.json(row.resolver.token) : null;
      const lines = String(row.description || "").split("\n");
      const filename = lines.find(line=>line.startsWith("📁 "));
      const release = lines.find(line=>line.startsWith("🗂️ "));
      const title = token && typeof token.title === "string" ? token.title : release ? release.slice("🗂️ ".length) : filename ? filename.slice(3) : lines[0];
      facts.set(row,{token,lines,title});
    }
    const {token,lines,title} = facts.get(row);
    switch (key) {
      case "cached": return row.resolver ? 1 : 0;
      case "resolution": return S.resolution(title) || S.resolution(row.title);
      case "size": return S.number(row.size);
      case "pack": return S.pack(title);
      case "seeders": return S.number(row.seeders);
      case "language":
      case "subtitle":
      case "codec": {
        const fileName = token && typeof token.fileName === "string" ? token.fileName.split(/[\\/]/).pop() : "";
        const codecLabel = lines[0].startsWith("🎞️ ") ? lines[0].slice(4).split(" · ").find(label=>["AVC","HEVC","AV1"].includes(label)) || "" : "";
        if (key === "codec") {
          if (/\bCodec unknown\b/i.test(lines[0])) return null;
          const metadata = token && token.trackMetadata || (codecLabel ? {videoCodec:{AVC:"avc",HEVC:"hevc",AV1:"av1"}[codecLabel]} : null);
          return S.codecRank([title,fileName],settings,metadata);
        }
        const line = lines.find(line=>line.startsWith(key === "language" ? "🌐 " : "💬 "));
        const property = key === "language" ? "audioLanguages" : "subtitleLanguages";
        const metadata = token && token.trackMetadata ? token.trackMetadata : line ? {[property]:displayedLanguages(line)} : null;
        return key === "language" ? S.languageRank([title,fileName],settings,metadata) : S.subtitleRank([title,fileName],settings,metadata);
      }
      default: return null;
    }
    });
    return rows.sort((a,b)=>S.sortCompare(a,b,order,metrics,row=>row.id));
  };
  S.rank = (records, settings) => {
    if (settings.streamPreferences) {
      // Facts include language/codec parsing. Only native-policy operations
      // need that work, and filtering/comparison share one snapshot per row.
      const snapshots=new Map();
      const facts=record=>{
        if (!snapshots.has(record)) snapshots.set(record,S.recordFacts(record));
        return snapshots.get(record);
      };
      records=records.filter(record=>S.acceptPreferences(facts(record),settings,false));
      return S.sortByPreferences(records,settings,facts,record=>`${record.infoHash}:${record.fileIdx === null || record.fileIdx === undefined ? "unknown" : record.fileIdx}`);
    }
    const quality = settings["quality.preferred"] || "1080p";
    const qualityPattern = quality === "any" ? null : new RegExp("\\b" + quality + "\\b", "i");
    const preferred = new Map(records.map(record=>[record,!qualityPattern || qualityPattern.test(record.title) || String(S.recordMetadata(record).resolution)+"p"===quality]));
    const order = S.sortOrder(settings);
    // Preserve the existing quality preference for the bounded inspection pool.
    // Availability is unknown here; visible proved rows use the full sort order.
    const metrics = memoizedMetrics((record,key)=>{
      switch (key) {
        case "resolution": { const metadata=S.recordMetadata(record); return Object.prototype.hasOwnProperty.call(metadata,"resolution") ? metadata.resolution : S.resolution(record.title); }
        case "size": return S.number(record.sizeBytes);
        case "pack": return S.pack(record.title);
        case "seeders": return S.number(record.seeders);
        case "language": return S.languageRank([record.title],settings,S.recordMetadata(record));
        case "subtitle": return S.subtitleRank([record.title],settings,S.recordMetadata(record));
        case "codec": return S.codecRank([record.title],settings,S.recordMetadata(record));
        default: return null;
      }
    });
    return records.sort((a,b)=>Number(preferred.get(b))-Number(preferred.get(a)) || S.sortCompare(a,b,order,metrics,
      record=>`${record.infoHash}:${record.fileIdx === null || record.fileIdx === undefined ? "unknown" : record.fileIdx}`));
  };
})();
