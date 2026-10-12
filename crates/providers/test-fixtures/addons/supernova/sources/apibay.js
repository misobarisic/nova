(() => {
  const S = globalThis.Supernova;
  const sourceId=value=>/^[1-9]\d{0,9}$/.test(String(value))?Number(value):null;
  const evidence=row=>S.releaseEvidence(typeof row.descr==='string'?row.descr:'');
  S.sources.apibay = {
    name:"The Pirate Bay / APIBay", anime:false,
    url: query => `https://apibay.org/q.php?q=${encodeURIComponent(query)}&cat=200`,
    parse: body => {
      const rows = S.json(body);
      if (!Array.isArray(rows)) throw new Error("Invalid APIBay feed.");
      return rows.slice(0,100).map(row => ({
        title:String(row.name || "").slice(0,1024),infoHash:S.hash(row.info_hash),
        imdb:/^tt\d+$/.test(row.imdb || "") ? row.imdb : null,
        sizeBytes:S.number(row.size),seeders:S.number(row.seeders),
        // The old .org endpoint redirects through HTTP. Use the canonical
        // HTTPS metainfo endpoint while preserving the host's downgrade ban.
        torrentUrl:S.hash(row.info_hash) ? `https://itorrents.net/torrent/${row.info_hash.toUpperCase()}.torrent` : null,
        sources:["The Pirate Bay / APIBay"],
        sourceMetadata:evidence(row),
        details:sourceId(row.id)?[{source:'apibay',id:sourceId(row.id)}]:[],
      })).filter(row => row.infoHash && !/^0{40}$/.test(row.infoHash) && row.title);
    },
    detailRequest:descriptor=>descriptor && descriptor.source==='apibay' && sourceId(descriptor.id)
      ? {url:`https://apibay.org/t.php?id=${sourceId(descriptor.id)}`,method:"GET",timeoutMs:1000} : null,
    detailEvidence:(body,descriptor,expectedHash)=>{
      if (!descriptor || descriptor.source!=='apibay' || !sourceId(descriptor.id)) return null;
      const row=S.json(body),hash=S.hash(expectedHash);
      // An API response must prove both the requested inventory ID and the
      // torrent identity before its uploader text can enrich this release.
      if (!row || typeof row!=='object' || Array.isArray(row) || !hash || /^0{40}$/.test(hash)
          || sourceId(row.id)!==sourceId(descriptor.id) || S.hash(row.info_hash)!==hash) return null;
      return evidence(row);
    },
  };
})();
