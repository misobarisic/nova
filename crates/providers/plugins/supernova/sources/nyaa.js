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
        magnet:{selector:'a[href^="magnet:"]',value:"href"},
        torrent:{selector:'a[href^="/download/"]',value:"href"},
        size:{selector:"td:nth-child(4)",value:"text"},
        seeders:{selector:"td:nth-child(6)",value:"text"},
      })));
      return Array.isArray(rows) ? rows.slice(0,150).map(row => {
        const infoHash = S.magnetHash(row.magnet);
        return {title:String(row.title || row.titleText || "").slice(0,1024),infoHash,
          sizeBytes:S.size(row.size),seeders:S.number(row.seeders),
          torrentUrl:S.metainfoUrl(/^\/download\/\d+\.torrent$/.test(row.torrent) ? "https://nyaa.si" + row.torrent : row.torrent,infoHash),sources:["Nyaa"]};
      }).filter(row => row.infoHash && row.title) : [];
    },
  };
})();
