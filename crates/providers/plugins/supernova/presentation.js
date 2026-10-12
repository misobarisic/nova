(() => {
  "use strict";
  const S = globalThis.Supernova;
  // Release labels are hints, never inspected bitrate, audio-track inventory,
  // or proof of quality. Keep transport URLs out of human-readable labels.
  const display = value => String(value || "").replace(/(?:https?:\/\/|magnet:\?)[^\s]+/gi,"[link]").replace(/[\u0000-\u001f\u007f]/g," ").trim();
  const basename = value => String(value || "").split(/[\\/]/).pop();
  const languageLabels = {en:"English",ja:"Japanese",hr:"Croatian",de:"German",fr:"French",es:"Spanish",it:"Italian",pt:"Portuguese",ru:"Russian",ko:"Korean",zh:"Chinese",ar:"Arabic",hi:"Hindi",nl:"Dutch",pl:"Polish",tr:"Turkish",sv:"Swedish",no:"Norwegian",da:"Danish",fi:"Finnish",cs:"Czech",hu:"Hungarian",ro:"Romanian",uk:"Ukrainian",el:"Greek",he:"Hebrew",th:"Thai",vi:"Vietnamese",id:"Indonesian"};
  const releaseMetadata = title => {
    const text = String(title || "");
    const source = /\bWEB[ ._-]*DL\b/i.test(text) ? "WEB-DL" : /\bWEB[ ._-]*RIP\b/i.test(text) ? "WEBRip" :
      /\b(?:blu[ ._-]*ray|b[dr]rip)\b/i.test(text) ? (/\bremux\b/i.test(text) ? "Blu-ray REMUX" : "Blu-ray") :
      /\bHDTV\b/i.test(text) ? "HDTV" : /\bDVD[ ._-]*RIP\b/i.test(text) ? "DVDRip" :
      /\b(?:CAM(?:RIP)?|HDCAM)\b/i.test(text) ? "CAM" : /\b(?:TELESYNC|TS)\b/i.test(text) ? "TS" :
      /\b(?:TELECINE|HDTC|TC)\b/i.test(text) ? "TC" : /\bWEB\b/i.test(text) ? "WEB" : null;
    const codec = S.releaseVideoCodec(text);
    const video = codec ? {avc:"AVC",hevc:"HEVC",av1:"AV1"}[codec] : /\bXviD\b/i.test(text) ? "XviD" : null;
    const group = text.replace(/\.(?:mkv|mp4|avi|m4v|mov|ts|webm)$/i,"").match(/-([A-Za-z][A-Za-z0-9]{1,31})$/);
    const bracket = text.match(/^\s*\[([A-Za-z][A-Za-z0-9 _-]{1,31})\]/);
    const releaseGroup = group && !/^(?:DL|RIP|HD|AAC|DDP|AVC|HEVC|AV1)$/i.test(group[1]) ? group[1] : bracket && !S.releaseLanguages(bracket[1]).length && !/\b(?:multi|dual|audio|subs?|\d+p)\b/i.test(bracket[1]) ? bracket[1] : null;
    const audio = /\b(?:DTS[ ._-]*HD[ ._-]*MA)\b/i.test(text) ? "DTS-HD MA" : /\bTrue[ ._-]*HD/i.test(text) ? "TrueHD" :
      /\b(?:E[ ._-]*AC[ ._-]*3|DDP)(?=\d|\b)/i.test(text) ? "E-AC-3" : /\b(?:AC[ ._-]*3|DD)(?=\d|\b)/i.test(text) ? "AC-3" :
      /\bDTS(?=\d|\b)/i.test(text) ? "DTS" : /\bAAC(?=\d|\b)/i.test(text) ? "AAC" : /\bFLAC\b/i.test(text) ? "FLAC" : /\bMP3\b/i.test(text) ? "MP3" : null;
    const channels = audio && (text.match(/\b(?:DDP?|DTS|AAC|True[ ._-]*HD|(?:E[ ._-]*)?AC[ ._-]*3)[ ._-]*([1-9])[ ._-]([01])\b/i) || text.match(/\b([1-9])[ ._-]([01])\b/));
    return {source,video,releaseGroup,audio,channels:channels ? `${channels[1]}.${channels[2]}` : null,atmos:/\bAtmos\b/i.test(text)};
  };
  S.cachedDiscoveryDescription = description => {
    const annotation = "Cached discovery; peer availability unverified.";
    if (description.includes(annotation)) return description;
    const lines = description.split("\n");
    const filename = lines.findIndex(line=>line.startsWith("📁 "));
    lines.splice(filename < 0 ? lines.length : filename,0,annotation);
    return lines.join("\n");
  };
  S.row = (record, torbox, rawToken) => {
    const token = torbox ? S.json(rawToken) : null;
    const fileName = token && typeof token.fileName === "string" ? token.fileName : record.fileName || record.title;
    // Strip a URL before taking its basename; otherwise a signed query could
    // survive after the scheme/host was removed.
    const fileLabel = basename(display(fileName));
    const tracks = S.validTrackMetadata(record.trackMetadata)
      && record.trackMetadata.fileName === fileName && record.trackMetadata.fileSize === record.sizeBytes ? record.trackMetadata : null;
    const release = releaseMetadata(record.title);
    const file = releaseMetadata(basename(fileName));
    const metadata = Object.fromEntries(Object.keys(release).map(key=>[key,release[key] || file[key]]));
    const codec = tracks ? tracks.videoCodec : S.releaseVideoCodec([record.title,basename(fileName)]);
    metadata.video = codec ? {avc:"AVC",hevc:"HEVC",av1:"AV1"}[codec] : tracks ? "Codec unknown" : /\bXviD\b/i.test(record.title + " " + basename(fileName)) ? "XviD" : null;
    const resolution = S.resolution(record.title) || S.resolution(basename(fileName));
    const video = [metadata.source,metadata.video].filter(Boolean).map(display);
    const audio = tracks ? [...tracks.audioCodecs,...tracks.audioChannels] : [metadata.audio,metadata.channels,metadata.atmos ? "Atmos" : null].filter(Boolean);
    const languages = tracks ? tracks.audioLanguages : [...new Set([record.title,basename(fileName)].flatMap(S.releaseLanguages))];
    const subtitles = tracks ? tracks.subtitleLanguages : [...new Set([record.title,basename(fileName)].flatMap(S.releaseSubtitleLanguages))];
    const provenance = display(record.sources.join(" + "));
    const peers = torbox ? "" : record.seeders === null ? " · peers unverified" : ` · ${record.seeders} reported seeders`;
    const lines = [video.length ? "🎞️ " + video.join(" · ") : null,audio.length ? "🔊 " + audio.join(" · ") : null,
      "🔎 " + provenance + peers,languages.length || tracks ? "🌐 Audio: " + (languages.map(code=>languageLabels[code] || code.toUpperCase()).join(" · ") || "Unspecified") : null,
      subtitles.length || tracks ? "💬 Subtitles: " + (subtitles.map(code=>languageLabels[code] || code.toUpperCase()).join(" · ") || "Unspecified") : null,
      record.title !== basename(fileName) ? "🗂️ " + display(record.title) : null,"📁 " + fileLabel].filter(Boolean);
    return {
      id:`${record.infoHash}:${record.fileIdx === null || record.fileIdx === undefined ? "unknown" : record.fileIdx}:${torbox ? "torbox" : "p2p"}`,
      title:`${torbox ? "[TB⚡] Supernova" : "Supernova P2P"}${resolution ? " " + resolution + "p" : ""}`,
      description:lines.join("\n"),source:provenance,seeders:torbox ? null : record.seeders,size:record.sizeBytes,
      ...(torbox ? {resolver:{provider_id:"supernova",token:rawToken}} : {info_hash:record.infoHash,file_idx:record.fileIdx}),
    };
  };
})();
