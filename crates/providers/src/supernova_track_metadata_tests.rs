//! Public AnimeTosho JSON probes, reduced to fields used by the adapter.
//! Captured from /feed/json?show=file&id=1377287 and id=1332343.
use crate::{MemoryProviderHost, PluginManifest, PluginRuntime};
use serde_json::{Value, json};

fn evaluate(script: &str) -> Value {
    let definition = crate::bundled_providers()
        .iter()
        .find(|provider| provider.url == crate::SUPERNOVA_PROVIDER_URL)
        .unwrap();
    let manifest: PluginManifest = serde_json::from_str(definition.plugin_manifest).unwrap();
    let source = format!(
        "{}\nglobalThis.novaProvider={{handle(request){{const S=globalThis.Supernova;{} }} }};",
        definition.source, script
    );
    PluginRuntime::new(manifest, source)
        .invoke(MemoryProviderHost::new(), &json!({"op":"streams"}))
        .unwrap()
}

#[test]
fn public_probe_separates_dual_audio_and_subtitles_without_filename_labels() {
    let fixture = include_str!("../test-fixtures/addons/supernova/fixtures/hokuto-tracks.json");
    let result = evaluate(&format!(
        "const file={fixture};return S.animetoshoTracks(file,{{fileName:file.filename,sizeBytes:file.size,fileId:file.id}},file.torrent_id);"
    ));
    assert_eq!(result["audioLanguages"], json!(["ja", "en"]));
    assert!(
        result["subtitleLanguages"]
            .as_array()
            .unwrap()
            .contains(&json!("fr"))
    );
    assert_eq!(result["videoCodec"], "hevc");
    assert_eq!(result["source"], "AnimeTosho");
}

#[test]
fn undefined_probe_languages_stay_unknown() {
    let fixture = include_str!("../test-fixtures/addons/supernova/fixtures/unknown-tracks.json");
    let result = evaluate(&format!(
        "const file={fixture};return S.animetoshoTracks(file,{{fileName:file.filename,sizeBytes:file.size,fileId:file.id}},file.torrent_id);"
    ));
    assert_eq!(result["audioLanguages"], json!([]));
    assert_eq!(result["subtitleLanguages"], json!([]));
    assert_eq!(result["audioCodecs"], json!(["AAC"]));
    assert_eq!(result["audioChannels"], json!(["2.0"]));
}

#[test]
fn selected_file_requires_matching_hash_exact_name_size_and_unique_membership() {
    let result = evaluate(
        r#"
        const hash='0123456789012345678901234567890123456789';
        const target={infoHash:hash,fileName:'folder/Example.mkv',sizeBytes:42};
        const file={id:123,filename:target.fileName,size:42};
        const torrent={info_hash:hash,files:[file,{id:124,filename:'other.mkv',size:99}]};
        return {valid:S.animetoshoSelectedFile(torrent,target).id,
          wrongHash:S.animetoshoSelectedFile({...torrent,info_hash:'ffffffffffffffffffffffffffffffffffffffff'},target),
          basename:S.animetoshoSelectedFile(torrent,{...target,fileName:'Example.mkv'}).id,
          wrongSize:S.animetoshoSelectedFile(torrent,{...target,sizeBytes:43}),
          duplicate:S.animetoshoSelectedFile({...torrent,files:[file,{...file,id:124}]},target),
          foreignId:S.animetoshoId('https://example.org/view/a123'),officialId:S.animetoshoId('https://animetosho.org/view/a123')};
    "#,
    );
    assert_eq!(result["valid"], 123);
    assert_eq!(result["officialId"], 123);
    assert_eq!(result["basename"], 123);
    for key in ["wrongHash", "wrongSize", "duplicate", "foreignId"] {
        assert!(result[key].is_null(), "{key}: {result}");
    }
}

#[test]
fn probe_must_belong_to_the_selected_torrent_and_file() {
    let fixture = include_str!("../test-fixtures/addons/supernova/fixtures/hokuto-tracks.json");
    let result = evaluate(&format!(
        "const file={fixture};const target={{fileName:file.filename,sizeBytes:file.size,fileId:file.id}};return [S.animetoshoTracks({{...file,id:1}},target,file.torrent_id),S.animetoshoTracks(file,target,1),S.animetoshoTracks(file,{{...target,sizeBytes:1}},file.torrent_id),S.animetoshoTracks(file,{{...target,fileName:'other.mkv'}},file.torrent_id)];"
    ));
    assert_eq!(result, json!([null, null, null, null]));
}

#[test]
fn source_probes_reuse_cache_and_respect_spare_request_budget() {
    let fixture = include_str!("../test-fixtures/addons/supernova/fixtures/hokuto-tracks.json");
    let result = evaluate(&format!(
        r#"
        const file={fixture};
        const record={{infoHash:'0123456789012345678901234567890123456789',animetoshoId:file.torrent_id,fileName:'folder/'+file.filename,sizeBytes:file.size,fileIdx:0}};
        const cache=new Map(),urls=[];let upgrades=0;
        S.resultCacheGet=key=>cache.get(key);
        S.resultCacheSet=(key,value)=>cache.set(key,value);
        S.batch=(requests,deadline,callback)=>requests.map((request,index)=>{{
          urls.push(request.url);
          const body=request.url.includes('show=torrent') ? {{info_hash:record.infoHash,files:[{{id:file.id,filename:file.filename,size:file.size}}]}} : file;
          const response={{status:200,body:JSON.stringify(body)}};
          if(callback) callback(index,response);return response;
        }});
        const skipped=S.enrichTrackMetadata([record],new Map(),Date.now()+3000,1,()=>upgrades++);
        const first=S.enrichTrackMetadata([record],new Map(),Date.now()+3000,6,()=>upgrades++);
        const second=S.enrichTrackMetadata([record],new Map(),Date.now()+3000,0,()=>upgrades++);
        return {{skipped,first,second,urls,upgrades,metadata:record.trackMetadata}};
    "#
    ));
    assert_eq!(result["skipped"], 0);
    assert_eq!(result["first"], 2);
    assert_eq!(result["second"], 0);
    assert_eq!(result["upgrades"], 2);
    assert_eq!(
        result["urls"],
        json!([
            "https://animetosho.org/feed/json?show=torrent&id=764685",
            "https://animetosho.org/feed/json?show=file&id=1377287"
        ])
    );
    assert_eq!(result["metadata"]["audioLanguages"], json!(["ja", "en"]));
    assert!(
        result["metadata"]["fileName"]
            .as_str()
            .unwrap()
            .starts_with("folder/")
    );
}
