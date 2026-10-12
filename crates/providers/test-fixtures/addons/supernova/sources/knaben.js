(() => {
  "use strict";
  const S = globalThis.Supernova;
  const feed = body => {
    const result = S.json(body);
    if (!result || typeof result !== "object" || Array.isArray(result) || !Array.isArray(result.hits)) throw new Error("Invalid Knaben feed.");
    return result;
  };
  S.sources.knaben = {
    name:"Knaben",anime:false,
    // API v2 is a cacheable public GET endpoint; its default unsafe/XXX
    // exclusions stay enabled. Include zero-peer inventory for TorBox checks;
    // reported peers alone never prove playback. Shared limits bound pages.
    url:(query,_lookup,page=0)=>`https://api.knaben.org/v2/search?q=${encodeURIComponent(query)}&o=seeders&d=desc&s=150&f=${page*150}&dead`,
    pages:body=>{
      const result=feed(body);
      const total=S.number(result.total && result.total.value);
      return total === null ? 1 : Math.min(6,Math.max(1,Math.ceil(total/150)));
    },
    parse:body=>feed(body).hits.slice(0,150).flatMap(row=>{
      if (!row || typeof row !== "object" || typeof row.title !== "string" || !row.title.trim()) return [];
      const supplied=S.hash(row.hash),magnet=S.magnetHash(row.magnetUrl);
      if ((row.hash && !supplied) || (supplied && magnet && supplied!==magnet)) return [];
      const infoHash=supplied || magnet;
      if (!infoHash || /^0{40}$/.test(infoHash)) return [];
      return [{title:row.title.slice(0,1024),infoHash,sizeBytes:S.number(row.bytes),
        seeders:S.number(row.liveSeeders) ?? S.number(row.seeders),
        // Knaben's `link` may be an indexer download proxy, not metainfo.
        // Use the hash mirror and retain exact hash/file verification.
        torrentUrl:S.metainfoUrl(null,infoHash),sources:["Knaben"],
        // The API's searchable description is source evidence; external
        // details/link URLs are never followed or retained for enrichment.
        sourceMetadata:S.releaseEvidence(typeof row.description==='string'?row.description:'')}];
    }),
  };
})();
