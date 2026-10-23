/* Independent Nova port of Yuzono's ReAnime source; no browser or remote code execution. */
(() => {
  "use strict";
  const ID = "reanime";
  const BASE = "https://reanime.to";
  const FLIX = "https://flixcloud.cc";
  const DEC = "https://enc-dec.app/api";
  const FALLBACK_MASK = [157,42,241,71,179,142,92,112,166,25,228,59,216,98,15,197];
  function http(url, data) {
    const origin = url.startsWith(BASE + "/") ? BASE : url.startsWith(FLIX + "/") ? FLIX : "https://enc-dec.app";
    const headers = {Accept: "application/json, text/plain, */*", Referer: origin + "/", Origin: origin};
    const raw = JSON.parse(data === undefined ? nova.http.get(url, JSON.stringify(headers)) : nova.http.request(JSON.stringify({
      url, method: "POST", headers, body: {type: "json", value: data},
    })));
    if (raw.error) throw new Error(raw.error);
    if (raw.status < 200 || raw.status >= 300) throw new Error("Re:ANIME request failed (HTTP " + raw.status + ")");
    return raw.body;
  }
  function api(path) { return JSON.parse(http(BASE + "/api/v1" + path)); }
  function slug(value) {
    if (!/^[a-zA-Z0-9][a-zA-Z0-9_-]{0,255}$/.test(String(value || ""))) throw new Error("Invalid Re:ANIME title ID");
    return value;
  }
  function positive(value) { return Number.isSafeInteger(Number(value)) && Number(value) > 0 ? Number(value) : null; }
  function media(dto) {
    const title = dto.title || {};
    const aliases = [...new Set([title.romaji, title.native, title.english, ...(dto.synonyms || [])].filter(v => typeof v === "string" && v))];
    return {
      provider_id: ID, source_id: slug(dto.anime_id), media_type: "series",
      title: title.english || title.romaji || title.native || dto.anime_id,
      aliases, year: dto.start_date?.year ? String(dto.start_date.year) : dto.season_year ? String(dto.season_year) : null,
      poster: dto.cover_image?.extra_large || dto.cover_image?.large || null,
      background: dto.banner_image || null,
      description: dto.description ? JSON.parse(nova.html.select("<div>" + dto.description + "</div>", "div", JSON.stringify({text:{selector:"",value:"text"}})))[0]?.text || dto.description.replace(/<[^>]*>/g, " ") : null,
      genres: Array.isArray(dto.genres) ? dto.genres : [],
      external_ids: {imdb: /^tt\d+$/.test(dto.imdb_id || "") ? dto.imdb_id : null,
        mal: positive(dto.mal_id) ? String(dto.mal_id) : null,
        anilist: positive(dto.anilist_id) ? String(dto.anilist_id) : null, other: {}},
    };
  }
  function catalog(request) {
    const extra = request.extra || {};
    const skip = Math.max(0, Math.floor(Number(extra.skip) || 0));
    if (skip > 100000) return [];
    if (request.catalogId === ID + ".latest") {
      // The site uses cursors. Retain item offsets, and never replay page one
      // when a requested continuation has no known cursor.
      const key = "latest:" + skip;
      const cursor = skip ? nova.storage.get(key) : "";
      if (skip && !cursor) return [];
      const result = api("/home/latest-aired?limit=12&lang=" + (cursor ? "&cursor=" + encodeURIComponent(cursor) : ""));
      const rows = (result.data || []).map(media);
      if (result.has_more && result.next_cursor && rows.length) nova.storage.set("latest:" + (skip + rows.length), result.next_cursor);
      else nova.storage.set("latest:" + (skip + rows.length), "");
      return rows;
    }
    if (![ID + ".popular", ID + ".search"].includes(request.catalogId)) throw new Error("Unknown Re:ANIME catalog");
    const query = String(extra.search || "").trim();
    if (request.catalogId === ID + ".search" && !query) return [];
    return (api("/search?sort=popularity_desc&limit=36&offset=" + skip + (query ? "&q=" + encodeURIComponent(query) : "")).results || []).map(media);
  }
  function episodeId(show, number) { return "ep:" + nova.crypto.base64UrlEncode(JSON.stringify({slug: show, number})); }
  function episodeIdentity(id) {
    if (!String(id || "").startsWith("ep:")) throw new Error("Invalid Re:ANIME episode ID");
    const identity = JSON.parse(nova.crypto.base64UrlDecode(id.slice(3)));
    slug(identity.slug);
    if (!positive(identity.number)) throw new Error("Invalid Re:ANIME episode number");
    return identity;
  }
  function nativeDetails(sourceId) {
    const show = slug(sourceId);
    const dto = api("/anime/" + show);
    if (dto.anime_id !== show) throw new Error("Re:ANIME returned a different title");
    const rows = api("/anime/" + show + "/episodes?limit=2000").data || [];
    const seen = new Set();
    const episodes = rows.filter(ep => positive(ep.episode_number) && ep.playable !== false).map(ep => ({
      provider_id: ID, source_id: episodeId(show, ep.episode_number), parent_id: show,
      number: Number(ep.episode_number), season: 1,
      title: ep.title || ep.title_romaji || "Episode " + ep.episode_number,
      released: ep.aired || null, thumbnail: ep.thumbnail || null,
    })).filter(ep => { if (seen.has(ep.number)) return false; seen.add(ep.number); return true; }).sort((a,b) => a.number - b.number);
    return {media: media(dto), episodes, anilistId: positive(dto.anilist_id), declaredCount: positive(dto.episodes_total), relations: dto.relations || [], format: dto.format, externalSeasons: dto.external_seasons || {}};
  }
  function sequence(details) {
    return {mediaId: ID + ":" + details.media.source_id, title: details.media.title, aliases: details.media.aliases,
      year: details.media.year, season: sourceIdentity(details).season, part: sourceIdentity(details).part, declaredCount: details.declaredCount,
      // The API can append OVAs after the declared regular inventory (e.g.
      // Steins;Gate 24 + episode 25). Keep native IDs/playback, but never let
      // that trailing inventory invalidate or shift canonical regular episodes.
      episodes: details.episodes.filter(ep => !details.declaredCount || ep.number <= details.declaredCount).map(ep => ({id: ID + ":" + ep.source_id, number: ep.number, title: ep.title, released: ep.released}))};
  }
  function details(request) {
    const native = nativeDetails(request.sourceId);
    if (!nova.metadata.available()) return native;
    nova.metadata.checkpoint(JSON.stringify({media:native.media, episodes:native.episodes}));
    try {
      const enrichment = JSON.parse(nova.metadata.enrich(JSON.stringify({details:{media:native.media,episodes:native.episodes},
        sourceSequences:lookupCandidates({title:native.media.title, year:native.media.year}, native).map(sequence), query:native.media.title, targets:["imdb","tmdb:tv","mal:anime","anilist:anime"]})));
      if (!enrichment.error && enrichment.details) return {...native, media:enrichment.details.media, enrichment};
    } catch (_) { /* Optional metadata never hides the native inventory. */ }
    return native;
  }
  function streams(request) {
    const identity = episodeIdentity(request.episodeId);
    const native = nativeDetails(identity.slug);
    if (!native.anilistId || !native.episodes.some(ep => ep.number === identity.number)) return [];
    const result = JSON.parse(http(BASE + "/api/flix/" + native.anilistId + "/" + identity.number));
    if (!result.success) return [];
    const seen = new Set();
    return (result.servers || []).filter(server => {
      if (!/^https:\/\/flixcloud\.cc\/e\/[a-z0-9]+(?:\?[^#]*)?$/.test(server.dataLink || "")) return false;
      const key = server.dataLink + ":" + server.dataType;
      if (seen.has(key)) return false; seen.add(key); return true;
    }).slice(0,16).map(server => ({
      id: ID + ":" + identity.slug + ":" + identity.number + ":" + nova.crypto.base64UrlEncode(server.dataLink + ":" + server.dataType),
      title: "Re:ANIME " + (server.serverName || "FlixCloud") + " [" + (server.dataType || "sub") + "]" + (server.softsub ? " [Softsub]" : ""),
      description: "FlixCloud HLS", source: "Re:ANIME",
      resolver: {provider_id:ID, token:nova.crypto.base64UrlEncode(JSON.stringify({episode:request.episodeId,link:server.dataLink,audio:server.dataType || "sub"}))},
    }));
  }
  function normalize(value) {
    return String(value || "").normalize("NFKD").replace(/[\u0300-\u036f]/g, "")
      .toLowerCase().replace(/[^\p{L}\p{N}]+/gu," ").trim();
  }
  function identity(value) {
    let base = normalize(value), season = 1, part = 1, labeled = false;
    const partMatch = base.match(/\s+(?:(?:part|cour)\s+(\d+)|(\d+)(?:st|nd|rd|th)\s+(?:part|cour))(?:\s|$)/);
    if (partMatch) {part = Number(partMatch[1] || partMatch[2]); base = base.slice(0,partMatch.index).trim(); labeled = true;}
    const seasonMatch = base.match(/\s+(?:(?:season|series)\s+(\d+)|(\d+)(?:st|nd|rd|th)\s+season)(?:\s|$)/);
    if (seasonMatch) {season = Number(seasonMatch[1] || seasonMatch[2]); base = base.slice(0,seasonMatch.index).trim(); labeled = true;}
    const roman = base.match(/\s+(ii|iii|iv|v|vi)$/);
    if (roman) {season = ["i","ii","iii","iv","v","vi"].indexOf(roman[1])+1; base = base.slice(0,roman.index).trim(); labeled = true;}
    return {base,season,part,labeled,seasonKnown:!!seasonMatch || !!roman};
  }
  function sourceIdentity(details) {
    const own = identity(details.media.title);
    // Prefer the source's explicit season over Roman numerals embedded before
    // a translated subtitle. TMDB season labels only fill an absent label.
    if (!own.seasonKnown && positive(details.externalSeasons?.tmdb)) own.season = Number(details.externalSeasons.tmdb);
    return own;
  }
  function names(row) {
    const title = row.title || {};
    return [title.english,title.romaji,title.native,...(row.synonyms || [])].filter(v => typeof v === "string" && v);
  }
  function lookupCandidates(lookup, seed = null) {
    const title = String(lookup.title || "").trim();
    if (!title) return seed ? [seed] : [];
    const expected = identity(title), normalized = normalize(title);
    const target = lookup.season === 1 && expected.labeled ? expected.season : Number(lookup.season || expected.season);
    const shorter = identity(title.split(":")[0]).base;
    const queries = [...new Set([title, shorter.length >= 4 ? shorter : expected.base])].slice(0,2);
    const candidates = new Map();
    function add(row, opened = false) {
      const format = String(row.format || "").toUpperCase();
      if (!opened && format && !["TV","ONA"].includes(format)) return;
      const titles = names(row);
      const identities = titles.map(identity);
      const exact = titles.some(t => normalize(t) === normalized);
      const family = identities.some(i => i.base === expected.base);
      const related = expected.base.length >= 4 && identities.some(i => i.base.startsWith(expected.base + " ") || expected.base.startsWith(i.base + " "));
      if (!opened && !exact && !family && !related) return;
      const own = identity(titles[0] || "");
      const score = (opened ? 10000 : 0) + (family ? 1000 : 0) + (own.season === target ? 500 : 0) + (exact ? 100 : 0);
      if (row.anime_id && (!candidates.has(row.anime_id) || candidates.get(row.anime_id).score < score)) candidates.set(row.anime_id,{row,score});
    }
    if (seed) add({anime_id:seed.media.source_id,title:{english:seed.media.title},synonyms:seed.media.aliases,format:seed.format},true);
    for (const query of queries) {
      try { for (const row of (api("/search?q=" + encodeURIComponent(query) + "&limit=36&offset=0").results || []).slice(0,36)) add(row); }
      catch (_) { /* A failed alternative query must not erase known candidates. */ }
    }
    const loaded = [];
    const attempted = new Set();
    // Search ranking is not identity proof. Retain up to six useful family
    // inventories for Rust's existing ambiguity/count/title-anchor checks.
    while (loaded.length < 6) {
      const next = [...candidates.values()].filter(c => !attempted.has(c.row.anime_id)).sort((a,b) => b.score-a.score)[0];
      if (!next || attempted.size >= 6) break;
      attempted.add(next.row.anime_id);
      try {
        const details = seed?.media.source_id === next.row.anime_id ? seed : nativeDetails(next.row.anime_id);
        if (!sequence(details).episodes.length) continue;
        loaded.push(details);
        for (const related of details.relations) if (["PREQUEL","SEQUEL"].includes(related.relation_type)) add(related);
      } catch (_) { /* Keep other candidates if one source entry is unavailable. */ }
    }
    return loaded;
  }
  function lookupStreams(request) {
    const lookup = request.lookup || {};
    const loaded = lookupCandidates(lookup);
    const match = JSON.parse(nova.metadata.resolveEpisode(JSON.stringify({target:lookup, sourceSequences:loaded.map(details => {
      const candidate = sequence(details);
      // Shared show identity can establish alternate series names, never an
      // episode offset. The host still proves season/count/part alignment.
      if (/^tt\d+$/.test(lookup.mediaId || "") && details.media.external_ids.imdb) {
        if (details.media.external_ids.imdb !== lookup.mediaId) return null;
        candidate.aliases = [...new Set([...candidate.aliases, lookup.title])];
      }
      return candidate;
    }).filter(Boolean)})));
    if (match.status !== "confirmed" || !String(match.sourceEpisodeId || "").startsWith(ID + ":")) return [];
    return streams({episodeId:match.sourceEpisodeId.slice(ID.length + 1)});
  }
  // Convert the restricted Svelte data literal without evaluating site JavaScript.
  // Quoted values are copied verbatim so embedded colons/commas remain intact.
  function dataLiteral(raw) {
    let output = "", i = 0;
    while (i < raw.length) {
      if (raw[i] === '"') {
        const start = i++;
        while (i < raw.length) { if (raw[i] === "\\") { i += 2; continue; } if (raw[i++] === '"') break; }
        output += raw.slice(start,i); continue;
      }
      const ident = raw.slice(i).match(/^[A-Za-z_$][\w$]*/)?.[0];
      if (ident) {
        const rest = raw.slice(i + ident.length);
        output += /^\s*:/.test(rest) ? JSON.stringify(ident) : ident === "undefined" ? "null" : ident;
        i += ident.length; continue;
      }
      if (raw[i] === "," && /^,\s*[}\]]/.test(raw.slice(i))) { i++; continue; }
      output += raw[i++];
    }
    return JSON.parse(output);
  }
  function resolveStream(request) {
    const token = JSON.parse(nova.crypto.base64UrlDecode(request.token || ""));
    episodeIdentity(token.episode);
    if (!/^https:\/\/flixcloud\.cc\/e\/[a-z0-9]+(?:\?[^#]*)?$/.test(token.link || "")) throw new Error("Invalid FlixCloud server");
    const html = http(token.link);
    const literal = html.match(/type:\s*"data",\s*data:\s*(\{[\s\S]*?\})\s*,\s*uses:/)?.[1];
    if (!literal) throw new Error("FlixCloud embed data is unavailable");
    const data = dataLiteral(literal);
    const subtitles = (data.subtitles || []).filter(sub => /^https:\/\//.test(sub.url || "")).map(sub => ({url:sub.url,language:sub.language || "Unknown"}));
    let mask = FALLBACK_MASK;
    const script = html.match(/href="([^"\s]*hls\.js[^"\s]*)"/)?.[1];
    if (script) {
      // Only request the declared FlixCloud host, including when an embed
      // supplies an absolute script path. The script is parsed, never executed.
      const scriptUrl = script.startsWith("/") && !script.startsWith("//") ? FLIX + script : script;
      if (scriptUrl.startsWith(FLIX + "/")) {
        try {
          const values = http(scriptUrl).match(/for\(var f=\[(\d{1,3}(?:,\d{1,3}){15})\]/)?.[1]?.split(",").map(Number);
          if (values?.length === 16 && values.every(v => v >= 0 && v <= 255)) { mask = values; nova.storage.set("flixMask",JSON.stringify(mask)); }
        } catch (_) { /* Retain the known mask if the optional script fails. */ }
      }
    }
    if (mask === FALLBACK_MASK) {
      try { const saved = JSON.parse(nova.storage.get("flixMask") || "null");
        if (saved?.length === 16 && saved.every(v => Number.isInteger(v) && v >= 0 && v <= 255)) mask = saved;
      } catch (_) { /* Invalid cached masks are ignored. */ }
    }
    delete data.subtitles; delete data.intro_chapter; delete data.outro_chapter;
    const tokenResponse = JSON.parse(http(DEC + "/dec-flixcloud?type=token", {data}));
    if (tokenResponse.status !== 200 || !tokenResponse.result?.token) throw new Error("FlixCloud token resolution failed");
    const encrypted = JSON.parse(http(FLIX + "/api/m3u8/" + encodeURIComponent(tokenResponse.result.token)));
    const decoded = JSON.parse(http(DEC + "/dec-flixcloud?type=stream", {data:{context:tokenResponse.result.context,stream_response:encrypted}}));
    if (decoded.status !== 200 || !decoded.result?.stream || !decoded.result.context?.w_payload) throw new Error("FlixCloud stream resolution failed");
    return {id:"reanime:resolved", title:"Re:ANIME [" + String(token.audio || "sub") + "]", url:decoded.result.stream,
      headers:{Referer:FLIX + "/",Origin:FLIX}, subtitles,
      hlsProxy:{mask,wPayload:decoded.result.context.w_payload}};
  }
  globalThis.novaProvider = {handle(request) {
    const config = {streams:true,metadata:true,...request.config};
    if (request.op === "manifest") {
      const manifest = JSON.parse(JSON.stringify(request.manifest));
      manifest.resources = manifest.resources.filter(r => (typeof r === "string" ? r : r.name) === "stream" ? config.streams : config.metadata);
      if (!config.metadata) manifest.catalogs = [];
      if (!config.metadata && !config.streams) manifest.types = [];
      return manifest;
    }
    if (!config.metadata && request.op === "catalog") return [];
    if (!config.metadata && request.op === "details") return null;
    if (!config.streams && ["streams","lookupStreams","resolveStream"].includes(request.op)) return [];
    switch(request.op) {
      case "catalog": return catalog(request);
      case "details": return details(request);
      case "streams": return streams(request);
      case "lookupStreams": return lookupStreams(request);
      case "resolveStream": return resolveStream(request);
      default: throw new Error("Unsupported Re:ANIME operation: " + request.op);
    }
  }};
})();
