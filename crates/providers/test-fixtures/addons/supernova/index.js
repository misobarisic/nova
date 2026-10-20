(() => {
  "use strict";
  const S = globalThis.Supernova;
  const lookupStreams = request => {
    const lookup = request.lookup;
    const settings = S.withStreamPreferences(request.settings || {},request.streamPreferences);
    if (!lookup || !["movie","series"].includes(lookup.mediaType) || !lookup.title || !lookup.mediaId) return [];
    if (lookup.mediaType === "series" && !(Number.isInteger(lookup.season) && lookup.season > 0 && Number.isInteger(lookup.episode) && lookup.episode > 0)) return [];
    const enabled = Object.keys(S.sources).filter(id => settings["sources." + id] === true);
    if (!enabled.length) return {streams:[],warnings:[{source:"Supernova",message:"Select a source in addon configuration."}]};
    const sourceConfiguration = enabled.includes("stremthru") ? {stremthru:settings["stremthru.url"] || "https://stremthru.13377001.xyz"} : {};
    const resultIdentity = JSON.stringify({v:S.VERSION,presentationVersion:4,lookup,enabled,sourceConfiguration,quality:settings["quality.preferred"],sort:S.sortOrder(settings),languages:settings["language.prefs"] || [],subtitles:settings["subtitle.prefs"] || [],codecs:settings["codec.prefs"] || [],seeders:settings["seeders.minimum"],mode:settings["playback.mode"],torbox:settings["torbox.enabled"]});
    const resultKey = "results:v2:" + nova.crypto.hmacSha256Base64Url(resultIdentity,"supernova-results-v2");
    const text = (value,limit) => typeof value === "string" && value.length > 0 && value.length <= limit;
    const only = (value,keys) => value && typeof value === "object" && !Array.isArray(value) && Object.keys(value).every(key => keys.includes(key));
    const safeValue = value => {
      if (typeof value === "string") {
        let decoded = value;
        try { decoded = decodeURIComponent(value.replace(/\+/g," ")); } catch (_) { /* Malformed escapes stay literal. */ }
        const secret = settings["torbox.apiKey"];
        return !/https?:\/\//i.test(decoded) && !(typeof secret === "string" && [secret,secret.trim()].some(key => key && (value.includes(key) || decoded.includes(key))));
      }
      if (Array.isArray(value)) return value.every(safeValue);
      return !value || typeof value !== "object" || Object.values(value).every(safeValue);
    };
    const validRow = row => {
      if (!only(row,["id","title","description","source","seeders","size","info_hash","file_idx","resolver","streamFacts"]) || !text(row.id,256) || !text(row.title,1024) || !text(row.description,4096) || !text(row.source,512) || !(row.seeders === null || S.number(row.seeders) !== null) || !(row.size === null || S.number(row.size) !== null) || !safeValue(row)) return false;
      if (row.streamFacts !== undefined && !S.validPreferenceFacts(row.streamFacts)) return false;
      if (row.resolver !== undefined) {
        if (row.info_hash !== undefined || row.file_idx !== undefined || !only(row.resolver,["provider_id","token"]) || row.resolver.provider_id !== "supernova" || !text(row.resolver.token,8192)) return false;
        const token = S.json(row.resolver.token);
        return only(token,["version","infoHash","title","fileName","lookup","sources"]) && token.version === S.VERSION && S.hash(token.infoHash) && text(token.title,1024) && text(token.fileName,2048) && JSON.stringify(token.lookup) === JSON.stringify(lookup) && Array.isArray(token.sources) && token.sources.length > 0 && token.sources.length <= 8 && token.sources.every(source => text(source,64)) && safeValue(token);
      }
      return !!S.hash(row.info_hash) && S.number(row.file_idx) !== null && S.number(row.file_idx) <= 4294967295;
    };
    let lastPublished = "", publicationCount = 0;
    const publish = (rows,final = false) => {
      const raw = JSON.stringify(rows);
      if (rows.length && raw !== lastPublished && (final || publicationCount < 12)) { nova.streams.publish(raw); lastPublished = raw; publicationCount++; }
    };
    const resultCache = S.resultCacheGet(resultKey);
    const resultNow = Math.floor(Date.now()/1000);
    if (only(resultCache,["streams","expiresAt"]) && Number.isInteger(resultCache.expiresAt) && resultCache.expiresAt > resultNow && resultCache.expiresAt <= resultNow + S.RESULT_TTL && Array.isArray(resultCache.streams) && resultCache.streams.length > 0 && resultCache.streams.length <= S.MAX && resultCache.streams.every(validRow)) {
      const rows = resultCache.streams.map(row=>({...row,description:row.resolver ? row.description : S.cachedDiscoveryDescription(row.description)}));
      publish(rows); return {streams:rows,cacheHit:true};
    }
    const started = Date.now();
    const deadline = started + 20000;
    // Optional discovery/inspection cannot consume the final availability
    // check or prevent already-proved direct files from reaching the caller.
    const discoveryDeadline = deadline - (settings["playback.mode"] === "cached" ? 500 : 6500);
    const optionalDeadline = discoveryDeadline - (settings["playback.mode"] === "direct" ? 0 : 3000);
    const identity = JSON.stringify({v:S.VERSION,fetchVersion:4,lookup,enabled,sourceConfiguration,quality:settings["quality.preferred"],sort:S.sortOrder(settings),languages:settings["language.prefs"] || [],subtitles:settings["subtitle.prefs"] || [],codecs:settings["codec.prefs"] || [],seeders:settings["seeders.minimum"]});
    const cacheKey = "lookup:v3:" + nova.crypto.hmacSha256Base64Url(identity,"supernova-matcher-v2");
    const cached = S.cacheGet(cacheKey);
    const now = Math.floor(started / 1000);
    const candidateRecords = Array.isArray(cached) ? cached : cached && cached.records;
    const validNumber = value => value === null || S.number(value) !== null;
    const validCache = Array.isArray(candidateRecords) && candidateRecords.length <= S.CANDIDATE_MAX && candidateRecords.every(record =>
      record && typeof record === "object" && S.hash(record.infoHash) && typeof record.title === "string" && record.title.length > 0 && record.title.length <= 1024 &&
      Array.isArray(record.sources) && record.sources.length > 0 && record.sources.length <= 8 && record.sources.every(source => typeof source === "string" && source.length > 0 && source.length <= 64) &&
      (record.animetoshoId === undefined || record.animetoshoId === null || (S.number(record.animetoshoId)>0 && S.number(record.animetoshoId)<=9999999999)) &&
      (record.trackMetadata === undefined || (S.validTrackMetadata(record.trackMetadata) && record.trackMetadata.fileName===record.fileName && record.trackMetadata.fileSize===record.sizeBytes)) &&
      (record.sourceMetadata === undefined || record.sourceMetadata === null || S.validReleaseEvidence(record.sourceMetadata)) &&
      (record.details === undefined || (Array.isArray(record.details) && record.details.length<=8 && record.details.every(S.validDetailReference))) &&
      (record.fileName === undefined || (typeof record.fileName === "string" && record.fileName.length > 0 && record.fileName.length <= 2048)) &&
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
    let sourceFailed = false, discoveryIncomplete = false;
    const warnings = sourceWarnings.slice();
    let torbox = [];
    const availableRows = new Map(), checkedHashes = new Set();
    let discoveryRequests=0;
    let descriptionRequests=0;
    const discoveryRequestLimit=48-(settings["playback.mode"] === "cached" ? 0 : 6);
    const currentRows = () => {
      const ranked = records || [];
      torbox = S.sortRows([...availableRows.values()],settings);
      const direct = settings["playback.mode"] === "cached" ? [] : ranked.filter(record => S.number(record.fileIdx) !== null).map(record => S.row(record,false));
      const rows = S.sortRows([...torbox.map(S.copyRow),...direct],settings).slice(0,S.MAX);
      if (discoveryCacheHit) for (const row of rows) if (!row.resolver) row.description = S.cachedDiscoveryDescription(row.description);
      return rows;
    };
    const enrichEvidence = () => {
      const used=S.enrichReleaseEvidence(records,availableRows,Math.min(discoveryDeadline,Date.now()+2200),Math.min(6-descriptionRequests,discoveryRequestLimit-discoveryRequests),()=>publish(currentRows()),enabled);
      descriptionRequests+=used;discoveryRequests+=used;
      records=S.rank(records,settings);
    };
    const checkNew = () => {
      if (settings["playback.mode"] === "direct") return;
      const fresh = records.filter(record=>!checkedHashes.has(record.infoHash));
      for (let offset=0;offset<fresh.length;offset+=150) {
        const budget=Math.min(3000,discoveryDeadline-Date.now());
        if (budget < 200) { discoveryIncomplete=true; break; }
        const remaining=discoveryRequestLimit-discoveryRequests;
        if (remaining < 1) { discoveryIncomplete=true;break; }
        const group=fresh.slice(offset,offset+Math.min(150,remaining*50));
        discoveryRequests+=Math.ceil(group.length/50);
        if (group.length < Math.min(150,fresh.length-offset)) discoveryIncomplete=true;
        group.forEach(record=>checkedHashes.add(record.infoHash));
        const merge = rows=>{for (const row of rows) availableRows.set(row.id,row);const ranked=currentRows();if(availableRows.size>S.CANDIDATE_MAX){const keep=new Set(torbox.slice(0,S.CANDIDATE_MAX).map(row=>row.id));for(const id of availableRows.keys())if(!keep.has(id))availableRows.delete(id);}publish(ranked);};
        merge(S.torboxOptions(group,lookup,settings,warnings,budget,merge));
      }
    };
    if (!discoveryCacheHit) {
      let pending = enabled.flatMap(id => (S.sources[id].queries ? S.sources[id].queries(lookup) : S.queries(lookup,S.sources[id].anime)).slice(0,2).map((query,variant) => ({id,query,variant,page:0,url:S.sources[id].url(query,lookup,0,settings)})));
      const merged = new Map();
      const failed = (search,message) => {
        sourceFailed = true;
        const name=S.sources[search.id].name;
        if (!sourceWarnings.some(warning => warning.source === name)) {
          const warning={source:name,message};sourceWarnings.push(warning);warnings.push(warning);
        }
      };
      const consume = (search,response) => {
        const source = S.sources[search.id];
        let parsed;
        try {
          if (!response || response.error || response.status < 200 || response.status >= 300) throw new Error("Unavailable source");
          parsed = source.parse(response.body);
        } catch (_) {
          if (!search.fallback && search.page === 0 && source.fallbackUrl) pending.push({...search,fallback:true,url:source.fallbackUrl(search.query)});
          else failed(search,"Source unavailable or returned an unsupported response. Other sources are still usable.");
          return;
        }
        for (const row of parsed) {
          if (!S.candidateMatches(row,lookup) || (row.seeders !== null && row.seeders < (settings["seeders.minimum"] || 0))) continue;
          const existing = merged.get(row.infoHash);
          if (existing) {
            existing.sources = [...new Set([...existing.sources,...row.sources])];
            if (row.seeders !== null) existing.seeders = Math.max(existing.seeders || 0,row.seeders);
            if (!existing.torrentUrl) existing.torrentUrl = row.torrentUrl;
            if (!existing.animetoshoId && S.number(row.animetoshoId)>0) existing.animetoshoId=row.animetoshoId;
            S.mergeRecordEvidence(existing,row);
          } else merged.set(row.infoHash,{...row,fileIdx:null});
        }
        if (!search.fallback && source.pages) {
          try {
            const pages=Math.min(search.variant === 0 ? 6 : 2,source.pages(response.body));
            if (search.page+1 < pages) pending.push({...search,page:search.page+1,url:source.url(search.query,lookup,search.page+1,settings)});
          } catch (_) { failed(search,"Source pagination could not be read. Other sources are still usable."); }
        }
      };
      // Check and publish first-page discoveries before following deeper pages.
      // Each later source wave admits only bounded new hashes; checked hashes
      // and verified rows remain operation-local, never account response objects.
      let first=true;
      while (pending.length && Date.now() < optionalDeadline-350 && discoveryRequests < discoveryRequestLimit-3) {
        const count=Math.min(first ? pending.length : 3,discoveryRequestLimit-discoveryRequests-3);
        const wave=pending.splice(0,count);
        first=false;
        discoveryRequests+=wave.length;
        const responses=S.batch(wave.map(search=>({url:search.url,method:"GET"})),optionalDeadline);
        wave.forEach((search,index)=>consume(search,responses[index]));
        const all=S.rank([...merged.values()],settings);
        if (all.length > S.CANDIDATE_MAX) discoveryIncomplete=true;
        records=all.slice(0,S.CANDIDATE_MAX);
        merged.clear();for (const record of records) merged.set(record.infoHash,record);
        checkNew();
        enrichEvidence();
        pending.sort((a,b)=>a.variant-b.variant || a.page-b.page);
      }
      if (pending.length) discoveryIncomplete=true;
      records=S.rank([...merged.values()],settings).slice(0,S.CANDIDATE_MAX);
    } else {
      records=S.rank(records.filter(record=>S.candidateMatches(record,lookup)),settings).slice(0,S.CANDIDATE_MAX);
      publish(currentRows());
      checkNew();
      enrichEvidence();
    }
    const inspectionDeadline = deadline - 500;
    const toInspect = settings["playback.mode"] !== "cached" ? records.filter(record => S.number(record.fileIdx) === null && record.torrentUrl).sort((a,b)=>(a.metainfoAttemptAt || 0)-(b.metainfoAttemptAt || 0)).slice(0,6) : [];
    let verificationFailed = false;
    S.batch(toInspect.map(record => ({url:record.torrentUrl,method:"GET",responseType:"binary"})),inspectionDeadline,(index,response) => {
      const record = toInspect[index];
      record.metainfoAttemptAt = Date.now();
      if (!response || response.error || response.status !== 200 || !response.bodyBase64) { verificationFailed = true; return; }
      let metadata;
      try { metadata = S.json(nova.torrent.inspect(response.bodyBase64)); } catch (_) { verificationFailed = true; return; }
      if (!metadata || S.hash(metadata.infoHash) !== record.infoHash) { verificationFailed = true; return; }
      const file = S.chooseFile(metadata.files,record,lookup,false);
      if (file) { record.fileIdx = file.selectedId; record.sizeBytes = file.size; record.fileName = String(file.name || file.path || record.title).slice(0,2048); publish(currentRows()); }
      else verificationFailed = true;
    });
    // Results are already published; site probes are optional and retain the
    // existing operation deadline and 48-request budget. Only selected files
    // qualify, and metadata failures never turn a proved stream into an error.
    publish(currentRows());
    S.enrichTrackMetadata(records,availableRows,Math.min(deadline-500,Date.now()+2800),48-discoveryRequests-toInspect.length,()=>publish(currentRows()));
    if (!discoveryCacheHit) expiresAt = Math.floor(Date.now()/1000) + (sourceFailed || discoveryIncomplete || verificationFailed || !records.length ? 60 : 86400);
    const remainingTtl = Math.max(0,expiresAt - Math.floor(Date.now()/1000));
    // Failed empty discovery must retry immediately. Updating file proof on a
    // cache hit never renews the original discovery TTL indefinitely.
    if ((!discoveryCacheHit || toInspect.length) && remainingTtl > 0 && !(sourceFailed && !records.length)) S.cacheSet(cacheKey,{records:records.map(record=>{const {trackMetadata,...discovery}=record;return discovery;}),warnings:sourceWarnings,discoveryExpiresAt:expiresAt},remainingTtl);
    const streams = currentRows();
    publish(streams,true);
    // This short disk cache stores proved list rows, never resolved playback links.
    // Errors and empty results retry immediately; selecting TorBox always resolves
    // afresh. The service supplies settings/clear invalidation and bounded LRU.
    // A partial proved list must not hide the next bounded metainfo retry.
    // Also retain discovery's original expiry, including its short retry TTL
    // when pagination, request budgets or source work could not finish.
    const pendingProof = settings["playback.mode"] !== "cached" && records.some(record=>S.number(record.fileIdx) === null && record.torrentUrl);
    const resultTtl = Math.min(S.RESULT_TTL,remainingTtl);
    if (!warnings.length && !pendingProof && resultTtl > 0 && streams.length && streams.every(validRow)) S.resultCacheSet(resultKey,{streams,expiresAt:Math.floor(Date.now()/1000)+resultTtl},resultTtl);
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
