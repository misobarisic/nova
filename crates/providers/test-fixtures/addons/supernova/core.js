(() => {
  "use strict";
  const S = globalThis.Supernova = { sources: {}, VERSION: 2, MAX: 50, CANDIDATE_MAX: 300, RESULT_TTL: 900 };
  // Invocation-only policy. Hosts without this capability retain the plugin's
  // existing configuration, so the same package remains usable on older Nova.
  S.withStreamPreferences = (settings, preferences) => !preferences || !nova.streamPreferences ? settings : {
    ...settings, streamPreferences:preferences,
    "quality.preferred":"any", "language.prefs":[], "codec.prefs":[],
  };
  S.preferenceFacts = new WeakMap();
  S.copyRow = row => {
    const copy={...row};
    if (S.preferenceFacts.has(row)) S.preferenceFacts.set(copy,S.preferenceFacts.get(row));
    return copy;
  };
  S.acceptPreferences = (facts,settings,complete) => !settings.streamPreferences || nova.streamPreferences.accepts(JSON.stringify(facts),complete);
  S.preferenceComparator = (settings,facts) => {
    if (!settings.streamPreferences) return ()=>0;
    const serialized=new Map();
    const snapshot=row=>{
      if (!serialized.has(row)) serialized.set(row,JSON.stringify(facts(row)));
      return serialized.get(row);
    };
    return (a,b)=>nova.streamPreferences.compare(snapshot(a),snapshot(b));
  };
  S.sortByPreferences = (rows,settings,facts,identity) => {
    if (typeof nova.streamPreferences.sort !== "function") {
      const compare=S.preferenceComparator(settings,facts);
      return rows.sort((a,b)=>compare(a,b) || (identity(a)<identity(b) ? -1 : identity(a)>identity(b) ? 1 : 0));
    }
    // Parse each facts object once in Rust, then use the same authoritative
    // comparator without QuickJS/JSON crossings for every comparison.
    const indices=JSON.parse(nova.streamPreferences.sort(JSON.stringify(rows.map(row=>({id:identity(row),facts:facts(row)})))));
    if (!Array.isArray(indices) || indices.length!==rows.length || new Set(indices).size!==rows.length || indices.some(index=>!Number.isInteger(index) || index<0 || index>=rows.length)) throw new Error("Invalid shared stream sort response");
    return indices.map(index=>rows[index]);
  };
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
  S.resultCacheGet = key => {
    try { return S.json(nova.cache.get(key)); } catch (_) { return null; }
  };
  S.resultCacheSet = (key, value, ttl) => {
    try { nova.cache.set(key, JSON.stringify(value), ttl); } catch (_) { /* Persisting normalized results is optional. */ }
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
  S.batch = (requests, deadline, onComplete) => {
    const result = [];
    for (let i = 0; i < requests.length; i += 3) {
      const remaining = deadline === undefined ? 3150 : deadline - Date.now();
      if (remaining <= 350) {
        const skipped = requests.slice(i).map(() => ({error:"Optional HTTP stage budget exhausted"}));
        if (onComplete) skipped.forEach((response,index)=>onComplete(i+index,response));
        result.push(...skipped);
        break;
      }
      const timeoutMs = Math.max(200,Math.min(3000,remaining - 150));
      const wave = requests.slice(i,i + 3).map(request => ({...request,timeoutMs:Math.min(request.timeoutMs || 3000,timeoutMs)}));
      const rows = S.json(onComplete ? nova.http.batchEach(JSON.stringify(wave),(index,raw)=>onComplete(i+index,S.json(raw))) : nova.http.batch(JSON.stringify(wave)));
      result.push(...(Array.isArray(rows) ? rows : wave.map(() => ({error:"Source unavailable"}))));
    }
    return result;
  };

})();
