(() => {
  "use strict";
  const S = globalThis.Supernova = { sources: {}, VERSION: 2, MAX: 50, CANDIDATE_MAX: 150 };
  S.json = raw => { try { return JSON.parse(raw); } catch (_) { return null; } };
  S.hash = value => /^[a-f\d]{40}$/i.test(String(value || "")) ? String(value).toLowerCase() : null;
  S.magnetHash = value => {
    const match = String(value || "").match(/(?:[?&])xt=urn:btih:([a-f\d]{40})(?:&|$)/i);
    return match ? S.hash(match[1]) : null;
  };
  S.number = value => {
    if (typeof value === "number") return Number.isSafeInteger(value) && value >= 0 ? value : null;
    if (typeof value !== "string" || !/^\d+$/.test(value.trim())) return null;
    const number = Number(value.trim());
    return Number.isSafeInteger(number) && number >= 0 ? number : null;
  };
  S.size = raw => {
    const match = String(raw || "").match(/^\s*([\d.,]+)\s*(B|[KMGT]i?B)\s*$/i);
    if (!match) return null;
    const power = {b:0,kb:1,kib:1,mb:2,mib:2,gb:3,gib:3,tb:4,tib:4}[match[2].toLowerCase()];
    const value = Math.round(Number(match[1].replace(/,/g, "")) * (match[2].toLowerCase().includes("i") ? 1024 : 1000) ** power);
    return Number.isSafeInteger(value) && value > 0 ? value : null;
  };
  S.cacheGet = key => {
    try { return S.json(nova.cache.get(key)); } catch (_) { return null; }
  };
  S.cacheSet = (key, value, ttl) => {
    try { nova.cache.set(key, JSON.stringify(value), ttl); } catch (_) { /* Cache failure cannot break discovery. */ }
  };
  S.http = request => {
    const response = S.json(nova.http.request(JSON.stringify(request)));
    if (!response || response.error || response.status < 200 || response.status >= 300) return null;
    return response;
  };
  S.metainfoUrl = (value, hash) => {
    const raw = typeof value === "string" ? value : "";
    const authority = raw.match(/^https:\/\/([^/?#]+)(?:[/?]|$)/i);
    if (raw.length <= 8192 && !/[\s@#]/.test(raw) && authority &&
        (!authority[1].includes(":") || /:443$/.test(authority[1]) || /^\[[a-f\d:.]+\]$/i.test(authority[1]))) return raw;
    return S.hash(hash) ? `https://itorrents.net/torrent/${hash.toUpperCase()}.torrent` : null;
  };
  S.batch = (requests, deadline) => {
    const result = [];
    for (let i = 0; i < requests.length; i += 3) {
      const remaining = deadline === undefined ? 3150 : deadline - Date.now();
      if (remaining <= 350) {
        result.push(...requests.slice(i).map(() => ({error:"Optional HTTP stage budget exhausted"})));
        break;
      }
      const timeoutMs = Math.max(200,Math.min(3000,remaining - 150));
      const wave = requests.slice(i,i + 3).map(request => ({...request,timeoutMs:Math.min(request.timeoutMs || 3000,timeoutMs)}));
      const rows = S.json(nova.http.batch(JSON.stringify(wave)));
      result.push(...(Array.isArray(rows) ? rows : wave.map(() => ({error:"Source unavailable"}))));
    }
    return result;
  };
  S.row = (record, torbox, token) => {
    const quality = String(record.title).match(/\b(2160|1080|720|480)p\b/i);
    return {
      id: `${record.infoHash}:${record.fileIdx === null || record.fileIdx === undefined ? "unknown" : record.fileIdx}:${torbox ? "torbox" : "p2p"}`,
      title: `Supernova · ${torbox ? "TorBox cached" : "P2P"}${quality ? " · " + quality[0] : ""}`,
      description: `${record.title}\n${record.sources.join(" + ")}${record.seeders === null ? " · peers unverified" : " · " + record.seeders + " reported seeders"}`,
      source: record.sources.join(" + "), seeders: record.seeders, size: record.sizeBytes,
      ...(torbox ? {resolver:{provider_id:"supernova",token}} : {info_hash:record.infoHash,file_idx:record.fileIdx}),
    };
  };
})();
