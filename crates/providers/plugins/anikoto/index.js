/* Bundled AniKoto source. It receives only the `nova` host capabilities. */
(() => {
  "use strict";

  const DOMAINS = [
    "anikototv.to",
    "anikoto.bz",
    "anikoto.cz",
    "anikoto.me",
    "anikoto.net",
    "anikototv.se",
  ];
  const PROVIDER_ID = "anikoto";
  const ROOT_FIELDS = {
    href: { selector: "a.name", value: "href" },
    title: { selector: "a.name", value: "text" },
    englishTitle: { selector: "a.name", value: "data-en" },
    japaneseTitle: { selector: "a.name", value: "data-jp" },
    format: { selector: "div.right", value: "text" },
    poster: { selector: "div.poster img", value: "data-src" },
    posterFallback: { selector: "div.poster img", value: "src" },
  };

  function rows(html, selector, fields) {
    return JSON.parse(nova.html.select(html, selector, JSON.stringify(fields)));
  }

  function httpGet(url, headers = {}) {
    const response = JSON.parse(nova.http.get(url, JSON.stringify(headers)));
    if (response.error) throw new Error(response.error);
    if (response.status < 200 || response.status >= 300) {
      throw new Error(`host returned HTTP ${response.status}`);
    }
    return response;
  }

  function siteGet(path, headers = {}) {
    const preferred = nova.storage.get("domain");
    const order = preferred && DOMAINS.includes(preferred)
      ? [preferred, ...DOMAINS.filter((domain) => domain !== preferred)]
      : DOMAINS;
    let lastError = "AniKoto is unavailable";
    for (const domain of order) {
      const base = `https://${domain}`;
      const raw = JSON.parse(nova.http.get(`${base}${path}`, JSON.stringify(headers)));
      if (raw.error) {
        lastError = raw.error;
        continue;
      }
      if (raw.status < 200 || raw.status >= 400) {
        lastError = `AniKoto returned HTTP ${raw.status}`;
        continue;
      }
      const finalDomain = String(raw.url || "").match(/^https:\/\/([^/:]+)/i)?.[1] || domain;
      const selectedDomain = DOMAINS.includes(finalDomain) ? finalDomain : domain;
      nova.storage.set("domain", selectedDomain);
      return { body: raw.body, base: `https://${selectedDomain}`, finalUrl: raw.url };
    }
    throw new Error(lastError);
  }

  function sourcePath(href) {
    const match = String(href || "").match(/(?:https?:\/\/[^/]+)?(\/watch\/[^?#]+)/i);
    return match ? match[1].replace(/\/$/, "").replace(/\/ep-\d+(?:\.\d+)?$/, "") : "";
  }

  function absoluteUrl(value, base) {
    value = String(value || "").trim();
    if (!value) return "";
    if (/^https:\/\//i.test(value)) return value;
    if (/^http:\/\//i.test(value)) return value.replace(/^http:/i, "https:");
    if (value.startsWith("//")) return `https:${value}`;
    return `${base}${value.startsWith("/") ? "" : "/"}${value}`;
  }

  function cardFromRow(row) {
    const path = sourcePath(row.href);
    if (!path || !row.title) return null;
    const currentDomain = nova.storage.get("domain") || DOMAINS[0];
    const base = `https://${currentDomain}`;
    const poster = absoluteUrl(row.poster || row.posterFallback, base);
    return {
      provider_id: PROVIDER_ID,
      source_id: nova.crypto.base64UrlEncode(path),
      media_type: "series",
      title: (row.englishTitle || row.title).trim(),
      aliases: row.japaneseTitle ? [row.japaneseTitle.trim()] : [],
      year: null,
      poster: poster || null,
      background: null,
      description: null,
      genres: [],
      external_ids: { imdb: null, tmdb: null, mal: null, other: {} },
    };
  }

  function vrfEncrypt(input) {
    let value = String(input);
    value = exchange(value, "AP6GeR8H0lwUz1", "UAz8Gwl10P6ReH");
    value = nova.crypto.rc4Base64Url(value, "ItFKjuWokn4ZpB");
    value = nova.crypto.rc4Base64Url(value, "fOyt97QWFB3");
    value = exchange(value, "1majSlPQd2M5", "da1l2jSmP5QM");
    value = exchange(value, "CPYvHj09Au3", "0jHA9CPYu3v");
    value = value.split("").reverse().join("");
    value = nova.crypto.rc4Base64Url(value, "736y1uTJpBLUX");
    const encoded = nova.crypto.base64UrlEncode(value);
    return encodeURIComponent(encoded + "=".repeat((4 - encoded.length % 4) % 4));
  }

  function exchange(input, from, to) {
    return input.split("").map((character) => {
      const index = from.indexOf(character);
      return index < 0 ? character : to[index];
    }).join("");
  }

  function catalog(request) {
    const extra = request.extra || {};
    const skip = Math.max(0, Math.floor(Number(extra.skip || 0)) || 0);
    const query = String(extra.search || "").trim();
    if (!["anikoto.latest", "anikoto.popular", "anikoto.search"].includes(request.catalogId)) {
      throw new Error("unknown AniKoto catalog");
    }
    if (request.catalogId === "anikoto.search" && !query) return [];
    const key = `pagination:${request.catalogId}`;
    let state;
    try { state = JSON.parse(nova.storage.get(key) || "null"); } catch (_) { state = null; }
    if (!state || state.query !== query || skip === 0) state = { query, size: 0, end: null };
    function fetchPage(page) {
      const path = query
        ? `/filter?keyword=${encodeURIComponent(query)}&page=${page}&vrf=${vrfEncrypt(query)}`
        : request.catalogId === "anikoto.latest"
          ? `/latest-updated/?page=${page}` : `/most-viewed/?page=${page}`;
      const result = siteGet(path, { Referer: "https://" + (nova.storage.get("domain") || DOMAINS[0]) + "/" });
      const cards = rows(result.body, "div.ani.items > div.item", ROOT_FIELDS)
        .map(cardFromRow).filter(Boolean).slice(0, 80);
      return { cards, hasNext: rows(result.body, "nav > ul.pagination > li.active ~ li", {}).length > 0 };
    }
    let first;
    // Nova sends an item offset. Learn the site's actual page size instead
    // of assuming that every source returns twenty results.
    if (!state.size) {
      first = fetchPage(1);
      state.size = first.cards.length;
      if (!first.hasNext) state.end = first.cards.length;
    }
    if (!state.size || (state.end !== null && skip >= state.end)) {
      nova.storage.set(key, JSON.stringify(state));
      return [];
    }
    const page = Math.floor(skip / state.size) + 1;
    const result = page === 1 && first ? first : fetchPage(page);
    if (!result.hasNext) state.end = (page - 1) * state.size + result.cards.length;
    nova.storage.set(key, JSON.stringify(state));
    return result.cards.slice(skip % state.size);
  }

  function loadSeries(sourceId) {
    const path = nova.crypto.base64UrlDecode(sourceId || "");
    if (!path.startsWith("/watch/")) throw new Error("invalid AniKoto media id");
    const pageResult = siteGet(path, { Referer: "https://" + (nova.storage.get("domain") || DOMAINS[0]) + "/" });
    const fields = {
      title: { selector: "h1.title, h2.title", value: "text" },
      englishTitle: { selector: "h1.title, h2.title", value: "data-en" },
      dataId: { selector: "[data-id]", value: "data-id" },
      dataTip: { selector: "[data-tip]", value: "data-tip" },
      synopsis: { selector: "div.synopsis > div.shorting > div.content", value: "text" },
      poster: { selector: "div.poster img", value: "data-src" },
      posterFallback: { selector: "div.poster img", value: "src" },
      mal: { selector: "a[href*='myanimelist.net/anime/']", value: "href" },
      aliases: { selector: "div.names.font-italic", value: "text" },
      genres: { selector: "div.bmeta a[href*='/genre/'], div.bmeta a[href*='genre=']", value: "texts" },
      year: { selector: "div.bmeta a[href*='/year/'], div.bmeta a[href*='year='], div.bmeta a[href*='year['], div.bmeta a[href*='year%5B']", value: "text" },
      metadata: { selector: "div.bmeta > div.meta > div, div.bmeta > div:not(.meta)", value: "texts" },
    };
    const page = rows(pageResult.body, "html", fields)[0] || {};
    page.year = page.year || String(page.metadata || "").match(/(?:Premiered|Aired):\s*[^:]*?\b((?:19|20)\d{2})\b/i)?.[1] || "";
    page.episodeCount = Number(String(page.metadata || "").match(/\bEpisodes:\s*(\d+)\b/i)?.[1]) || null;
    const animeId = page.dataId || page.dataTip;
    if (!animeId) throw new Error("AniKoto media page had no anime id");

    const currentBase = pageResult.finalUrl.match(/^https:\/\/[^/]+/i)?.[0] || pageResult.base;
    const episodeList = siteGet(`/ajax/episode/list/${encodeURIComponent(animeId)}?vrf=${vrfEncrypt(animeId)}`, {
      Accept: "application/json, text/javascript, */*; q=0.01",
      Referer: `${currentBase}${path}`,
      "X-Requested-With": "XMLHttpRequest",
    });
    const payload = JSON.parse(episodeList.body);
    const fragment = typeof payload.result === "string" ? payload.result : (payload.html || "");
    const episodeRows = rows(fragment, "div.episodes ul > li", {
      number: { selector: "a", value: "data-num" },
      serverIds: { selector: "a", value: "data-ids" },
      title: { selector: "span.d-title", value: "text" },
      mal: { selector: "a", value: "data-mal" },
      slug: { selector: "a", value: "data-slug" },
      timestamp: { selector: "a", value: "data-timestamp" },
      filler: { selector: "a", value: "class" },
    });
    return { path, page, currentBase, episodeRows };
  }

  function releaseDate(timestamp) {
    const seconds = Number(timestamp);
    if (!Number.isFinite(seconds) || seconds <= 0) return null;
    const date = new Date(seconds * 1000);
    return Number.isFinite(date.getTime()) ? date.toISOString() : null;
  }

  function details(request) {
    const series = loadSeries(request.sourceId);
    const { path, page, currentBase, episodeRows } = series;
    const mediaSourceId = request.sourceId;
    const episodes = episodeRows
      .map((episode) => {
        const number = Number(episode.number);
        if (!episode.number || !Number.isInteger(number) || number < 0 || !episode.serverIds) return null;
        // Server IDs and mapper timestamps expire. Persist only the show's
        // path and episode number so metadata refreshes keep watch history.
        const source = {
          mediaPath: path,
          number,
        };
        const title = String(episode.title || "").trim();
        return {
          provider_id: PROVIDER_ID,
          source_id: `ep:${nova.crypto.base64UrlEncode(JSON.stringify(source))}`,
          parent_id: mediaSourceId,
          number,
          season: 1,
          title: title ? `Episode ${number}: ${title}` : `Episode ${number}`,
          released: releaseDate(episode.timestamp),
          thumbnail: null,
        };
      })
      .filter(Boolean)
      .sort((a, b) => a.number - b.number);

    const malId = String(page.mal || "").match(/myanimelist\.net\/anime\/(\d+)/i)?.[1]
      || episodeRows.find((episode) => /^\d+$/.test(episode.mal || ""))?.mal || null;
    const aliases = String(page.aliases || "").split(";").map((value) => value.trim()).filter(Boolean);
    const poster = absoluteUrl(page.poster || page.posterFallback, currentBase);
    return {
      media: {
        provider_id: PROVIDER_ID,
        source_id: mediaSourceId,
        media_type: "series",
        title: String(page.englishTitle || page.title || "").trim(),
        aliases,
        year: String(page.year || "").match(/\b\d{4}\b/)?.[0] || null,
        poster: poster || null,
        background: null,
        description: String(page.synopsis || "").trim() || null,
        genres: String(page.genres || "").split(",").map((value) => value.trim()).filter(Boolean),
        external_ids: { imdb: null, tmdb: null, mal: malId, other: {} },
      },
      episodes,
      // This context stays inside the host's metadata operation. It lets the
      // reverse mapper reuse this fetch, including current availability.
      mappingContext: series,
      mappingQuery: seriesIdentity(page.englishTitle || page.title).base,
    };
  }

  function streams(request, loadedSeries = null) {
    const token = String(request.episodeId || "");
    if (!token.startsWith("ep:")) return [];
    const identity = JSON.parse(nova.crypto.base64UrlDecode(token.slice(3)));
    if (!String(identity.mediaPath || "").startsWith("/watch/")
      || !Number.isInteger(identity.number) || identity.number < 0) throw new Error("invalid AniKoto episode id");
    const series = loadedSeries || loadSeries(nova.crypto.base64UrlEncode(identity.mediaPath));
    const currentEpisode = series.episodeRows.find((row) => Number(row.number) === identity.number);
    if (!currentEpisode || !currentEpisode.serverIds) return [];
    const episode = { ...currentEpisode, number: identity.number };
    const base = series.currentBase;
    const referer = `${base}${series.path}/ep-${episode.number}`;
    const headers = {
      Accept: "application/json, text/javascript, */*; q=0.01",
      Referer: referer,
      "X-Requested-With": "XMLHttpRequest",
    };
    const response = siteGet(`/ajax/server/list?servers=${encodeURIComponent(episode.serverIds)}`, headers);
    const payload = JSON.parse(response.body);
    const fragment = typeof payload.result === "string" ? payload.result : (payload.html || "");
    const serverRows = rows(fragment, "div.servers > div.type", {
      label: { selector: "label", value: "text" },
      type: { selector: "", value: "data-type" },
      html: { selector: "", value: "html" },
    }).flatMap((group) => rows(group.html, "li[data-link-id]:not(.download-icon)", {
      id: { selector: "", value: "data-link-id" },
      name: { selector: "", value: "text" },
    }).map((server) => ({ ...server, type: group.label || group.type }))).slice(0, 8);
    const streams = [];
    for (const server of serverRows) {
      if (!server.id || !server.name || server.id === "undefined") continue;
      try {
        const target = siteGet(`/ajax/server?get=${encodeURIComponent(server.id)}`, headers);
        const serverJson = JSON.parse(target.body);
        const resolved = resolveStream(String(serverJson.result?.url || ""), base, referer);
        if (!resolved) continue;
        streams.push({
          id: `${server.id}`,
          title: `${server.name.trim()}${server.type ? ` · ${server.type.trim()}` : ""}`,
          description: `Episode ${episode.number}`,
          ...resolved,
        });
      } catch (error) {
        nova.log(`${server.name}: ${error}`);
      }
    }

    if (episode.mal && episode.slug && episode.timestamp) {
      try {
        const mapped = nova.http.get(
          `https://mapper.nekostream.site/api/mal/${encodeURIComponent(episode.mal)}/${encodeURIComponent(episode.slug)}/${encodeURIComponent(episode.timestamp)}`,
          JSON.stringify({ Accept: "application/json", Referer: `${base}/`, Origin: base }),
        );
        const mapResult = JSON.parse(mapped);
        if (!mapResult.error && mapResult.status >= 200 && mapResult.status < 300) {
          const mapper = JSON.parse(mapResult.body);
          for (const [name, server] of Object.entries(mapper)) {
            if (name.toLowerCase() === "status" || !server) continue;
            for (const [variant, data] of [["H-Sub", server.sub], ["A-Dub", server.dub]]) {
              try {
                const resolved = resolveStream(String(data?.url || ""), base, `${base}/`);
                if (!resolved) continue;
                streams.push({
                  id: `mapper-${name}-${variant}`,
                  title: `${name} · ${variant}`,
                  description: `Episode ${episode.number}`,
                  ...resolved,
                });
              } catch (error) {
                nova.log(`${name} ${variant}: ${error}`);
              }
            }
          }
        }
      } catch (error) {
        nova.log(`mapper request failed: ${error}`);
      }
    }
    return streams.slice(0, 40);
  }

  function isDirectUrl(url) {
    return /^https:\/\//i.test(url) && /\.(?:m3u8|mp4)(?:$|[?#])/i.test(url)
      && !/\/stream\//i.test(url);
  }

  function normalizedTitle(value) {
    return String(value || "").normalize("NFKD").toLowerCase()
      .replace(/\p{M}+/gu, "").replace(/[^\p{L}\p{N}]+/gu, " ").trim();
  }

  function episodeTitle(value) {
    const title = normalizedTitle(value).replace(/^(?:episode|ep|stage|turn)\s*\d+(?:\s*\d+)?\s*/, "");
    return !title || /^\d+$/.test(title) || /^(?:episode|ep|unknown|untitled|tba|tbd)$/.test(title) ? "" : title;
  }

  function titleRelation(expected, titles) {
    if (titles.includes(expected)) return 2;
    if (expected.length < 4) return 0;
    return titles.some((title) => title.startsWith(`${expected} `) || expected.startsWith(`${title} `)) ? 1 : 0;
  }

  function namedSeason(value) {
    const title = normalizedTitle(value);
    const numeric = title.match(/\b(?:season|series)\s+(\d+)\b/)
      || title.match(/\b(\d+)(?:st|nd|rd|th)\s+season\b/)
      || title.match(/\br(\d+)$/);
    if (numeric) return Number(numeric[1]);
    const words = title.match(/\b(first|second|third|fourth|fifth|sixth)\s+season\b/);
    return words ? ["first", "second", "third", "fourth", "fifth", "sixth"].indexOf(words[1]) + 1 : null;
  }

  function startYear(value) {
    return Number(String(value || "").match(/\b(?:19|20)\d{2}\b/)?.[0]) || null;
  }

  function seriesIdentity(value) {
    let base = normalizedTitle(value);
    const part = base.match(/\s+(?:part|cour)\s+(\d+)$/)
      || base.match(/\s+(\d+)(?:st|nd|rd|th)\s+(?:part|cour)$/);
    if (part) base = base.slice(0, part.index).trim();
    const season = namedSeason(base);
    const roman = base.match(/\s+(ii|iii|iv|v|vi)$/);
    base = base.replace(/\s+(?:(?:season|series)\s+\d+|\d+(?:st|nd|rd|th)\s+season|(?:first|second|third|fourth|fifth|sixth)\s+season|r\d+)$/, "");
    if (roman) base = base.slice(0, roman.index).trim();
    return { base, season: season || (roman ? ["i", "ii", "iii", "iv", "v", "vi"].indexOf(roman[1]) + 1 : 1),
      part: part ? Number(part[1]) : 1, labeled: !!season || !!roman || !!part };
  }

  function sourceEpisodeCount(series) {
    const numbers = series.episodeRows.map((row) => Number(row.number))
      .filter((number) => Number.isInteger(number) && number > 0);
    if (!numbers.length) return null;
    const unique = new Set(numbers);
    const maximum = Math.max(...numbers);
    if (unique.size !== numbers.length || unique.size !== maximum || maximum > 10000) return null;
    const declared = series.page.episodeCount;
    // A declared count also describes an ongoing cour. Available rows must
    // still form a prefix, and the target episode must actually have servers.
    if (declared && (declared < maximum || declared > 10000)) return null;
    return declared || maximum;
  }

  function episodeSequence(lookup, candidates, expectedIdentity, sourceYear) {
    if (expectedIdentity.labeled) return null;
    const ordered = candidates.filter((candidate) => candidate.baseRelation === 2)
      .sort((a, b) => a.identity.season - b.identity.season || a.identity.part - b.identity.part);
    if (!ordered.length || ordered[0].identity.season !== 1 || ordered[0].identity.part !== 1) return null;
    if (sourceYear && ordered[0].year && ordered[0].year !== sourceYear) return null;
    let total = 0;
    const boundaries = new Set([0]);
    for (let index = 0; index < ordered.length; index++) {
      const entry = ordered[index];
      if (!entry.count) return null;
      if (index) {
        const previous = ordered[index - 1];
        const sameSeason = entry.identity.season === previous.identity.season
          && entry.identity.part === previous.identity.part + 1;
        const nextSeason = entry.identity.season === previous.identity.season + 1 && entry.identity.part === 1;
        // Gaps, duplicate editions, and reversed dates leave the ordering
        // unknown. Never shift episodes across an unverified missing part.
        if ((!sameSeason && !nextSeason) || (entry.year && previous.year && entry.year < previous.year)) return null;
      }
      entry.offset = total;
      total += entry.count;
      boundaries.add(total);
    }
    const absolute = lookup.absoluteEpisode;
    const count = lookup.seasonEpisodeCount;
    if (!Number.isInteger(absolute) || !Number.isInteger(count) || count < lookup.episode || absolute > total) return null;
    const start = absolute - lookup.episode;
    const end = start + count;
    // Matching block boundaries proves a merged metadata season covers whole
    // native parts. Equal series totals also prove the inverse (split metadata
    // seasons inside one native entry), without assuming twelve-episode cours.
    if (!((boundaries.has(start) && boundaries.has(end)) || lookup.seriesEpisodeCount === total)) return null;
    if (start < 0 || end > total) return null;
    const entry = ordered.find((candidate) => absolute > candidate.offset && absolute <= candidate.offset + candidate.count);
    return entry ? { candidate: entry, number: absolute - entry.offset } : null;
  }

  function lookupStreams(request) {
    const lookup = request.lookup || {};
    const cacheKey = `lookup:v2:${nova.crypto.hmacSha256Base64Url(JSON.stringify(lookup), "anikoto-source-lookup")}`;
    let cached;
    try { cached = JSON.parse(nova.storage.get(cacheKey) || "null"); } catch (_) {}
    if (cached && cached.expires > Date.now() && String(cached.path || "").startsWith("/watch/")) {
      try {
        const result = streams({ episodeId: episodeToken(cached.path, cached.number) });
        if (result.length) return result;
      } catch (_) {}
    }
    const match = resolveEpisode(lookup, loadLookupCandidates(lookup));
    if (!match) return [];
    nova.storage.set(cacheKey, JSON.stringify({ path: match.path, number: match.number, expires: Date.now() + 3600000 }));
    // Only the session cache contains the source mapping. Playback keeps
    // the caller's original Stremio IDs for library history and sync.
    return streams({ episodeId: episodeToken(match.path, match.number) }, match.series);
  }

  function loadLookupCandidates(lookup, seed = null) {
    const title = String(lookup.title || "").trim();
    const expected = normalizedTitle(title);
    const expectedIdentity = seriesIdentity(title);
    const candidates = [];
    const seen = new Set();
    function addCandidate(path, names) {
      if (!path || seen.has(path)) return;
      const titles = names.map(normalizedTitle).filter(Boolean);
      const identities = titles.map(seriesIdentity);
      const relation = titleRelation(expected, titles);
      const baseRelation = titleRelation(expectedIdentity.base, identities.map((identity) => identity.base));
      if (!relation && baseRelation !== 2) return;
      seen.add(path);
      const identity = identities.find((identity) => identity.base === expectedIdentity.base) || identities[0];
      // Reverse mapping starts with a known source entry. Never let its
      // season disappear behind the candidate cap or a page of side stories.
      candidates.push({ path, relation, baseRelation, identity,
        rank: (seed?.path === path ? 1000 : 0) + baseRelation * 200 + relation * 100
          + (identity.season === lookup.season ? 150 : 0) });
    }
    if (seed) addCandidate(seed.path, [seed.page.englishTitle, seed.page.title,
      ...String(seed.page.aliases || "").split(";")]);
    // Search independently of Discover's pagination state. A long-running
    // anime can have enough spinoffs to put its original title on page two.
    for (let page = 1; page <= 2; page++) {
      const result = siteGet(`/filter?keyword=${encodeURIComponent(title)}&page=${page}&vrf=${vrfEncrypt(title)}`);
      for (const row of rows(result.body, "div.ani.items > div.item", ROOT_FIELDS).slice(0, 80)) {
        const path = sourcePath(row.href);
        if (/^(movie|music|special)$/i.test(String(row.format || "").trim())) continue;
        addCandidate(path, [row.englishTitle, row.title, row.japaneseTitle]);
      }
      if (!rows(result.body, "nav > ul.pagination > li.active ~ li", {}).length) break;
    }
    candidates.sort((a, b) => b.rank - a.rank);
    const loaded = [];
    for (const candidate of candidates.slice(0, 6)) {
      try {
        const series = seed?.path === candidate.path ? seed : loadSeries(nova.crypto.base64UrlEncode(candidate.path));
        const year = startYear(series.page.year);
        if (expectedIdentity.labeled && (candidate.identity.season !== expectedIdentity.season
          || candidate.identity.part !== expectedIdentity.part)) continue;
        const episodeIndex = new Map();
        const titleIndex = new Map();
        for (const episode of series.episodeRows) {
          const number = Number(episode.number);
          if (!Number.isInteger(number) || number < 1 || !episode.serverIds) continue;
          episodeIndex.set(number, episode);
          const title = episodeTitle(episode.title);
          if (title) titleIndex.set(title, [...(titleIndex.get(title) || []), number]);
        }
        loaded.push({ ...candidate, series, year, count: sourceEpisodeCount(series), episodeIndex, titleIndex });
      } catch (error) {
        nova.log(`source lookup candidate failed: ${error}`);
      }
    }
    return loaded;
  }

  function resolveEpisode(lookup, loaded) {
    if (lookup.mediaType !== "series" || !Number.isInteger(lookup.season) || lookup.season < 1
      || !Number.isInteger(lookup.episode) || lookup.episode < 1
      || (lookup.absoluteEpisode != null && lookup.absoluteEpisode < lookup.episode)
      || (lookup.seasonEpisodeCount != null && lookup.seasonEpisodeCount < lookup.episode)) return null;
    const expectedIdentity = seriesIdentity(lookup.title);
    if (!expectedIdentity.base) return null;
    const sourceYear = startYear(lookup.year);
    const releaseYear = startYear(lookup.released);
    const expectedEpisode = episodeTitle(lookup.episodeTitle);
    const matchesByTarget = new Map();
    function eligible(candidate) {
      return !(sourceYear && candidate.year && candidate.year < sourceYear)
        && !(releaseYear && candidate.year && candidate.year > releaseYear);
    }
    function addMatch(candidate, number, confidence, alignedSequence = false) {
      if (!eligible(candidate)) return;
      const episode = candidate.episodeIndex.get(number);
      if (!episode) return;
      const actual = episodeTitle(episode.title);
      // Episode titles can be translated or edited differently. Exact family
      // identity and verified partition counts can prove the sequence without
      // requiring identical wording; number-only fallbacks cannot.
      if (expectedEpisode && actual && expectedEpisode !== actual && !alignedSequence) return;
      const key = `${candidate.path}:${number}`;
      const previous = matchesByTarget.get(key);
      if (!previous || previous.confidence < confidence) {
        matchesByTarget.set(key, { path: candidate.path, number, series: candidate.series, confidence });
      }
    }
    // Meaningful episode names can locate the same episode even when neither
    // season nor episode numbering agrees. Repeated names are not evidence.
    const titleMatches = [];
    if (expectedEpisode) {
      for (const candidate of loaded) {
        if (!eligible(candidate)) continue;
        for (const number of candidate.titleIndex.get(expectedEpisode) || []) titleMatches.push({ candidate, number });
      }
    }
    if (titleMatches.length === 1) addMatch(titleMatches[0].candidate, titleMatches[0].number, 1000);
    const aligned = episodeSequence(lookup, loaded, expectedIdentity, sourceYear);
    if (aligned) addMatch(aligned.candidate, aligned.number, 800, true);
    for (const candidate of loaded) {
      // Counts that disagree invalidate the simple season-number assumption.
      // The sequence/title strategies above must prove that different grouping.
      if (lookup.seasonEpisodeCount && candidate.count !== lookup.seasonEpisodeCount) continue;
      const sameStart = !sourceYear || !candidate.year || candidate.year === sourceYear;
      let number = lookup.episode;
      let continuous = false;
      if (lookup.season === 1) {
        if (!sameStart || (!sourceYear && releaseYear && candidate.year && candidate.year !== releaseYear)) continue;
      } else if (candidate.identity.season !== lookup.season) {
        if (candidate.relation !== 2 || !Number.isInteger(lookup.absoluteEpisode)
          || lookup.absoluteEpisode <= lookup.episode || !sameStart) continue;
        number = lookup.absoluteEpisode;
        continuous = true;
      }
      const episode = candidate.episodeIndex.get(number);
      const sameEpisode = expectedEpisode && episodeTitle(episode?.title) === expectedEpisode;
      const confirmedSeason = !continuous && candidate.confirmedSeason === lookup.season
        && candidate.count === lookup.seasonEpisodeCount;
      if (candidate.relation < 2 && candidate.baseRelation < 2
        && !(sameEpisode && titleMatches.length === 1) && !confirmedSeason) continue;
      addMatch(candidate, number, confirmedSeason ? 750 : (continuous ? 400 : 500) + (sameEpisode ? 150 : 0)
        + (candidate.relation === 2 ? 50 : 0) + (sourceYear && candidate.year === sourceYear ? 50 : 0), confirmedSeason);
    }
    const matches = [...matchesByTarget.values()];
    matches.sort((a, b) => b.confidence - a.confidence);
    if (!matches.length || (matches[1] && matches[0].confidence === matches[1].confidence)) {
      nova.log(matches.length ? "source lookup is ambiguous" : "source lookup found no matching episode");
      return null;
    }
    return matches[0];
  }

  function canonicalCandidate(request) {
    const source = request.source;
    const identity = seriesIdentity(source.title);
    const sourceYear = startYear(source.year);
    const matches = (request.candidates || []).map((candidate) => {
      if (candidate.media_type !== "series" || !/^tt\d+$/.test(candidate.external_ids?.imdb || "")) return null;
      const year = startYear(candidate.year);
      if (sourceYear && (!year || (identity.labeled ? year > sourceYear : year !== sourceYear))) return null;
      if (source.external_ids?.imdb) return source.external_ids.imdb === candidate.external_ids.imdb ? { candidate, relation: 3 } : null;
      const titles = [source.title, ...(source.aliases || [])].map((title) => seriesIdentity(title).base);
      const relation = Math.max(...[candidate.title, ...(candidate.aliases || [])].map((title) => titleRelation(seriesIdentity(title).base, titles)));
      return relation ? { candidate, relation } : null;
    }).filter(Boolean).sort((a, b) => b.relation - a.relation);
    return matches.length && (!matches[1] || matches[1].relation < matches[0].relation)
      ? matches[0].candidate.external_ids.imdb : null;
  }

  function mapEpisodes(request) {
    const canonical = request.canonical;
    const source = request.source;
    const identity = seriesIdentity(source.page.englishTitle || source.page.title);
    const loaded = loadLookupCandidates({ title: canonical.title, season: identity.season }, source);
    const seasons = new Map();
    for (const video of canonical.videos.slice(0, 10000)) {
      if (!Number.isInteger(video.season) || video.season < 1 || video.season > 100
        || !Number.isInteger(video.episode) || video.episode < 1 || video.episode > 10000) continue;
      if (!seasons.has(video.season)) seasons.set(video.season, new Set());
      seasons.get(video.season).add(video.episode);
    }
    const counts = new Map();
    for (const [season, numbers] of seasons) {
      const maximum = Math.max(...numbers);
      if (numbers.size === maximum) counts.set(season, maximum);
    }
    const offsets = new Map();
    let total = 0;
    const lastSeason = Math.max(0, ...seasons.keys());
    for (let season = 1; season <= lastSeason; season++) {
      if (!counts.has(season)) { total = null; break; }
      offsets.set(season, total);
      total += counts.get(season);
    }
    // A shortened canonical series title (e.g. "Code Geass") cannot by
    // itself prove that a longer native title belongs to the same series.
    // Two distinct, globally unique episode names at the same positions,
    // together with equal season counts, confirm the remaining positions
    // even when one provider has edited an individual episode title.
    const canonicalTitleCounts = new Map();
    for (const video of canonical.videos.slice(0, 10000)) {
      if (!seasons.get(video.season)?.has(video.episode)) continue;
      const title = episodeTitle(video.title);
      if (title) canonicalTitleCounts.set(title, (canonicalTitleCounts.get(title) || 0) + 1);
    }
    const sourceTitleCounts = new Map();
    for (const candidate of loaded) {
      for (const [title, numbers] of candidate.titleIndex) {
        sourceTitleCounts.set(title, (sourceTitleCounts.get(title) || 0) + numbers.length);
      }
    }
    for (const candidate of loaded) {
      const season = candidate.identity.season;
      if (candidate.count !== counts.get(season)) continue;
      const anchors = new Set();
      let conflictingAnchor = false;
      for (const video of canonical.videos.slice(0, 10000)) {
        if (video.season !== season || !video.id.startsWith(`${canonical.mediaId}:`)) continue;
        const title = episodeTitle(video.title);
        if (!title || canonicalTitleCounts.get(title) !== 1 || sourceTitleCounts.get(title) !== 1) continue;
        const nativeNumber = candidate.titleIndex.get(title)?.[0];
        if (nativeNumber === video.episode) anchors.add(title);
        else if (nativeNumber != null) conflictingAnchor = true;
      }
      if (anchors.size >= 2 && !conflictingAnchor) candidate.confirmedSeason = season;
    }
    const mapped = new Map();
    for (const video of canonical.videos.slice(0, 10000)) {
      if (!video.id.startsWith(`${canonical.mediaId}:`) || !seasons.get(video.season)?.has(video.episode)) continue;
      const match = resolveEpisode({
        mediaType: "series", title: canonical.title, year: canonical.year,
        season: video.season, episode: video.episode,
        absoluteEpisode: offsets.has(video.season) ? offsets.get(video.season) + video.episode : null,
        seasonEpisodeCount: counts.get(video.season), seriesEpisodeCount: total,
        episodeTitle: video.title, released: video.released,
      }, loaded);
      if (!match || match.path !== source.path) continue;
      const previous = mapped.get(match.number);
      // Two canonical episodes pointing at one source episode are ambiguous.
      mapped.set(match.number, mapped.has(match.number) && previous !== video.id ? false : video.id);
    }
    return [...mapped].filter(([, id]) => id).map(([number, id]) => ({ number, ids: [id] }));
  }

  function episodeToken(path, number) {
    return `ep:${nova.crypto.base64UrlEncode(JSON.stringify({ mediaPath: path, number }))}`;
  }

  function resolveStream(url, base, referer) {
    if (isDirectUrl(url)) {
      return { url, headers: { Referer: referer, Origin: base }, subtitles: [] };
    }
    if (/^https:\/\/megaplay\.buzz\/(?:videojs\/)?stream\//i.test(url)) {
      // This variant puts image headers before MPEG-TS packets. Standard
      // mpv cannot demux it; the other MegaPlay servers supply normal HLS.
      if (/[?&]s=tcdn(?:[&#]|$)/i.test(url)) return null;
      return resolveMegaPlay(url, base);
    }
    if (/^https:\/\/mewcdn\.online\/player\/plyr\.php[?#]/i.test(url)) {
      return resolveMewcdn(url, base);
    }
    return null;
  }

  function resolveMegaPlay(embedUrl, base) {
    const page = httpGet(embedUrl, {
      Referer: `${base}/`, "X-Requested-With": "XMLHttpRequest",
      Accept: "text/html,application/xhtml+xml,*/*;q=0.8",
    });
    const mediaId = page.body.match(/data-id=["']([^"']+)["']/i)?.[1]
      || page.body.match(/File\s+(\d+)/i)?.[1];
    if (!mediaId) throw new Error("MegaPlay page had no media id");
    const embedBase = page.url.match(/^https:\/\/[^/]+/i)?.[0];
    if (!embedBase) throw new Error("invalid MegaPlay page URL");
    const server = embedUrl.match(/[?&]s=([^&#]+)/)?.[1];
    const sourcesUrl = `${embedBase}/stream/getSources?id=${encodeURIComponent(mediaId)}`
      + (server ? `&s=${server}` : "");
    const response = httpGet(sourcesUrl, {
      Referer: embedUrl, "X-Requested-With": "XMLHttpRequest", Accept: "application/json,*/*",
    });
    const sources = JSON.parse(response.body);
    let mediaUrl = "";
    let decrypted = false;
    if (typeof sources.enc === "string" && sources.enc) {
      // These protocol constants match the upstream hoster's AES-256 CBC
      // convention: its 16-byte key is zero-filled to 32 bytes.
      const raw = nova.crypto.aes256CbcDecrypt(sources.enc,
        "aT9MTVRBeDBRNiw6fTUwVQAAAAAAAAAAAAAAAAAAAAA=", "VzA7MjdUb2FVcGxfUCUnYw==");
      if (raw) {
        try { mediaUrl = String(JSON.parse(raw).file || ""); } catch (_) {}
        decrypted = !!mediaUrl;
      }
    }
    if (!mediaUrl) {
      mediaUrl = typeof sources.sources === "string" ? sources.sources
        : String(sources.sources?.[0]?.file || sources.sources?.file || "");
    }
    if (!isDirectUrl(mediaUrl)) throw new Error("MegaPlay returned no playable source");
    if (decrypted && !/[?&]token=/i.test(mediaUrl)) {
      const path = mediaUrl.match(/\/([a-f0-9]{32})\/([a-f0-9]{32})\//i);
      if (path) {
        // Sign only at resolution time. The short-lived token must never
        // become the media/episode identifier or a persisted metadata URL.
        const payload = `${Math.floor(Date.now() / 1000) + 90}|${path[1].toLowerCase()}/${path[2].toLowerCase()}`;
        const token = `${nova.crypto.base64UrlEncode(payload)}.${nova.crypto.hmacSha256Base64Url(payload, "MpCdnT0k3n!9f2K#xQ7vL5mR8wN1pY4s")}`;
        mediaUrl += `${mediaUrl.includes("?") ? "&" : "?"}token=${encodeURIComponent(token)}`;
      }
    }
    const subtitles = (Array.isArray(sources.tracks) ? sources.tracks : []).slice(0, 20)
      .filter((track) => (!track.kind || track.kind === "captions") && /^https:\/\//i.test(track.file || ""))
      .map((track) => ({ url: track.file, language: subtitleLanguage(track.label) }))
      .sort((a, b) => Number(b.language === "eng") - Number(a.language === "eng"));
    return {
      url: mediaUrl,
      headers: { Referer: `${embedBase}/`, Origin: embedBase },
      subtitles,
    };
  }

  function subtitleLanguage(label) {
    const language = String(label || "").split(" (")[0].trim();
    const codes = { english: "eng", chinese: "chi", japanese: "jpn", indonesian: "ind",
      thai: "tha", vietnamese: "vie", spanish: "spa", portuguese: "por", french: "fra",
      german: "deu", italian: "ita", arabic: "ara", russian: "rus", korean: "kor" };
    return codes[language.toLowerCase()] || language || null;
  }

  function resolveMewcdn(embedUrl, base) {
    const fragment = embedUrl.split("#")[1] || "";
    let mediaUrl = nova.crypto.base64UrlDecode(fragment).trim();
    if (!isDirectUrl(mediaUrl)) throw new Error("invalid Mewcdn playlist fragment");
    const page = httpGet(embedUrl, { Referer: `${base}/` });
    const hostMap = page.body.match(/var\s+HOST_MAP\s*=\s*\{([^}]+)\}/)?.[1] || "";
    for (const entry of hostMap.matchAll(/['"]([^'"]+)['"]\s*:\s*['"]([^'"]+)['"]/g)) {
      if (mediaUrl.includes(entry[1])) {
        mediaUrl = mediaUrl.replace(entry[1], entry[2]);
        break;
      }
    }
    if (!isDirectUrl(mediaUrl)) throw new Error("Mewcdn returned no playable source");
    return {
      url: mediaUrl, headers: { Referer: "https://mewcdn.online/", Origin: "https://mewcdn.online" },
      subtitles: [],
    };
  }

  globalThis.novaProvider = {
    handle(request) {
      switch (request.op) {
        case "catalog": return catalog(request);
        case "details": return details(request);
        case "streams": return streams(request);
        case "lookupStreams": return lookupStreams(request);
        case "canonicalCandidate": return canonicalCandidate(request);
        case "mapEpisodes": return mapEpisodes(request);
        default: throw new Error(`unsupported provider operation: ${request.op}`);
      }
    },
  };
})();
