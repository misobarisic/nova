(() => {
  "use strict";
  const S = globalThis.Supernova;
  const lookupStreams = request => {
    const lookup = request.lookup;
    const settings = request.settings || {};
    if (!lookup || !["movie","series"].includes(lookup.mediaType) || !lookup.title || !lookup.mediaId) return [];
    if (lookup.mediaType === "series" && !(Number.isInteger(lookup.season) && lookup.season > 0 && Number.isInteger(lookup.episode) && lookup.episode > 0)) return [];
    const enabled = Object.keys(S.sources).filter(id => settings["sources." + id] === true);
    if (!enabled.length) return {streams:[],warnings:[{source:"Supernova",message:"Select a source in addon configuration."}]};
    const started = Date.now();
    const deadline = started + 20000;
    // Optional discovery/inspection cannot consume the final availability
    // check or prevent already-proved direct files from reaching the caller.
    const optionalDeadline = deadline - (settings["playback.mode"] === "direct" ? 500 : 4000);
    const identity = JSON.stringify({v:S.VERSION,fetchVersion:2,lookup,enabled,quality:settings["quality.preferred"],seeders:settings["seeders.minimum"]});
    const cacheKey = "lookup:v2:" + nova.crypto.hmacSha256Base64Url(identity,"supernova-matcher-v2");
    const cached = S.cacheGet(cacheKey);
    const now = Math.floor(started / 1000);
    const candidateRecords = Array.isArray(cached) ? cached : cached && cached.records;
    const validNumber = value => value === null || S.number(value) !== null;
    const validCache = Array.isArray(candidateRecords) && candidateRecords.length <= S.CANDIDATE_MAX && candidateRecords.every(record =>
      record && typeof record === "object" && S.hash(record.infoHash) && typeof record.title === "string" && record.title.length > 0 && record.title.length <= 1024 &&
      Array.isArray(record.sources) && record.sources.length > 0 && record.sources.length <= 8 && record.sources.every(source => typeof source === "string" && source.length > 0 && source.length <= 64) &&
      validNumber(record.fileIdx) && (record.fileIdx === null || S.number(record.fileIdx) <= 4294967295) && validNumber(record.sizeBytes) && validNumber(record.seeders) &&
      (record.torrentUrl === null || (typeof record.torrentUrl === "string" && S.metainfoUrl(record.torrentUrl,null) === record.torrentUrl)));
    const cachedRecords = validCache ? candidateRecords : null;
    let expiresAt = cached && S.number(cached.discoveryExpiresAt);
    // Older injected fixtures/entries have no expiry stamp. Their one-time
    // migration is short; new cache writes always preserve this fixed expiry.
    if (expiresAt === null || expiresAt === undefined) expiresAt = now + 60;
    let records = Array.isArray(cachedRecords) && expiresAt > now ? cachedRecords : null;
    const discoveryCacheHit = Array.isArray(records);
    const sourceWarnings = discoveryCacheHit && cached && Array.isArray(cached.warnings) ? cached.warnings.slice(0,3) : [];
    let sourceFailed = false;
    if (!discoveryCacheHit) {
      const searches = enabled.flatMap(id => S.queries(lookup,S.sources[id].anime).slice(0,2).map(query => ({id,query,page:0,url:S.sources[id].url(query,lookup,0)})));
      const merged = new Map();
      const followups = [];
      const failed = (search,message) => {
        sourceFailed = true;
        if (!sourceWarnings.some(warning => warning.source === S.sources[search.id].name)) sourceWarnings.push({source:S.sources[search.id].name,message});
      };
      const consume = (search,response) => {
        const source = S.sources[search.id];
        let parsed;
        try {
          if (!response || response.error || response.status < 200 || response.status >= 300) throw new Error("Unavailable source");
          parsed = source.parse(response.body);
        } catch (_) {
          if (!search.fallback && search.page === 0 && source.fallbackUrl) {
            followups.push({...search,fallback:true,url:source.fallbackUrl(search.query)});
          } else failed(search,"Source unavailable or returned an unsupported response. Other sources are still usable.");
          return;
        }
        for (const row of parsed) {
          if (!S.candidateMatches(row,lookup) || (row.seeders !== null && row.seeders < (settings["seeders.minimum"] || 0))) continue;
          const existing = merged.get(row.infoHash);
          if (existing) {
            existing.sources = [...new Set([...existing.sources,...row.sources])];
            if (row.seeders !== null) existing.seeders = Math.max(existing.seeders || 0,row.seeders);
            if (!existing.torrentUrl) existing.torrentUrl = row.torrentUrl;
          } else merged.set(row.infoHash,{...row,fileIdx:null});
        }
        if (!search.fallback && search.page === 0 && source.pages) {
          try {
            if (source.pages(response.body) > 1) followups.push({...search,page:1,url:source.url(search.query,lookup,1)});
          } catch (_) { failed(search,"Source pagination could not be read. Other sources are still usable."); }
        }
      };
      const responses = S.batch(searches.map(search => ({url:search.url,method:"GET"})),optionalDeadline);
      searches.forEach((search,index) => consume(search,responses[index]));
      // Only first-page evidence schedules a bounded second page or fallback;
      // no recursive pagination occurs and each source query has at most two.
      const extras = followups.slice(0,searches.length);
      const extraResponses = S.batch(extras.map(search => ({url:search.url,method:"GET"})),optionalDeadline);
      extras.forEach((search,index) => consume(search,extraResponses[index]));
      records = S.rank([...merged.values()],settings).slice(0,S.CANDIDATE_MAX);
    }
    records = S.rank(records.filter(record => S.candidateMatches(record,lookup)),settings).slice(0,S.CANDIDATE_MAX);
    const warnings = sourceWarnings.slice();
    const availabilityBudget = Math.min(3000,deadline - Date.now() - 500);
    // Cached availability sees the bounded candidate inventory before optional
    // P2P inspection spends the rest of the operation's time.
    const torbox = availabilityBudget >= 200 ? S.torboxOptions(records,lookup,settings,warnings,availabilityBudget) : [];
    if (availabilityBudget < 200 && settings["playback.mode"] !== "direct") warnings.push({source:"TorBox",message:"Availability check could not finish in time. Try again."});
    const inspectionDeadline = deadline - 500;
    const toInspect = settings["playback.mode"] !== "cached" ? records.filter(record => S.number(record.fileIdx) === null && record.torrentUrl).sort((a,b)=>(a.metainfoAttemptAt || 0)-(b.metainfoAttemptAt || 0)).slice(0,6) : [];
    const torrents = S.batch(toInspect.map(record => ({url:record.torrentUrl,method:"GET",responseType:"binary"})),inspectionDeadline);
    let verificationFailed = false;
    toInspect.forEach((record,index) => {
      record.metainfoAttemptAt = Date.now();
      const response = torrents[index];
      if (!response || response.error || response.status !== 200 || !response.bodyBase64) { verificationFailed = true; return; }
      let metadata;
      try { metadata = S.json(nova.torrent.inspect(response.bodyBase64)); } catch (_) { verificationFailed = true; return; }
      if (!metadata || S.hash(metadata.infoHash) !== record.infoHash) { verificationFailed = true; return; }
      const file = S.chooseFile(metadata.files,record,lookup,false);
      if (file) { record.fileIdx = file.selectedId; record.sizeBytes = file.size; }
      else verificationFailed = true;
    });
    if (!discoveryCacheHit) expiresAt = Math.floor(Date.now()/1000) + (sourceFailed || verificationFailed || !records.length ? 60 : 86400);
    const remainingTtl = Math.max(0,expiresAt - Math.floor(Date.now()/1000));
    // Failed empty discovery must retry immediately. Updating file proof on a
    // cache hit never renews the original discovery TTL indefinitely.
    if ((!discoveryCacheHit || toInspect.length) && remainingTtl > 0 && !(sourceFailed && !records.length)) S.cacheSet(cacheKey,{records,warnings:sourceWarnings,discoveryExpiresAt:expiresAt},remainingTtl);
    if (verificationFailed) warnings.push({source:"Supernova",message:"Some torrent files could not be verified. Other results are still usable."});
    const direct = settings["playback.mode"] === "cached" ? [] : records.filter(record => S.number(record.fileIdx) !== null).map(record => S.row(record,false));
    const streams = [...torbox,...direct].slice(0,S.MAX);
    if (discoveryCacheHit) {
      for (const stream of streams) stream.description += "\nCached discovery; peer availability unverified.";
    }
    return warnings.length ? {streams,warnings} : streams;
  };
  globalThis.novaProvider = {
    handle(request) {
      switch (request.op) {
        case "manifest": return request.manifest;
        case "lookupStreams": return lookupStreams(request);
        case "resolveStream":
          try { return S.resolveTorbox(request.token,request.settings || {}); }
          catch (error) { return {error:{code:error.code || "provider_error",message:error.code ? error.message : "Could not resolve this stream."}}; }
        case "streams": return [];
        default: throw new Error("Unsupported Supernova action.");
      }
    },
  };
})();
