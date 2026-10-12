(() => {
  "use strict";
  const S=globalThis.Supernova;
  S.validDetailReference=detail=>!!(detail && typeof detail==="object" && !Array.isArray(detail)
    && Object.keys(detail).every(key=>["source","id"].includes(key))
    && typeof detail.source==="string" && Number.isSafeInteger(detail.id) && detail.id>0 && detail.id<=9999999999
    && S.sources[detail.source] && typeof S.sources[detail.source].detailRequest==="function" && typeof S.sources[detail.source].detailEvidence==="function");
  S.mergeRecordEvidence=(record,incoming)=>{
    if (S.validReleaseEvidence(incoming.sourceMetadata)) record.sourceMetadata=S.mergeReleaseEvidence(record.sourceMetadata,incoming.sourceMetadata);
    const refs=[...(record.details || []),...(incoming.details || [])].filter(S.validDetailReference);
    record.details=refs.filter((detail,index)=>refs.findIndex(other=>other.source===detail.source && other.id===detail.id)===index).slice(0,8);
  };
  // Evidence has per-field precedence. Actual selected-file probes, including
  // explicitly unknown languages/codecs, outrank uploader descriptions.
  S.recordMetadata=(record,tracks=record.trackMetadata)=>({
    ...(S.validReleaseEvidence(record.sourceMetadata) ? record.sourceMetadata : {}),
    ...(tracks || {}),
  });
  S.enrichReleaseEvidence=(records,availableRows,deadline,maxRequests,onUpgrade,enabledSources=Object.keys(S.sources))=>{
    const pending=[];
    const apply=(record,evidence)=>{
      const merged=S.mergeReleaseEvidence(record.sourceMetadata,evidence);
      if (JSON.stringify(merged)===JSON.stringify(record.sourceMetadata)) return;
      record.sourceMetadata=merged;
      for (const [id,row] of availableRows) {
        const token=row.resolver && S.json(row.resolver.token);
        if (token && token.infoHash===record.infoHash) availableRows.set(id,S.row({...record,fileIdx:S.number(id.split(":")[1]),fileName:token.fileName,sizeBytes:row.size},true,row.resolver.token));
      }
      if (onUpgrade) onUpgrade();
    };
    // Read cached descriptions beyond the cold-fetch cap. This stage is a
    // bounded optional upgrade, not a crawl of every torrent detail page.
    for (const record of records.slice(0,24)) {
      for (const detail of (record.details || []).filter(detail=>S.validDetailReference(detail) && enabledSources.includes(detail.source)).slice(0,2)) {
        const key="evidence:v1:"+nova.crypto.hmacSha256Base64Url(JSON.stringify([record.infoHash,detail]),"supernova-evidence-v1");
        const cached=S.resultCacheGet(key),now=Math.floor(Date.now()/1000);
        if (cached && Number.isInteger(cached.expiresAt) && cached.expiresAt>now && cached.expiresAt<=now+86400 && (cached.evidence===null || S.validReleaseEvidence(cached.evidence))) {
          if (cached.evidence) apply(record,cached.evidence);
        } else pending.push({record,detail,key});
      }
    }
    const targets=pending.slice(0,Math.max(0,Math.min(6,maxRequests)));
    if (!targets.length || deadline-Date.now()<350) return 0;
    // Every URL comes from a source-owned numeric reference, never from an
    // uploader description, information link, comment, or arbitrary HTML URL.
    const requests=targets.map(target=>S.sources[target.detail.source].detailRequest(target.detail));
    S.batch(requests,deadline,(index,response)=>{
      const target=targets[index];
      let evidence=null;
      try {
        if (response && response.status===200 && !response.error) evidence=S.sources[target.detail.source].detailEvidence(response.body,target.detail,target.record.infoHash);
      } catch (_) { /* A source layout change cannot suppress usable streams. */ }
      if (!S.validReleaseEvidence(evidence)) evidence=null;
      const ttl=evidence ? 86400 : 60;
      S.resultCacheSet(target.key,{evidence,expiresAt:Math.floor(Date.now()/1000)+ttl},ttl);
      if (evidence) apply(target.record,evidence);
    });
    return requests.length;
  };
})();
