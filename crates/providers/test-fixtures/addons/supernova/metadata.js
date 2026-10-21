(() => {
  "use strict";
  const S = globalThis.Supernova;
  const codes={eng:"en",jpn:"ja",hrv:"hr",deu:"de",ger:"de",fra:"fr",fre:"fr",spa:"es",ita:"it",por:"pt",rus:"ru",kor:"ko",zho:"zh",chi:"zh",ara:"ar",hin:"hi",nld:"nl",dut:"nl",pol:"pl",tur:"tr",swe:"sv",nor:"no",dan:"da",fin:"fi",ces:"cs",cze:"cs",hun:"hu",ron:"ro",rum:"ro",ukr:"uk",ell:"el",gre:"el",heb:"he",tha:"th",vie:"vi",ind:"id"};
  const languages=new Set(Object.values(codes));
  const language=value=>{const code=String(value || "").toLowerCase();return languages.has(code) ? code : codes[code] || null;};
  const audioCodecs={aac:"AAC",ac3:"AC-3",eac3:"E-AC-3",dts:"DTS",truehd:"TrueHD",flac:"FLAC",mp3:"MP3",opus:"Opus",vorbis:"Vorbis",pcm_s16le:"PCM",pcm_s24le:"PCM"};
  const basename=value=>String(value || "").split(/[\\/]/).pop();
  const unique=values=>[...new Set(values.filter(Boolean))];
  S.validTrackMetadata = metadata => {
    if (!metadata || typeof metadata!=="object" || Array.isArray(metadata) || Object.keys(metadata).some(key=>!["audioLanguages","subtitleLanguages","videoCodec","audioCodecs","audioChannels","source","fileName","fileSize","resolution"].includes(key))) return false;
    const list=(values,valid)=>Array.isArray(values) && values.length<=32 && values.every(valid) && new Set(values).size===values.length;
    return (!Object.prototype.hasOwnProperty.call(metadata,"resolution") || metadata.resolution===null || S.validResolutionHeight(metadata.resolution)) &&
      metadata.source==="AnimeTosho" && typeof metadata.fileName==="string" && metadata.fileName.length>0 && metadata.fileName.length<=2048 && S.number(metadata.fileSize)>0 &&
      list(metadata.audioLanguages,value=>languages.has(value)) && list(metadata.subtitleLanguages,value=>languages.has(value)) &&
      [null,"avc","hevc","av1","vp9","other"].includes(metadata.videoCodec) && list(metadata.audioCodecs,value=>Object.values(audioCodecs).includes(value)) &&
      list(metadata.audioChannels,value=>typeof value==="string" && /^(?:[1-9]\d? channels|[1-9]\.[01])$/.test(value));
  };
  // The torrent hash proves membership; a unique basename and exact size prove
  // which selected file the site's probe describes (TorBox may add a folder).
  // Duplicate basenames/sizes are ambiguous. Never union batch tracks.
  S.animetoshoSelectedFile = (torrent,target) => {
    if (!torrent || S.hash(torrent.info_hash)!==target.infoHash || !Array.isArray(torrent.files) || torrent.files.length>1000) return null;
    const matches=torrent.files.filter(file=>file && basename(file.filename)===basename(target.fileName) && S.number(file.size)===target.sizeBytes && S.number(file.id)>0);
    return matches.length===1 ? matches[0] : null;
  };
  S.animetoshoTracks = (file,target,torrentId) => {
    if (!file || (target.fileId!==undefined && S.number(file.id)!==target.fileId) || basename(file.filename)!==basename(target.fileName) || S.number(file.size)!==target.sizeBytes || S.number(file.torrent_id)!==torrentId || !file.info || !file.info.ffprobe) return null;
    const streams=file.info.ffprobe.streams;
    if (!Array.isArray(streams) || !streams.length || streams.length>128) return null;
    const audio=streams.filter(stream=>stream && stream.codec_type==="audio"),subtitles=streams.filter(stream=>stream && stream.codec_type==="subtitle");
    const videoStreams=streams.filter(stream=>stream && stream.codec_type==="video" && !(stream.disposition && stream.disposition.attached_pic));
    const video=videoStreams.map(stream=>({h264:"avc",hevc:"hevc",av1:"av1",vp9:"vp9"})[stream.codec_name] || (typeof stream.codec_name==="string" && stream.codec_name ? "other" : null));
    const metadata={source:"AnimeTosho",fileName:target.fileName,fileSize:target.sizeBytes,
      audioLanguages:unique(audio.map(stream=>language(stream.tags && stream.tags.language))),subtitleLanguages:unique(subtitles.map(stream=>language(stream.tags && stream.tags.language))),
      videoCodec:video.length>0 && video.every(codec=>codec && codec===video[0]) ? video[0] : null,audioCodecs:unique(audio.map(stream=>audioCodecs[stream.codec_name])),
      audioChannels:unique(audio.map(stream=>{const layout=String(stream.channel_layout || "");if (/^[1-9]\.[01](?:\([^)]*\))?$/.test(layout)) return layout.slice(0,3);const channels=S.number(stream.channels);return channels>0 && channels<=64 ? ({1:"1.0",2:"2.0"}[channels] || `${channels} channels`) : null;}))};
    // Missing old probes retain release hints. An explicit malformed or
    // conflicting selected-file height is unknown and overrides those hints.
    if (videoStreams.some(stream=>Object.prototype.hasOwnProperty.call(stream,"height"))) {
      const heights=videoStreams.map(stream=>stream.height);
      metadata.resolution=heights.length && heights.every(height=>S.validResolutionHeight(height) && height===heights[0]) ? heights[0] : null;
    }
    return S.validTrackMetadata(metadata) ? metadata : null;
  };
  S.enrichTrackMetadata = (records,availableRows,deadline,maxRequests,onUpgrade) => {
    const targets=[];
    for (const record of records) {
      if (!(S.number(record.animetoshoId)>0)) continue;
      if (S.number(record.fileIdx)!==null && record.fileName && S.number(record.sizeBytes)>0) targets.push({record,infoHash:record.infoHash,fileName:record.fileName,sizeBytes:record.sizeBytes});
      for (const row of availableRows.values()) {
        const token=row.resolver && S.json(row.resolver.token);
        if (token && token.infoHash===record.infoHash && token.fileName && S.number(row.size)>0 && !targets.some(target=>target.infoHash===record.infoHash && target.fileName===token.fileName && target.sizeBytes===row.size)) targets.push({record,infoHash:record.infoHash,fileName:token.fileName,sizeBytes:row.size});
      }
    }
    const apply=(target,metadata)=>{
      if (target.record.fileName===target.fileName && target.record.sizeBytes===target.sizeBytes) target.record.trackMetadata=metadata;
      for (const [id,row] of availableRows) {
        const token=row.resolver && S.json(row.resolver.token);
        if (token && token.infoHash===target.infoHash && token.fileName===target.fileName && row.size===target.sizeBytes) availableRows.set(id,S.row({...target.record,fileIdx:S.number(id.split(":")[1]),fileName:target.fileName,sizeBytes:target.sizeBytes,trackMetadata:metadata},true,row.resolver.token));
      }
      if (onUpgrade) onUpgrade();
    };
    const pending=[];
    for (const target of targets.slice(0,3)) {
      target.key="tracks:animetosho:v2:"+nova.crypto.hmacSha256Base64Url(JSON.stringify([target.infoHash,target.fileName,target.sizeBytes]),"supernova-tracks-v2");
      const cached=S.resultCacheGet(target.key),now=Math.floor(Date.now()/1000);
      if (cached && Number.isInteger(cached.expiresAt) && cached.expiresAt>now && cached.expiresAt<=now+86400 && (cached.metadata===null || (S.validTrackMetadata(cached.metadata) && cached.metadata.fileName===target.fileName && cached.metadata.fileSize===target.sizeBytes))) { if (cached.metadata) apply(target,cached.metadata); }
      else pending.push(target);
    }
    const selected=pending.slice(0,Math.floor(Math.max(0,maxRequests)/2));
    if (!selected.length || deadline-Date.now()<350) return 0;
    let requests=selected.length;
    const torrentResponses=S.batch(selected.map(target=>({url:`https://animetosho.org/feed/json?show=torrent&id=${target.record.animetoshoId}`,method:"GET",timeoutMs:1200})),deadline);
    const files=[];
    const store=(target,metadata)=>S.resultCacheSet(target.key,{metadata,expiresAt:Math.floor(Date.now()/1000)+(metadata ? 86400 : 60)},metadata ? 86400 : 60);
    selected.forEach((target,index)=>{const response=torrentResponses[index],torrent=response && response.status===200 && S.json(response.body);const file=S.animetoshoSelectedFile(torrent,target);if(file) files.push({...target,fileId:S.number(file.id)});else store(target,null);});
    if (deadline-Date.now()<350) return requests;
    requests+=files.length;
    S.batch(files.map(target=>({url:`https://animetosho.org/feed/json?show=file&id=${target.fileId}`,method:"GET",timeoutMs:1200})),deadline,(index,response)=>{
      const target=files[index],file=response && response.status===200 && S.json(response.body);
      const metadata=S.animetoshoTracks(file,target,target.record.animetoshoId);store(target,metadata);if(metadata) apply(target,metadata);
    });
    return requests;
  };
})();
