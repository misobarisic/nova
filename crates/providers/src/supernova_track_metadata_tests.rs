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
fn probe_resolution_uses_exact_valid_video_heights_and_keeps_conflicts_unknown() {
    let result = evaluate(
        r#"
        const target={fileName:'Example.mkv',sizeBytes:42,fileId:123};
        const video=height=>({codec_type:'video',codec_name:'hevc',height});
        const probe=streams=>S.animetoshoTracks({id:123,filename:target.fileName,size:42,torrent_id:7,info:{ffprobe:{streams}}},target,7);
        const base={title:'Example 1080p',infoHash:'a'.repeat(40),sources:['Nyaa'],fileIdx:0,fileName:target.fileName,sizeBytes:42,seeders:1,sourceMetadata:{resolution:720}};
        const inspected=streams=>{const metadata=probe(streams),record={...base,trackMetadata:metadata};return {metadata,fact:S.recordFacts(record).height,row:S.row(record,false).streamFacts.height};};
        const good=[144,240,800,8640].map(height=>inspected([video(height),{...video(1080),disposition:{attached_pic:1}}]));
        const bad=[143,8641,800.5,'800',null,{},[],true].map(height=>inspected([video(height)]));
        const missing=inspected([{codec_type:'video',codec_name:'hevc'}]);
        const conflict=inspected([video(240),video(800)]),partial=inspected([video(800),{codec_type:'video',codec_name:'hevc'}]);
        const duplicate=inspected([video(800),video(800)]);
        const metadata=probe([video(800)]);
        return {good,bad,missing,conflict,partial,duplicate,invalidStored:[143,8641,800.5,'800'].map(resolution=>S.validTrackMetadata({...metadata,resolution})),oldStored:S.validTrackMetadata((({resolution,...rest})=>rest)(metadata))};
        "#,
    );
    for (index, height) in [144, 240, 800, 8640].into_iter().enumerate() {
        let row = &result["good"][index];
        assert_eq!(row["metadata"]["resolution"], height);
        assert_eq!(row["fact"], height);
        assert_eq!(row["row"], height);
    }
    for row in result["bad"]
        .as_array()
        .unwrap()
        .iter()
        .chain([&result["conflict"], &result["partial"]])
    {
        assert!(row["metadata"]["resolution"].is_null());
        assert!(row["fact"].is_null());
        assert!(row["row"].is_null());
    }
    assert!(result["missing"]["metadata"].get("resolution").is_none());
    assert_eq!(
        result["missing"]["fact"], 720,
        "missing probe heights retain release hints"
    );
    assert_eq!(result["duplicate"]["fact"], 800);
    assert_eq!(result["invalidStored"], json!([false, false, false, false]));
    assert_eq!(result["oldStored"], true);
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
