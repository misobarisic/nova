//! Shared description enrichment, ownership and ranking regressions.
use crate::{MemoryProviderHost, PluginManifest, PluginRuntime};
use serde_json::{Value, json};

fn evaluate(script: &str) -> Value {
    let definition = crate::bundled_providers()
        .iter()
        .find(|p| p.url == crate::SUPERNOVA_PROVIDER_URL)
        .unwrap();
    let manifest: PluginManifest = serde_json::from_str(definition.plugin_manifest).unwrap();
    PluginRuntime::new(
        manifest,
        format!(
            "{}\nglobalThis.novaProvider={{handle(){{const S=globalThis.Supernova;{script}}}}};",
            definition.source
        ),
    )
    .invoke(MemoryProviderHost::new(), &json!({"op":"streams"}))
    .unwrap()
}

#[test]
fn descriptions_influence_rows_and_ranking_but_verified_tracks_win() {
    let result = evaluate(
        r#"
      const base={title:'Example S01E02 Japanese AVC',sources:['Nyaa'],fileIdx:0,fileName:'Example S01E02.mkv',sizeBytes:42,seeders:1};
      const sourceMetadata=S.releaseEvidence('Audio: English\nSubtitles: French\nVideo: HEVC 1080p');
      const records=[{...base,infoHash:'a'.repeat(40),sourceMetadata},{...base,infoHash:'b'.repeat(40),sourceMetadata,
        trackMetadata:{source:'AnimeTosho',fileName:base.fileName,fileSize:42,audioLanguages:['ja'],subtitleLanguages:[],videoCodec:'avc',audioCodecs:['AAC'],audioChannels:['2.0']}}];
      const settings={'quality.preferred':'any','language.prefs':['en','ja'],'sort.order':[{key:'language',direction:'desc'},...S.SORT_DEFAULT.filter(c=>c.key!=='language')]};
      const rows=records.map(record=>S.row(record,false));
      return {rank:S.rank(records.slice().reverse(),settings).map(r=>r.infoHash),sort:S.sortRows(rows.slice().reverse(),settings).map(r=>r.id),rows};
    "#,
    );
    assert_eq!(result["rank"], json!(["a".repeat(40), "b".repeat(40)]));
    assert!(result["sort"][0].as_str().unwrap().starts_with('a'));
    let unprobed = result["rows"][0]["description"].as_str().unwrap();
    assert!(unprobed.contains("🌐 Audio: English"));
    assert!(unprobed.contains("💬 Subtitles: French"));
    assert!(unprobed.contains("🎞️ HEVC"));
    let verified = result["rows"][1]["description"].as_str().unwrap();
    assert!(verified.contains("🌐 Audio: Japanese"));
    assert!(verified.contains("💬 Subtitles: Unspecified"));
    assert!(verified.contains("🎞️ AVC"));
}

#[test]
fn detail_fetches_are_bounded_cached_and_respect_enabled_sources() {
    let result = evaluate(
        r#"
      const cache=new Map(),urls=[];
      S.resultCacheGet=key=>cache.get(key);S.resultCacheSet=(key,value)=>cache.set(key,value);
      const records=Array.from({length:8},(_,index)=>({infoHash:(index+1).toString(16).padStart(40,'0'),details:[{source:'nyaa',id:index+1}]}));
      S.batch=(requests,deadline,callback)=>requests.map((request,index)=>{
        urls.push(request.url);const id=Number(request.url.split('/').pop()),hash=records[id-1].infoHash;
        const body='<html><div class="panel-body"><kbd>'+hash+'</kbd></div><a href="/download/'+id+'.torrent">torrent</a><a href="magnet:?xt=urn:btih:'+hash+'">magnet</a><div id="torrent-description">Audio: Japanese\nSubtitles: English</div></html>';
        callback(index,{status:200,body});return {};
      });
      const disabled=S.enrichReleaseEvidence(records,new Map(),Date.now()+3000,6,()=>{},['animetosho']);
      const cold=S.enrichReleaseEvidence(records,new Map(),Date.now()+3000,3,()=>{},['nyaa']);
      const copies=records.map(({sourceMetadata,...record})=>record);
      const warm=S.enrichReleaseEvidence(copies,new Map(),Date.now()+3000,0,()=>{},['nyaa']);
      return {disabled,cold,warm,urls,metadata:copies.map(r=>r.sourceMetadata || null),valid:[{source:'nyaa',id:0},{source:'nyaa',id:'1?url=https://example.org'},{source:'missing',id:1},{source:'nyaa',id:1,url:'https://example.org'}].map(S.validDetailReference)};
    "#,
    );
    assert_eq!(result["disabled"], 0);
    assert_eq!(result["cold"], 3);
    assert_eq!(result["warm"], 0);
    assert_eq!(result["urls"].as_array().unwrap().len(), 3);
    assert_eq!(result["metadata"][0]["audioLanguages"], json!(["ja"]));
    assert_eq!(result["metadata"][2]["subtitleLanguages"], json!(["en"]));
    assert!(result["metadata"][3].is_null());
    assert_eq!(result["valid"], json!([false, false, false, false]));
}

#[test]
fn nyaa_evidence_rejects_wrong_hash_page_and_injected_reference() {
    let result = evaluate(
        r#"
      const hash='a'.repeat(40),detail={source:'nyaa',id:7};
      const page=(hash,id=7)=>'<html><div class="panel-body"><kbd>'+hash+'</kbd></div><a href="/download/'+id+'.torrent">torrent</a><a href="magnet:?xt=urn:btih:'+hash+'">magnet</a><div id="torrent-description">Audio: Japanese</div></html>';
      return {valid:S.sources.nyaa.detailEvidence(page(hash),detail,hash),wrongHash:S.sources.nyaa.detailEvidence(page('b'.repeat(40)),detail,hash),wrongId:S.sources.nyaa.detailEvidence(page(hash,8),detail,hash),injected:S.sources.nyaa.detailRequest({source:'nyaa',id:'7?url=https://example.org'})};
    "#,
    );
    assert_eq!(result["valid"]["audioLanguages"], json!(["ja"]));
    for key in ["wrongHash", "wrongId", "injected"] {
        assert!(result[key].is_null(), "{key}: {result}");
    }
}
