(() => {
  "use strict";
  const S = globalThis.Supernova;
  S.sources.animetosho = {
    name:"AnimeTosho",anime:true,
    // Port Comet's Torznab request shape; main-site JSON remains available
    // when the separate feed host or Torznab service cannot be reached.
    url: (query,_lookup,page=0) => `https://feed.animetosho.org/api?t=search&q=${encodeURIComponent(query).replace(/%20/g,"+")}&offset=${page * 150}&limit=150`,
    fallbackUrl: query => `https://animetosho.org/feed/json?q=${encodeURIComponent(query)}`,
    pages: body => {
      if (!String(body).trimStart().startsWith("<")) return 1;
      const response=S.json(nova.xml.select(body,"response",JSON.stringify({total:{selector:"",value:"total"}})));
      return Array.isArray(response) && response.some(row => S.number(row.total) > 150) ? 2 : 1;
    },
    parse: body => {
      const structured = S.json(body);
      if (Array.isArray(structured)) return structured.slice(0,150).flatMap(row => {
        const infoHash=S.hash(row.info_hash);
        if (!infoHash || !row.title) return [];
        return [{title:String(row.title).slice(0,1024),infoHash,sizeBytes:S.number(row.total_size),seeders:S.number(row.seeders),
          torrentUrl:S.metainfoUrl(row.torrent_url,infoHash),sources:["AnimeTosho"]}];
      });
      if (!String(body).trimStart().startsWith("<")) throw new Error("Invalid AnimeTosho feed.");
      const roots=S.json(nova.xml.select(body,"rss","{}"));
      if (!Array.isArray(roots) || roots.length!==1) throw new Error("Invalid AnimeTosho RSS feed.");
      const rows=S.json(nova.xml.select(body,"item",JSON.stringify({
        title:{selector:"title"},description:{selector:"description"},torrent:{selector:"enclosure",value:"url"},
        // Namespace prefixes vary; bounded direct-child local names keep
        // attributes associated with their own RSS item.
        attributes:{selector:"attr",value:"attributes"},
      })));
      return Array.isArray(rows) ? rows.slice(0,150).flatMap(row => {
        const attributes=S.json(row.attributes);
        const attrs={};
        if (Array.isArray(attributes)) for (const attr of attributes) {
          if (["infohash","size","seeders","magneturl"].includes(attr.name)) {
            if (attrs[attr.name] !== undefined && attrs[attr.name] !== attr.value) return [];
            attrs[attr.name]=attr.value;
          }
        }
        let magnet=attrs.magneturl;
        if (!magnet) {
          const links=S.json(nova.html.select(row.description || "",'a[href^="magnet:"]',JSON.stringify({magnet:{selector:"",value:"href"}})));
          if (Array.isArray(links) && links.length===1) magnet=links[0].magnet;
        }
        const infoHash=S.hash(attrs.infohash) || S.magnetHash(magnet);
        const magnetHash=S.magnetHash(magnet);
        if (!infoHash || !row.title || (magnetHash && magnetHash!==infoHash)) return [];
        return [{title:String(row.title).slice(0,1024),infoHash,sizeBytes:S.number(attrs.size),seeders:S.number(attrs.seeders),
          torrentUrl:S.metainfoUrl(row.torrent,infoHash),sources:["AnimeTosho"]}];
      }) : [];
    },
  };
})();
