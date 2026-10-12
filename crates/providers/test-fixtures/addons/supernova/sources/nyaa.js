(() => {
  "use strict";
  const S = globalThis.Supernova;
  S.sources.nyaa = {
    name: "Nyaa", anime: true,
    // Comet searches title queries without forcing a language/category or
    // seeder sort. Only advertised pages enter the bounded lookup crawl.
    url: (query, _lookup, page = 0) => `https://nyaa.si/?q=${encodeURIComponent(query).replace(/%20/g,"+")}${page ? "&p=" + (page + 1) : ""}`,
    pages: body => {
      const links = S.json(nova.html.select(body,".pagination a[href]",JSON.stringify({href:{selector:"",value:"href"}})));
      return Array.isArray(links) ? Math.min(6,Math.max(1,...links.map(link=>Number((String(link.href).match(/(?:[?&])p=(\d+)(?:&|$)/) || [])[1]) || 1))) : 1;
    },
    parse: body => {
      const rows = S.json(nova.html.select(body, ".torrent-list tbody tr", JSON.stringify({
        title:{selector:'td:nth-child(2) a[href^="/view/"]:not(.comments)',value:"title"},
        titleText:{selector:'td:nth-child(2) a[href^="/view/"]:not(.comments)',value:"text"},
        detail:{selector:'td:nth-child(2) a[href^="/view/"]:not(.comments)',value:"href"},
        magnet:{selector:'a[href^="magnet:"]',value:"href"},
        torrent:{selector:'a[href^="/download/"]',value:"href"},
        size:{selector:"td:nth-child(4)",value:"text"},
        seeders:{selector:"td:nth-child(6)",value:"text"},
      })));
      return Array.isArray(rows) ? rows.slice(0,150).map(row => {
        const infoHash = S.magnetHash(row.magnet);
        const detail = String(row.detail || "").match(/^\/view\/([1-9]\d{0,9})$/);
        return {title:String(row.title || row.titleText || "").slice(0,1024),infoHash,
          sizeBytes:S.size(row.size),seeders:S.number(row.seeders),
          torrentUrl:S.metainfoUrl(/^\/download\/\d+\.torrent$/.test(row.torrent) ? "https://nyaa.si" + row.torrent : row.torrent,infoHash),sources:["Nyaa"],
          ...(detail ? {details:[{source:"nyaa",id:Number(detail[1])}]} : {})};
      }).filter(row => row.infoHash && row.title) : [];
    },
    detailRequest: detail => detail && detail.source==="nyaa" && Number.isSafeInteger(detail.id) && detail.id>0 && detail.id<=9999999999
      ? {url:`https://nyaa.si/view/${detail.id}`,method:"GET",timeoutMs:1000} : null,
    detailEvidence: (body,detail,expectedHash) => {
      if (!S.sources.nyaa.detailRequest(detail) || !S.hash(expectedHash)) return null;
      const pages=S.json(nova.html.select(body,"html",JSON.stringify({
        description:{selector:"#torrent-description",value:"text"},
        hash:{selector:".panel-body kbd",value:"text"},
        magnet:{selector:'a[href^="magnet:"]',value:"href"},
        torrent:{selector:'a[href^="/download/"]',value:"href"},
      })));
      if (!Array.isArray(pages) || pages.length!==1) return null;
      const page=pages[0];
      // Follow a known source ID only. A redirect or changed page must still
      // prove this exact torrent before its uploader's description is used.
      if (S.hash(String(page.hash || "").trim())!==expectedHash || S.magnetHash(page.magnet)!==expectedHash || page.torrent!==`/download/${detail.id}.torrent`) return null;
      return S.releaseEvidence(page.description);
    },
  };
})();
