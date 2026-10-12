(() => {
  const S = globalThis.Supernova;
  S.sources.apibay = {
    name:"APIBay", anime:false,
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
        sources:["APIBay"],
      })).filter(row => row.infoHash && !/^0{40}$/.test(row.infoHash) && row.title);
    },
  };
})();
