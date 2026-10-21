(() => {
  "use strict";
  const S = globalThis.Supernova;
  // Descriptions are untrusted prose. Only labelled technical fields and
  // standalone encode specifications establish facts; plots and names do not.
  const aliases = {
    en:"en|eng|english",ja:"ja|jpn|japanese",hr:"hr|hrv|croatian",de:"de|deu|ger|german",
    fr:"fr|fra|fre|french",es:"es|spa|spanish",it:"it|ita|italian",pt:"pt|por|portuguese",
    ru:"ru|rus|russian",ko:"ko|kor|korean",zh:"zh|zho|chi|chinese|mandarin|cantonese",
    ar:"ar|ara|arabic",hi:"hi|hin|hindi",nl:"nl|nld|dut|dutch",pl:"pl|pol|polish",
    tr:"tr|tur|turkish",sv:"sv|swe|swedish",no:"no|nor|norwegian",da:"da|dan|danish",
    fi:"fi|fin|finnish",cs:"cs|ces|cze|czech",hu:"hu|hun|hungarian",ro:"ro|ron|rum|romanian",
    uk:"uk|ukr|ukrainian",el:"el|ell|gre|greek",he:"he|heb|hebrew",th:"th|tha|thai",
    vi:"vi|vie|vietnamese",id:"id|ind|indonesian"
  };
  const languagePattern = new RegExp("\\b(?:"+Object.values(aliases).join("|")+")\\b","gi");
  const languageMap = new Map(Object.entries(aliases).flatMap(([code,names])=>names.split("|").map(name=>[name,code])));
  const videoPatterns = [["avc",/\b(?:avc|h[ .-]?264|x264|mpeg[ -]?4 avc)\b/gi],["hevc",/\b(?:hevc|h[ .-]?265|x265)\b/gi],["av1",/\bav1\b/gi],
    ["vp9",/\bvp[ -]?9\b/gi],["other",/\b(?:vp[ -]?8|mpeg[ -]?2|mpeg[ -]?4 visual|xvid|divx|h[ .-]?263|vc[ -]?1|wmv3|prores)\b/gi]];
  const audioPatterns = [["E-AC-3",/\b(?:e[ .-]?ac[ .-]?3|ddp|dd\+)\b/gi],["AC-3",/\b(?:ac[ .-]?3|dd)\b/gi],
    ["TrueHD",/\btrue[ .-]?hd\b/gi],["DTS",/\bdts(?:[ .-]?hd(?:[ .-]?ma)?)?\b/gi],
    ["AAC",/\baac(?:[ .-]?lc)?\b/gi],["FLAC",/\bflac\b/gi],["MP3",/\bmp3\b/gi],
    ["Opus",/\bopus\b/gi],["Vorbis",/\bvorbis\b/gi],["PCM",/\b(?:l?pcm|pcm_s(?:16|24)le)\b/gi]];
  const sources = new Set(["WEB-DL","WEBRip","Blu-ray","Blu-ray REMUX","BDRip","BRRip","DVD","DVDRip","HDTV","TVRip","Remux"]);
  const sourcePatterns = [["WEB-DL",/\bweb[ .-]?dl\b/gi],["WEBRip",/\bweb[ .-]?rip\b/gi],
    ["Blu-ray",/\bblu[ .-]?ray\b/gi],["BDRip",/\bbd[ .-]?rip\b/gi],["BRRip",/\bbr[ .-]?rip\b/gi],
    ["DVDRip",/\bdvd[ .-]?rip\b/gi],["DVD",/\bdvd\b/gi],["HDTV",/\bhdtv\b/gi],["TVRip",/\btv[ .-]?rip\b/gi],["Remux",/\bremux\b/gi]];
  const own = (value,key)=>Object.prototype.hasOwnProperty.call(value,key);
  const keys = ["audioLanguages","subtitleLanguages","videoCodec","resolution","audioCodecs","audioChannels","releaseSource"];
  const scalars = new Set(["videoCodec","resolution","releaseSource"]);
  const channelValid = value=>typeof value==="string" && value.length<=16 && /^(?:[1-9]\d? channels|[1-9]\.[01])$/.test(value);
  S.validReleaseEvidence = value => {
    if (!value || typeof value!=="object" || Array.isArray(value) || !Object.keys(value).length || Object.keys(value).some(key=>!keys.includes(key))) return false;
    const list = (key,valid)=>!own(value,key) || (Array.isArray(value[key]) && value[key].length<=32 && new Set(value[key]).size===value[key].length && value[key].every(valid));
    return list("audioLanguages",item=>languageMap.has(item) && languageMap.get(item)===item) &&
      list("subtitleLanguages",item=>languageMap.has(item) && languageMap.get(item)===item) &&
      list("audioCodecs",item=>audioPatterns.some(([codec])=>codec===item)) && list("audioChannels",channelValid) &&
      (!own(value,"videoCodec") || [null,"avc","hevc","av1","vp9","other"].includes(value.videoCodec)) &&
      (!own(value,"resolution") || value.resolution===null || S.validResolutionHeight(value.resolution)) &&
      (!own(value,"releaseSource") || value.releaseSource===null || sources.has(value.releaseSource));
  };
  S.mergeReleaseEvidence = (left,right) => {
    const result = {};
    for (const evidence of [left,right]) {
      if (!S.validReleaseEvidence(evidence)) continue;
      for (const key of keys) {
        if (!own(evidence,key)) continue;
        if (scalars.has(key)) {
          // Null records a conflict, rather than absence, and remains sticky.
          const blurayRefinement=key==="releaseSource" && [result[key],evidence[key]].every(value=>["Blu-ray","Blu-ray REMUX"].includes(value));
          result[key]=!own(result,key) ? evidence[key] : result[key]===evidence[key] ? result[key] : blurayRefinement ? "Blu-ray REMUX" : null;
        } else result[key]=[...new Set([...(result[key] || []),...evidence[key]])].slice(0,32);
      }
    }
    return Object.keys(result).length ? result : null;
  };
  const matches = (text,patterns) => {
    const found=[];
    let remaining=text;
    for (const [value,pattern] of patterns) {
      pattern.lastIndex=0;
      if (pattern.test(remaining)) {
        found.push(value);
        // Longer aliases consume their short form (E-AC-3 must not add AC-3).
        pattern.lastIndex=0;
        remaining=remaining.replace(pattern," ");
      }
    }
    return found;
  };
  const languages = text => {
    if (text.length>1024) return [];
    const normalized=text.toLowerCase().replace(/\b([a-z]{2,3})[_-](?:[a-z]{2}|\d{3})\b/g,(value,code)=>languageMap.has(code) ? code : value).replace(/\[[^\]\r\n]{0,64}\]|\([^()\r\n]{0,64}\)/g," ");
    const claims=value=>[...value.matchAll(languagePattern)].map(match=>languageMap.get(match[0]));
    const remainder=value=>{
      value=value.replace(languagePattern," ");
      for (const [,pattern] of audioPatterns) value=value.replace(pattern," ");
      // Multi / Dual qualify an inventory but supply no unnamed language.
      return value.replace(/\b(?:multi|dual|audio|subtitles?|subs?|dub(?:bed)?|original|native|full|forced|signs|songs|sdh|cc|ass|ssa|srt|pgs|utf[ -]?8|stereo|mono|surround|channels?|bits?|default|commentary|latin american|brazilian)\b/g," ")
        .replace(/\d+(?:\.\d+)?\s*(?:kbps|mbps|khz|hz)?/g," ").replace(/[\s,;:/+&|_.=-]+/g,"");
    };
    const found=claims(normalized);
    if (!remainder(normalized)) return [...new Set(found)].slice(0,32);
    // An explicitly delimited inventory can contain unsupported language names
    // or opaque locale tags. Preserve the supported items, but reject the whole
    // inventory if an item includes prose around a recognized language.
    const items=normalized.split(/\s+-\s+|[,;/+&|]/).map(item=>item.trim()).filter(Boolean);
    if (items.length<2 || items.length>32) return [];
    for (const item of items) {
      if (!remainder(item)) continue;
      if (claims(item).length || !/^(?:[a-z]{2,24}|(?:simplified|traditional)[ _]?(?:cr|chinese))$/.test(item)) return [];
    }
    return [...new Set(found)].slice(0,32);
  };
  const channelValues = text => {
    const values=[...text.matchAll(/\b([1-9]\.[01])\b(?!\s*(?:kbps|mbps|khz|hz)\b)/gi)].map(match=>match[1]);
    for (const match of text.matchAll(/\b(\d{1,2})\s*channels?\b/gi)) {
      const count=Number(match[1]);
      if (count>0 && count<=64) values.push(({1:"1.0",2:"2.0"})[count] || `${count} channels`);
    }
    if (/\bstereo\b/i.test(text)) values.push("2.0");
    if (/\bmono\b/i.test(text)) values.push("1.0");
    return [...new Set(values)].slice(0,32);
  };
  const scalar = values => values.length===1 ? values[0] : null;
  const technical = (text,kind) => {
    const evidence={};
    if (kind==="video" || kind==="spec") {
      const codecs=matches(text,videoPatterns);
      if (codecs.length) evidence.videoCodec=codecs.length===1 ? codecs[0] : null;
      const heights=[...text.matchAll(/(?<![a-z\d])(?<!\b\d{1,3}[.,])([1-9]\d{2,3})[pi](?![a-z\d])/gi)].map(match=>Number(match[1]));
      for (const match of text.matchAll(/\b([1-9]\d{2,4})\s*[x×]\s*([1-9]\d{2,3})\b/gi)) { const width=Number(match[1]),height=Number(match[2]); if (width>=height && width<=16384) heights.push(height); }
      if (/\b(?:4k|uhd)\b/i.test(text)) heights.push(2160);
      if (/\b8k\b/i.test(text)) heights.push(4320);
      if (heights.length) { const height=scalar([...new Set(heights)]); evidence.resolution=S.validResolutionHeight(height) ? height : null; }
    }
    if (kind==="audio" || kind==="spec") {
      const codecs=matches(text,audioPatterns),channels=channelValues(text);
      if (codecs.length) evidence.audioCodecs=codecs;
      if (channels.length) evidence.audioChannels=channels;
    }
    if (kind==="source" || kind==="spec") {
      const found=matches(text,sourcePatterns);
      if (found.length) evidence.releaseSource=found.length===2 && found.includes("Blu-ray") && found.includes("Remux") ? "Blu-ray REMUX" : scalar(found);
    }
    return evidence;
  };
  const plain = text => text.normalize("NFKC")
    .replace(/<script\b[^>]*>[\s\S]*?<\/script\s*>|<style\b[^>]*>[\s\S]*?<\/style\s*>|<a\b[^>]*>[\s\S]*?<\/a\s*>/gi," ")
    .replace(/!?(?:\[[^\]\r\n]*\])\([^\r\n)]*\)/g," ")
    .replace(/https?:\/\/[^\s<>]+/gi," ")
    .replace(/<\/(?:td|th)\s*>/gi," | ").replace(/<\/?(?:p|div|tr|br|li|h[1-6]|pre)\b[^>]*>/gi,"\n")
    .replace(/<[^>]*>/g," ").replace(/&(?:nbsp|amp|lt|gt|quot|apos);/gi,entity=>({"&nbsp;":" ","&amp;":"&","&lt;":"<","&gt;":">","&quot;":"\"","&apos;":"'"})[entity.toLowerCase()])
    .replace(/[*`_]/g," ");
  S.releaseEvidence = text => {
    if (typeof text!=="string" || !text.trim() || text.length>65536) return null;
    const lines=plain(text).replace(/(?:[|;/]\s*|\s{2,})(?=(?:audio|subtitles?|subs?|video|resolution|source)\s*:)/gi,"\n").split(/\r?\n/);
    if (lines.length>2048) return null;
    let evidence=null,section=null;
    const add=value=>{evidence=S.mergeReleaseEvidence(evidence,value);};
    for (let line of lines) {
      line=line.trim().replace(/^#{1,6}\s+/,"").replace(/^\|\s*|\s*\|$/g,"").trim();
      if (!line || /^[-| :]+$/.test(line)) continue;
      if (line.length>2048) { section=null;continue; }
      const label=line.match(/^(audio(?:\s+(?:languages?|tracks?))?|subtitles?(?:\s+(?:languages?|tracks?))?|subs?|text|video|resolution|source|release source|encode|encoding|codec|general)(?:\s*#\s*\d+)?\s*(?::|\||$)\s*(.*)$/i);
      if (label) {
        const name=label[1].toLowerCase(),value=label[2].trim();
        const kind=/^(?:subs?|subtitle|text)/.test(name) ? "subtitle" : name.startsWith("audio") ? "audio" : /^(?:source|release source)$/.test(name) ? "source" : /^(?:encode|encoding|codec)$/.test(name) ? "spec" : name==="general" ? "general" : "video";
        section=kind;
        if (kind==="audio" || kind==="subtitle") {
          const found=languages(value);
          if (found.length) add({[kind==="audio" ? "audioLanguages" : "subtitleLanguages"]:found});
        }
        add(technical(value,kind));
        continue;
      }
      const field=line.match(/^(language|format|codec(?: id)?|height|width|channel(?:s|\(s\)| layout)?|resolution|source)\s*(?::|\|)\s*(.+)$/i);
      if (section && field) {
        const name=field[1].toLowerCase(),value=field[2];
        if (name==="language" && (section==="audio" || section==="subtitle")) {
          const found=languages(value);
          if (found.length) add({[section==="audio" ? "audioLanguages" : "subtitleLanguages"]:found});
        } else if (name==="height" && section==="video") {
          const explicit=value.trim().match(/^([1-9]\d{2,3}|[1-9][ ,]\d{3})\s*(?:pixels?)?$/i);
          const height=explicit ? Number(explicit[1].replace(/[ ,]/g,"")) : null;
          if (S.validResolutionHeight(height)) add({resolution:height});
        } else if (name==="source") add(technical(value,"source"));
        else if (name!=="width" && name!=="language") add(technical(value,section));
        continue;
      }
      // MediaInfo interleaves IDs, bitrates and durations with useful fields.
      // These fields preserve the current track section without supplying facts.
      if (section && /^(?:id|unique id|complete name|file size|duration|bit rate(?: mode)?|maximum bit rate|sampling rate|frame rate|format(?: profile| settings)?|compression mode|delay relative to video|stream size|title|default|forced|writing library|encoded date|tagged date|color[^:]*|chroma[^:]*|bit depth|scan type|display aspect ratio)\s*[:|]/i.test(line)) continue;
      if (section==="audio" || section==="subtitle") {
        const found=languages(line);
        if (found.length) {
          add({[section==="audio" ? "audioLanguages" : "subtitleLanguages"]:found});
          add(technical(line,section));
          continue;
        }
      }
      // Filename/group brackets and narrative codec mentions are insufficient.
      // Standalone specs must consist entirely of recognized technical tokens.
      if (!/[\[\]]/.test(line)) {
        const facts=technical(line,"spec");
        let remainder=line;
        for (const [,pattern] of [...videoPatterns,...audioPatterns,...sourcePatterns]) remainder=remainder.replace(pattern," ");
        remainder=remainder.replace(/\b\d{3,5}\s*[x×]\s*\d{3,5}\b|(?<![a-z\d])(?<!\b\d{1,3}[.,])[1-9]\d{2,3}[pi](?![a-z\d])|\b(?:4k|8k|uhd|hdr10\+?|hdr|sdr|dv|dolby vision|softsubbed|hardsubbed|\d+[ -]?bit|stereo|mono)\b/gi," ")
          .replace(/\b\d+(?:\.\d+)?\s*(?:channels?|kbps|mbps|khz|hz)\b|\b[1-9]\.[01]\b/gi," ").replace(/[\s,;:/+&|().=-]+/g,"");
        if (!remainder && (own(facts,"videoCodec") || own(facts,"resolution"))) add(facts);
      }
      section=null;
    }
    return evidence;
  };
})();
