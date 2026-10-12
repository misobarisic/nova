(() => {
  "use strict";
  const S = globalThis.Supernova;
  const BASE = "https://api.torbox.app/v1/api/torrents/";
  const failure = (code, message) => { const error = new Error(message); error.code = code; throw error; };
  const keyFor = settings => {
    const key = String(settings["torbox.apiKey"] || "").trim();
    if (!settings["torbox.enabled"] || !key) failure("missing_key", "Configure a TorBox API key first.");
    if (key.length > 512 || /[\s\x00-\x1f\x7f]/.test(key)) failure("invalid_key", "The TorBox API key is invalid.");
    return key;
  };
  const apiData = raw => {
    if (!raw || raw.error) failure("unavailable", "TorBox is unavailable. Try again later.");
    if (raw.status === 401 || raw.status === 403) failure("invalid_key", "TorBox rejected the API key or account permissions.");
    if (raw.status === 429) failure("rate_limit", "TorBox is rate limited. Try again later.");
    if (raw.status < 200 || raw.status >= 300) failure("unavailable", "TorBox is unavailable. Try again later.");
    const response = S.json(raw.body);
    if (!response || response.success !== true) {
      const code = String(response && response.error || "");
      if (/auth|token|key/i.test(code)) failure("invalid_key", "TorBox rejected the API key or account permissions.");
      if (/already|duplicate/i.test(code)) failure("already_exists", "This torrent already exists in the TorBox account.");
      if (/cache|not_found/i.test(code)) failure("not_cached", "This torrent is no longer cached on TorBox.");
      if (/limit|slot|plan/i.test(code)) failure("account_limit", "TorBox account limits prevent playback.");
      failure("unavailable", "TorBox could not resolve this stream.");
    }
    return response.data;
  };
  const api = (route, key, body, timeoutMs) => apiData(S.json(nova.http.request(JSON.stringify({
    url:BASE + route,method:body ? "POST" : "GET",
    headers:{Authorization:"Bearer " + key},...(body ? {body} : {}),...(timeoutMs ? {timeoutMs} : {}),
  }))));
  const cached = (records, key, timeoutMs, warnings = null) => {
    const hashes = [...new Set(records.map(record => S.hash(record.infoHash)).filter(Boolean))].slice(0,150);
    if (!hashes.length) return {};
    const groups = [];
    for (let offset = 0; offset < hashes.length; offset += 50) groups.push(hashes.slice(offset,offset + 50));
    const route = group => `checkcached?hash=${group.join(",")}&format=object&list_files=true`;
    // Lookup checks the bounded discovery pool in one concurrent GET wave.
    // Resolver revalidation still performs a normal single-hash API request.
    const requests = groups.map(group => ({
      url:BASE + route(group),method:"GET",headers:{Authorization:"Bearer " + key},timeoutMs:timeoutMs || 3000,
    }));
    const responses = warnings ? S.json(nova.http.batch(JSON.stringify(requests))) : null;
    const safe = {};
    groups.forEach((group,index) => {
      let data;
      try { data = warnings ? apiData(Array.isArray(responses) ? responses[index] : null) : api(route(group),key,null,timeoutMs); }
      catch (error) {
        if (!warnings) throw error;
        const message = error.code ? error.message : "TorBox is unavailable. Try again later.";
        if (!warnings.some(warning => warning.source === "TorBox" && warning.message === message)) warnings.push({source:"TorBox",message});
        return;
      }
      if (!data || typeof data !== "object" || Array.isArray(data)) return;
      for (const hash of group) {
        const entry = data[hash] || data[hash.toUpperCase()];
        if (entry && S.hash(entry.hash) === hash && Array.isArray(entry.files)) safe[hash] = entry;
      }
    });
    return safe;
  };
  S.torboxOptions = (records, lookup, settings, warnings, timeoutMs = 3000) => {
    if (settings["playback.mode"] === "direct") return [];
    if (!settings["torbox.enabled"] || !settings["torbox.apiKey"]) {
      if (warnings) warnings.push({source:"TorBox",message:"Enable TorBox and configure its API key to use cached playback."});
      return [];
    }
    try {
      // Cache no credentials, links or account objects; live availability is
      // rechecked per lookup and again on selection to contain stale state.
      const available = cached(records, keyFor(settings), timeoutMs, warnings || []);
      return records.flatMap(record => {
        const entry = available[record.infoHash];
        const file = entry && S.chooseFile(entry.files, record, lookup, true, entry.name);
        if (!file || file.infected === true || file.zipped === true) return [];
        const token = JSON.stringify({version:S.VERSION,infoHash:record.infoHash,title:record.title,
          fileName:String(file.name || file.path),lookup,sources:record.sources});
        return [S.row({...record,sizeBytes:S.number(file.size) || record.sizeBytes},true,token)];
      });
    } catch (error) {
      nova.log(`Supernova TorBox: ${error.code || "unavailable"}`);
      if (warnings) warnings.push({source:"TorBox",message:error.message || "TorBox is unavailable. Try again later."});
      return [];
    }
  };
  S.resolveTorbox = (rawToken, settings) => {
    const token = S.json(rawToken);
    if (!token || token.version !== S.VERSION || !S.hash(token.infoHash) || typeof token.fileName !== "string" || token.fileName.length > 2048 || !token.lookup) {
      failure("invalid_token", "Invalid Supernova stream selection.");
    }
    const lookup = token.lookup;
    if (!["movie","series"].includes(lookup.mediaType) || typeof lookup.title !== "string" || lookup.title.length > 1024 || !lookup.mediaId ||
        (lookup.mediaType === "series" && !(Number.isInteger(lookup.season) && lookup.season > 0 && Number.isInteger(lookup.episode) && lookup.episode > 0))) {
      failure("invalid_token", "Invalid Supernova stream selection.");
    }
    if (settings["playback.mode"] === "direct") failure("settings_changed", "TorBox playback is disabled.");
    const key = keyFor(settings);
    const record = {infoHash:S.hash(token.infoHash),title:String(token.title || ""),sources:Array.isArray(token.sources) ? token.sources : []};
    const entry = cached([record], key)[record.infoHash];
    const cachedFile = entry && S.chooseFile(entry.files, record, lookup, true, entry.name);
    if (!cachedFile || cachedFile.infected === true || cachedFile.zipped === true || String(cachedFile.name || cachedFile.path) !== token.fileName) failure("not_cached", "This torrent is no longer cached on TorBox.");
    const listed = api("mylist?limit=100",key);
    let torrent = Array.isArray(listed) ? listed.find(item => S.hash(item.hash) === record.infoHash && item.download_finished === true && item.download_present !== false) : null;
    if (!torrent) {
      try {
        const created = api("createtorrent",key,{type:"multipart",value:{
          magnet:`magnet:?xt=urn:btih:${record.infoHash}`,add_only_if_cached:"true",seed:"3",allow_zip:"false",
        }});
        const createdId = created && S.number(created.torrent_id);
        if (!created || S.hash(created.hash) !== record.infoHash || createdId === null) failure("invalid_response", "TorBox could not confirm the cached torrent.");
        const detail = api(`mylist?id=${createdId}&bypass_cache=true`,key);
        torrent = Array.isArray(detail) ? detail.find(item => S.number(item.id) === createdId) : detail;
      } catch (error) {
        if (error.code !== "already_exists") throw error;
        // Account lists may lag or omit older entries. One bounded fresh list
        // retry reconciles duplicate refusal without issuing another create.
        const fresh = api("mylist?limit=1000&bypass_cache=true",key);
        torrent = Array.isArray(fresh) ? fresh.slice(0,1000).find(item => S.hash(item.hash) === record.infoHash) : null;
      }
    }
    if (!torrent || S.hash(torrent.hash) !== record.infoHash || S.number(torrent.id) === null || torrent.download_finished !== true || torrent.download_present === false) {
      failure("not_cached", "TorBox has not made this cached torrent available yet. Try again later.");
    }
    // TorBox file IDs are independently reselected from the account list.
    // They are never inferred from P2P file indexes or cached list ordering.
    const file = S.chooseFile(torrent.files, record, lookup, true, torrent.name);
    if (!file || String(file.name || file.path) !== token.fileName || file.infected === true || file.zipped === true) failure("ambiguous_file", "Cannot determine the correct video file.");
    // TorBox's documented requestdl endpoint requires this token parameter.
    // Keep it internal to the HTTP host: no row/cache/log ever stores this URL.
    const url = api(`requestdl?token=${encodeURIComponent(key)}&torrent_id=${S.number(torrent.id)}&file_id=${file.selectedId}&redirect=false`,key);
    if (typeof url !== "string" || !/^https:\/\/[^\s/@]+(?:\/|$)/i.test(url) || url.length > 8192) failure("invalid_url", "TorBox returned an invalid playback URL.");
    // The Rust final-only policy validates credential-bearing CDN URLs and
    // marks them for transient in-app playback; listed rows never carry links.
    return {id:"supernova:resolved",title:"Supernova · TorBox",description:record.title,url};
  };
})();
