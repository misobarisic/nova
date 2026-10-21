(() => {
  "use strict";
  const S=globalThis.Supernova;
  S.sources.stremthru={
    name:"StremThru",anime:false,
    // Port Comet's IMDb inventory query once, not once per title variant.
    // The official endpoint also supports title/year/category searches.
    queries:lookup=>[lookup.title],
    url:(_query,lookup,_page=0,settings={})=>{
      const base=String(settings["stremthru.url"] || "https://stremthru.13377001.xyz").trim().replace(/\/+$/,"");
      if (base.length>2048 || !S.metainfoUrl(base,null) || /[?#]/.test(base)) return "";
      const identity=/^tt\d+$/.test(lookup.mediaId) ? `imdbid=${lookup.mediaId}` :
        `q=${encodeURIComponent(String(lookup.title).slice(0,200))}&cat=${lookup.mediaType==="movie"?2000:5000}${/^\d{4}$/.test(String(lookup.year || "")) ? "&year="+lookup.year : ""}`;
      return `${base}/v0/torznab/api?t=search&${identity}&limit=300`;
    },
    parse:body=>{
      const roots=S.json(nova.xml.select(body,"rss","{}"));
      if(!Array.isArray(roots) || roots.length!==1)throw new Error("Invalid StremThru feed.");
      const rows=S.json(nova.xml.select(body,"item",JSON.stringify({title:{selector:"title"},description:{selector:"description"},attributes:{selector:"attr",value:"attributes"}})));
      return Array.isArray(rows) ? rows.slice(0,S.CANDIDATE_MAX).flatMap(row=>{
        if(!row.title || row.title.trim()==="Invalid Magnet")return [];
        const attrs={},media=[],list=S.json(row.attributes);
        if(Array.isArray(list))for(const attr of list){
          if(["infohash","magneturl","size","indexername","imdb"].includes(attr.name)){
            if(attrs[attr.name]!==undefined && attrs[attr.name]!==attr.value)return [];
            attrs[attr.name]=attr.value;
          }
          if (["audio","resolution","video"].includes(attr.name) && typeof attr.value==="string" && media.length<32) {
            const value=attr.name==="resolution" && /^[1-9]\d{2,3}$/.test(attr.value) && S.validResolutionHeight(Number(attr.value)) ? attr.value+"p" : attr.value;
            media.push(({audio:"Audio",resolution:"Resolution",video:"Video"})[attr.name]+": "+value.slice(0,256));
          }
        }
        const infoHash=S.hash(attrs.infohash),magnetHash=S.magnetHash(attrs.magneturl);
        if(!infoHash || /^0{40}$/.test(infoHash) || (magnetHash && magnetHash!==infoHash))return [];
        const label=String(attrs.indexername || "").trim();
        const safeLabel=/^[A-Za-z][A-Za-z0-9 ._+-]{0,47}$/.test(label) && !/\b(?:bearer|token|secret|key|authorization|password|cookie)\b|[a-f\d]{16,}/i.test(label);
        const imdb=String(attrs.imdb || "").replace(/^tt/,"");
        // These are the media attributes emitted by StremThru's FeedItem,
        // independent of its optional description and indexer label.
        // The generic `language` attribute does not identify a track role.
        return [{title:String(row.title).slice(0,1024),infoHash,sizeBytes:S.number(attrs.size),seeders:null,
          imdb:/^\d+$/.test(imdb)?"tt"+imdb:null,torrentUrl:S.metainfoUrl(null,infoHash),sources:["StremThru"+(safeLabel?"|"+label:"")],
          sourceMetadata:S.mergeReleaseEvidence(S.releaseEvidence(row.description),S.releaseEvidence(media.join('\n')))}];
      }) : [];
    },
  };
})();
