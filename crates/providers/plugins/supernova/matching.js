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
  S.rank = (records, settings) => {
    const quality = settings["quality.preferred"] || "1080p";
    const score = record => (quality === "any" || new RegExp("\\b" + quality + "\\b", "i").test(record.title) ? 1000000 : 0) + Math.min(record.seeders || 0, 999999);
    return records.sort((a,b) => score(b) - score(a) || a.title.localeCompare(b.title));
  };
})();
