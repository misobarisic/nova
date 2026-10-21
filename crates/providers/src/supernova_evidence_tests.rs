//! Description evidence stays separate from filename hints and verified tracks.
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
fn explicit_resolution_evidence_preserves_exact_heights_and_rejects_ambiguous_tokens() {
    let result = evaluate(
        r#"
        const valid=[144,240,800,8640],invalid=[0,143,8641,65535,800.5,'800',null,{},[],true];
        const record=height=>({title:'Example',fileName:'Example.mkv',fileIdx:0,sizeBytes:1,seeders:1,sources:['Nyaa'],infoHash:'a'.repeat(40),sourceMetadata:{resolution:height}});
        const good=['Example.240p.AVC','[Group] Example 8640p','Example_800p_AVC','Example 800p 800p','Example 4k','Example 8k','Example.2026.1080p.WEB-DL','Example.2026.240p.AVC'];
        const bad=['Example 143p','Example 8641p','Example 0240p','Example 123240p','Example height240p','Example 240pixels','Example 240p50','Example 240.5p','Example 2.400p','Example 240p 8640p','Example 2160p 8k','Example 800','Example high quality'];
        return {valid:valid.map(height=>S.validReleaseEvidence({resolution:height})),invalid:invalid.map(height=>S.validReleaseEvidence({resolution:height})),
          facts:valid.map(height=>S.recordFacts(record(height)).height),rows:valid.map(height=>S.row(record(height),false).streamFacts.height),
          good:good.map(S.resolution),bad:bad.map(S.resolution),
          descriptions:['Resolution: 240p','Resolution: 8640p','Video: 1280x800','Video\nHeight: 800 pixels','Video\nHeight: 8 640 pixels','800p AVC'].map(text=>S.releaseEvidence(text)?.resolution ?? null),
          prose:['This story has 240p fans','The upload is 800 pixels tall','Video\nHeight: 800 adventures','Resolution: height240p','Resolution: 240p50','Resolution: 2.400p','Resolution: 143p','Resolution: 8641p','Resolution: 800p 1080p'].map(text=>S.releaseEvidence(text)?.resolution ?? null)};
        "#,
    );
    assert_eq!(result["valid"], json!([true, true, true, true]));
    // Explicit null remains the normalized representation of conflicting facts.
    assert_eq!(
        result["invalid"],
        json!([
            false, false, false, false, false, false, true, false, false, false
        ])
    );
    assert_eq!(result["facts"], json!([144, 240, 800, 8640]));
    assert_eq!(result["rows"], result["facts"]);
    assert_eq!(
        result["good"],
        json!([240, 8640, 800, 800, 2160, 4320, 1080, 240])
    );
    assert!(result["bad"].as_array().unwrap().iter().all(Value::is_null));
    assert_eq!(
        result["descriptions"],
        json!([240, 8640, 800, 800, 8640, 800])
    );
    assert!(
        result["prose"]
            .as_array()
            .unwrap()
            .iter()
            .all(Value::is_null)
    );
}

#[test]
fn legacy_discovery_ranking_does_not_extract_unused_native_policy_facts() {
    let result = evaluate(
        r#"
        const unexpected=()=>{throw new Error('legacy ranking requested native policy facts');};
        S.recordFacts=unexpected;S.rowFacts=unexpected;S.acceptPreferences=unexpected;S.preferenceComparator=unexpected;S.sortByPreferences=unexpected;
        const settings={"quality.preferred":"any"};
        const records=Array.from({length:300},(_,index)=>({infoHash:String(index).padStart(40,'0'),title:'Example S01 Complete 1080p',fileIdx:null,sizeBytes:100,seeders:index}));
        const ranked=S.rank(records,settings);
        const rows=S.sortRows([{id:'a',title:'Example 1080p',description:'📁 Example.mkv',size:100,seeders:1},{id:'b',title:'Example 1080p',description:'📁 Example.mkv',size:100,seeders:2}],settings);
        return {first:ranked[0].seeders,count:ranked.length,rows:rows.map(row=>row.id)};
    "#,
    );
    assert_eq!(result, json!({"first":299,"count":300,"rows":["b","a"]}));
}

#[test]
fn native_record_facts_reuse_unchanged_rows_and_refresh_after_proof_and_evidence_updates() {
    let result = evaluate(
        r#"
        let parses=0;const languages=S.releaseLanguages;
        S.releaseLanguages=text=>{parses++;return languages(text);};
        const record={title:'Example 1080p HEVC Japanese',fileIdx:null,sizeBytes:1000,seeders:5};
        const initial=S.recordFacts(record),firstParses=parses;
        const repeated=S.recordFacts(record);
        const reused=initial===repeated && parses===firstParses;
        record.seeders=10;const peers=S.recordFacts(record);
        record.fileIdx=0;record.fileName='Example 720p AVC English.mkv';record.sizeBytes=400;
        const proof=S.recordFacts(record);
        record.sourceMetadata=S.releaseEvidence('Video: AVC\nResolution: 720p\nAudio: English\nSubtitles: Croatian');
        const evidence=S.recordFacts(record);
        record.sourceMetadata=S.releaseEvidence('Video: HEVC\nResolution: 1080p\nAudio: Japanese\nSubtitles: English');
        const replacement=S.recordFacts(record);
        record.trackMetadata={source:'AnimeTosho',fileName:record.fileName,fileSize:400,videoCodec:'av1',audioLanguages:['hr'],subtitleLanguages:['ja'],audioCodecs:[],audioChannels:[]};
        const probe=S.recordFacts(record);
        record.fileName='Other selected file.mkv';record.sizeBytes=450;
        const changedFile=S.recordFacts(record);
        record.title='Example Season Pack 2160p';record.sourceMetadata=null;record.trackMetadata=null;
        const changedTitle=S.recordFacts(record);
        return {reused,initial,peers,proof,evidence,replacement,probe,changedFile,changedTitle};
    "#,
    );
    assert_eq!(
        result["reused"], true,
        "unchanged ranking waves reuse parsing"
    );
    assert!(result["initial"]["bytes"].is_null());
    assert_eq!(result["initial"]["audio_languages"], json!(["ja"]));
    assert_eq!(result["peers"]["seeders"], 10);
    assert_eq!(result["proof"]["bytes"], 400);
    assert!(
        result["proof"]["codec"].is_null(),
        "conflicting release/file codec labels remain unknown"
    );
    assert_eq!(result["evidence"]["codec"], "avc");
    assert_eq!(result["evidence"]["subtitle_languages"], json!(["hr"]));
    assert_eq!(result["replacement"]["codec"], "hevc");
    assert_eq!(result["replacement"]["audio_languages"], json!(["ja"]));
    assert_eq!(result["probe"]["codec"], "av1");
    assert_eq!(result["probe"]["audio_languages"], json!(["hr"]));
    assert_eq!(
        result["changedFile"]["codec"], "hevc",
        "old selected-file probe cannot follow a new file identity/size"
    );
    assert_eq!(result["changedFile"]["bytes"], 450);
    assert_eq!(result["changedTitle"]["season_pack"], true);
    assert_eq!(result["changedTitle"]["height"], 2160);
}

#[test]
fn native_batch_sort_preserves_exact_u64_order_and_rejects_malformed_or_unbounded_input() {
    use nova_config::stream_filters::{SortCriterion, SortDirection, StreamFilterRules};
    let manifest: PluginManifest = serde_json::from_value(json!({"formatVersion":1,"id":"sort-fixture","name":"Fixture","version":"1","entry":"fixture.js","permissions":{}})).unwrap();
    let runtime = PluginRuntime::new(
        manifest,
        r#"globalThis.novaProvider={handle(request){return JSON.parse(nova.streamPreferences.sort(request.oversized?' '.repeat(2097153):request.payload));}}"#,
    );
    let mut rules = StreamFilterRules {
        enabled: true,
        ..Default::default()
    };
    let index = rules
        .sort_order
        .iter()
        .position(|rule| rule.criterion == SortCriterion::Size)
        .unwrap();
    let size = rules.sort_order.remove(index);
    rules.sort_order.insert(0, size);
    let run = |rules: &StreamFilterRules, payload: &str| {
        runtime
            .invoke(
                MemoryProviderHost::new(),
                &json!({"streamPreferences":rules,"payload":payload}),
            )
            .unwrap()
    };
    let rows = json!([
        {"id":"a","facts":{"bytes":u64::MAX}},
        {"id":"b","facts":{"bytes":u64::MAX-1}},
        {"id":"c","facts":{"bytes":null}},
        {"id":"z","facts":{"bytes":u64::MAX-1}},
    ])
    .to_string();
    assert_eq!(
        run(&rules, &rows),
        json!([1, 3, 0, 2]),
        "facts stay in Rust: no JS floating-point rounding at u64 limits"
    );
    rules.sort_order[0].direction = SortDirection::Desc;
    assert_eq!(
        run(&rules, &rows),
        json!([0, 1, 3, 2]),
        "unknown values stay last in reverse direction too"
    );
    assert_eq!(run(&rules, "[]"), json!([]));
    for invalid in [
        "[".to_owned(),
        json!([{"id":"","facts":{}}]).to_string(),
        json!([{"id":"x".repeat(8193),"facts":{}}]).to_string(),
        json!([{"id":"x","facts":{"height":20}}]).to_string(),
        json!([{"id":"x","facts":{"codec":"unsupported-value"}}]).to_string(),
        json!([{"id":"x","facts":{"audio_languages":["https://example.com/private"]}}]).to_string(),
        json!([{"id":"x","facts":{"subtitle_languages":vec!["en";33]}}]).to_string(),
        serde_json::to_string(&vec![json!({"id":"x","facts":{}}); 4097]).unwrap(),
    ] {
        assert!(
            run(&rules, &invalid)["error"].is_string(),
            "malformed or unbounded sort input must be rejected"
        );
    }
    assert!(
        runtime
            .invoke(
                MemoryProviderHost::new(),
                &json!({"streamPreferences":rules,"oversized":true})
            )
            .unwrap()["error"]
            .is_string()
    );
}

#[test]
fn shared_policy_filters_upstream_candidates_and_rows_and_ranks_audio_before_codec() {
    use nova_config::stream_filters::{StreamFilterRules, VideoCodec};
    let definition = crate::bundled_providers()
        .iter()
        .find(|provider| provider.url == crate::SUPERNOVA_PROVIDER_URL)
        .unwrap();
    let manifest: PluginManifest = serde_json::from_str(definition.plugin_manifest).unwrap();
    let source = format!(
        "{}\n{}",
        definition.source,
        r#"
        globalThis.novaProvider={handle(request){
            const S=globalThis.Supernova;
            const original={"quality.preferred":"2160p","language.prefs":["en"],"codec.prefs":["hevc"]};
            const settings=S.withStreamPreferences(original,request.streamPreferences);
            const make=(id,codec,audio,height=720,bytes=100)=>({infoHash:id.repeat(40),title:'Example',sources:['nyaa'],fileIdx:0,fileName:id+'.mkv',sizeBytes:bytes,seeders:1,sourceMetadata:{videoCodec:codec,audioLanguages:audio,resolution:height}});
            const records=[make('a','avc',['ja']),make('b','hevc',['en']),make('c','avc',['en']),make('d','vp9',['ja']),make('e',null,[]),make('f','avc',['ja'],2160),make('g','avc',['ja'],720,2000)];
            const candidates=S.rank(records,settings);
            const rows=S.sortRows(records.map(record=>S.row(record,false)),settings);
            return {candidates:candidates.map(record=>record.infoHash[0]),rows:rows.map(row=>row.id[0]),original,unknownProvisional:S.acceptPreferences(S.recordFacts(records[4]),settings,false),unknownFinal:S.acceptPreferences(S.rowFacts(S.row(records[4],false)),settings,true)};
        }};
    "#
    );
    let runtime = PluginRuntime::new(manifest, source);
    let mut rules = StreamFilterRules {
        enabled: true,
        max_height: Some(1080),
        max_bytes: Some(1000),
        video_codecs: vec![VideoCodec::Avc, VideoCodec::Hevc, VideoCodec::Vp9],
        excluded_video_codecs: vec![VideoCodec::Vp9],
        audio_languages: vec!["ja".into(), "en".into()],
        codec_priority: vec![VideoCodec::Hevc, VideoCodec::Avc],
        retain_unknown: false,
        ..Default::default()
    };
    let run = |rules: &StreamFilterRules| {
        runtime
            .invoke(
                MemoryProviderHost::new(),
                &json!({"op":"streams","streamPreferences":rules}),
            )
            .unwrap()
    };
    let soft = run(&rules);
    assert_eq!(soft["rows"], json!(["a", "b", "c"]));
    assert!(
        soft["candidates"].as_array().unwrap().contains(&json!("e")),
        "unknown candidates reach inspection"
    );
    assert_eq!(soft["unknownProvisional"], true);
    assert_eq!(soft["unknownFinal"], false);
    assert_eq!(
        soft["original"]["quality.preferred"], "2160p",
        "ephemeral overlay leaves plugin settings intact"
    );
    rules.require_audio_language = true;
    rules.audio_languages = vec!["ja".into()];
    assert_eq!(run(&rules)["rows"], json!(["a"]));
    rules.require_audio_language = false;
    rules.require_preferred_codec = true;
    rules.codec_priority = vec![VideoCodec::Hevc];
    assert_eq!(run(&rules)["rows"], json!(["b"]));
    rules.excluded_video_codecs.push(VideoCodec::Hevc);
    assert_eq!(
        run(&rules)["rows"],
        json!([]),
        "explicit block wins over codec preference"
    );
    rules.enabled = false;
    let disabled = run(&rules);
    assert_eq!(
        disabled["rows"].as_array().unwrap().len(),
        7,
        "disabled policy has no hidden legacy filters"
    );
    assert_eq!(disabled["unknownFinal"], true);
}

#[test]
fn shared_resolution_policy_reaches_candidate_caps_batch_sort_and_final_rows() {
    use nova_config::stream_filters::{
        SortCriterion, SortDirection, StreamFilterRules, StreamSortRule,
    };
    let definition = crate::bundled_providers()
        .iter()
        .find(|provider| provider.url == crate::SUPERNOVA_PROVIDER_URL)
        .unwrap();
    let manifest: PluginManifest = serde_json::from_str(definition.plugin_manifest).unwrap();
    let source = format!(
        "{}\n{}",
        definition.source,
        r#"
        globalThis.novaProvider={handle(request){
            const S=globalThis.Supernova;
            const settings=S.withStreamPreferences({"quality.preferred":"2160p"},request.streamPreferences);
            if(request.fallback) nova.streamPreferences.sort=undefined;
            const make=(id,height)=>({infoHash:id.repeat(40),title:'Example',sources:['nyaa'],fileIdx:0,fileName:id+'.mkv',sizeBytes:100,seeders:1,sourceMetadata:{resolution:height}});
            const records=[make('a',720),make('b',1080),make('c',2160),make('d',800),make('e',null),make('f',4320),make('g',576)];
            const candidates=S.rank(records,settings);
            const rows=S.sortRows(records.map(record=>S.row(record,false)),settings);
            S.CANDIDATE_MAX=1; S.MAX=1;
            return {candidates:candidates.map(record=>record.infoHash[0]),rows:rows.map(row=>row.id[0]),pool:candidates.slice(0,S.CANDIDATE_MAX).map(record=>record.infoHash[0]),visible:rows.slice(0,S.MAX).map(row=>row.id[0])};
        }};
        "#
    );
    let runtime = PluginRuntime::new(manifest, source);
    let mut rules = StreamFilterRules {
        enabled: true,
        max_height: Some(2160),
        excluded_resolutions: vec![720],
        resolution_priority: vec![1080, 720],
        retain_unknown: false,
        sort_order: vec![StreamSortRule {
            criterion: SortCriterion::Resolution,
            direction: SortDirection::Desc,
        }],
        ..Default::default()
    };
    let run = |rules: &StreamFilterRules, fallback| {
        runtime
            .invoke(
                MemoryProviderHost::new(),
                &json!({
                    "op":"streams", "streamPreferences":rules, "fallback":fallback
                }),
            )
            .unwrap()
    };
    for fallback in [false, true] {
        let soft = run(&rules, fallback);
        assert_eq!(soft["candidates"], json!(["b", "c", "d", "g", "e"]));
        assert_eq!(soft["rows"], json!(["b", "c", "d", "g"]));
        assert_eq!(
            soft["pool"],
            json!(["b"]),
            "preference must apply before inspection caps"
        );
        assert_eq!(soft["visible"], json!(["b"]));
    }
    rules.sort_order[0].direction = SortDirection::Asc;
    let ascending = run(&rules, false);
    assert_eq!(ascending["rows"], json!(["b", "g", "d", "c"]));
    assert_eq!(
        ascending["pool"],
        json!(["b"]),
        "direction cannot reverse preferences"
    );
    rules.require_preferred_resolution = true;
    let strict = run(&rules, false);
    assert_eq!(strict["rows"], json!(["b"]));
    assert_eq!(
        strict["candidates"],
        json!(["b", "e"]),
        "unknown candidates may acquire file facts"
    );
    rules.retain_unknown = true;
    assert_eq!(run(&rules, false)["rows"], json!(["b", "e"]));
    rules.retain_unknown = false;
    rules.excluded_resolutions.push(1080);
    assert_eq!(
        run(&rules, false)["rows"],
        json!([]),
        "exclusions override strict preferences"
    );
    rules.resolution_priority.clear();
    assert_eq!(
        run(&rules, false)["rows"],
        json!(["g", "d", "c"]),
        "empty preferred list is unrestricted"
    );
    rules.enabled = false;
    assert_eq!(run(&rules, false)["rows"].as_array().unwrap().len(), 7);
}

#[test]
fn shared_full_sort_reorders_every_criterion_without_hidden_addon_tiebreaks() {
    use nova_config::stream_filters::{
        SortCriterion, SortDirection, StreamFacts, StreamFilterRules, VideoCodec,
    };
    let definition = crate::bundled_providers()
        .iter()
        .find(|provider| provider.url == crate::SUPERNOVA_PROVIDER_URL)
        .unwrap();
    let manifest: PluginManifest = serde_json::from_str(definition.plugin_manifest).unwrap();
    let source = format!(
        "{}\n{}",
        definition.source,
        r#"
        globalThis.novaProvider={handle(request){
            const S=globalThis.Supernova;
            const settings=S.withStreamPreferences({"quality.preferred":"2160p","language.prefs":["fr"],"codec.prefs":["vp9"],"subtitle.prefs":["ja"],"sort.order":S.SORT_DEFAULT.map(item=>({...item,direction:'asc'}))},request.streamPreferences);
            const make=(id,codec,audio,subtitles,height,bytes,seeders,pack)=>({infoHash:id.repeat(40),title:pack?'Example Season Pack':'Example',sources:['nyaa'],fileIdx:0,fileName:id+'.mkv',sizeBytes:bytes,seeders,sourceMetadata:{videoCodec:codec,audioLanguages:audio,subtitleLanguages:subtitles,resolution:height}});
            const records=[make('a','avc',['ja'],['fr'],720,600,5,false),make('b','hevc',['en'],['ja'],1080,300,50,true),make('c','avc',['en'],['en'],1080,300,null,false)];
            const rows=records.map((record,index)=>S.row(record,request.cached && index===1,JSON.stringify({fileName:record.fileName})));
            const sorted=S.sortRows(rows,settings);
            const unproved={...records[1],fileIdx:null};
            return {rows:sorted.map(row=>row.id[0]),candidates:S.rank(records,settings).map(record=>record.infoHash[0]),facts:rows.map(row=>row.streamFacts),provisional:S.recordFacts(unproved),copied:S.rowFacts(S.copyRow(rows[0]))};
        }};
    "#
    );
    let runtime = PluginRuntime::new(manifest, source);
    let rules = StreamFilterRules {
        enabled: true,
        audio_languages: vec!["ja".into(), "en".into()],
        codec_priority: vec![VideoCodec::Hevc, VideoCodec::Avc],
        subtitle_languages: vec!["en".into(), "fr".into()],
        ..Default::default()
    };
    let run = |rules: &StreamFilterRules, cached: bool| {
        runtime
            .invoke(
                MemoryProviderHost::new(),
                &json!({"op":"streams","streamPreferences":rules,"cached":cached}),
            )
            .unwrap()
    };
    let ordinary = run(&rules, false);
    assert_eq!(ordinary["rows"], json!(["a", "b", "c"]));
    assert_eq!(ordinary["candidates"], ordinary["rows"]);
    let promoted = |criterion, direction| {
        let mut next = rules.clone();
        let index = next
            .sort_order
            .iter()
            .position(|rule| rule.criterion == criterion)
            .unwrap();
        let mut rule = next.sort_order.remove(index);
        rule.direction = direction;
        next.sort_order.insert(0, rule);
        next
    };
    use SortCriterion::*;
    use SortDirection::*;
    for (criterion, direction, expected) in [
        (Seeders, Desc, json!(["b", "a", "c"])),
        (Seeders, Asc, json!(["a", "b", "c"])),
        (Resolution, Desc, json!(["b", "c", "a"])),
        (Size, Asc, json!(["b", "c", "a"])),
        (SeasonPack, Desc, json!(["b", "a", "c"])),
        (Audio, Desc, json!(["b", "c", "a"])),
        (Codec, Desc, json!(["a", "c", "b"])),
        (Subtitle, Asc, json!(["c", "a", "b"])),
        (Subtitle, Desc, json!(["a", "c", "b"])),
    ] {
        let result = run(&promoted(criterion, direction), false);
        assert_eq!(result["rows"], expected, "{criterion:?} {direction:?}");
        assert_eq!(
            result["candidates"], expected,
            "candidate {criterion:?} {direction:?}"
        );
    }
    let cached = run(&promoted(Cached, Desc), true);
    assert_eq!(cached["rows"], json!(["b", "a", "c"]));
    assert_eq!(cached["facts"][1]["cached"], true);
    assert!(
        cached["facts"][1]["seeders"].is_null(),
        "cached availability is not a peer count"
    );
    assert!(ordinary["provisional"]["cached"].is_null());
    assert!(
        ordinary["provisional"]["bytes"].is_null(),
        "unproved episode bytes cannot be a pack total"
    );
    assert_eq!(ordinary["copied"], ordinary["facts"][0]);
    let a: StreamFacts = serde_json::from_value(ordinary["facts"][0].clone()).unwrap();
    let b: StreamFacts = serde_json::from_value(ordinary["facts"][1].clone()).unwrap();
    assert_eq!(a.subtitle_languages, ["fr"]);
    assert_eq!(b.season_pack, Some(true));
    assert!(
        rules.compare(&a, &b).is_lt(),
        "app receives exactly the evidence compared upstream"
    );
    // Every metric ties when disabled; the source may only use stable IDs,
    // even though its obsolete plugin sort/quality fields strongly disagree.
    let mut disabled = rules;
    disabled.enabled = false;
    assert_eq!(run(&disabled, true)["rows"], json!(["a", "b", "c"]));
}

#[test]
fn markdown_track_lists_keep_judas_audio_and_subtitle_languages_separate() {
    let result = evaluate(
        r#"return S.releaseEvidence('### Release information\n**Video:** 1080p HEVC 10bit\n**Audio:** Japanese - English\n**Subtitles:** English - French\nSource: WEB-DL');"#,
    );
    assert_eq!(
        result,
        json!({"audioLanguages":["ja","en"],"subtitleLanguages":["en","fr"],"videoCodec":"hevc","resolution":1080,"releaseSource":"WEB-DL"})
    );
}

#[test]
fn mediainfo_sections_preserve_multiple_tracks_and_skip_nontrack_fields() {
    let result = evaluate(
        r#"return S.releaseEvidence(`General
Complete name : release.mkv
Video
ID : 1
Format : HEVC
Width : 1 920 pixels
Height : 1 080 pixels
Audio #1
ID : 2
Format : E-AC-3
Channel(s) : 6 channels
Language : Japanese
Audio #2
ID : 3
Format : AAC
Channel(s) : 2 channels
Language : eng
Text #1
ID : 4
Format : ASS
Language : English`);"#,
    );
    assert_eq!(result["audioLanguages"], json!(["ja", "en"]));
    assert_eq!(result["subtitleLanguages"], json!(["en"]));
    assert_eq!(result["audioCodecs"], json!(["E-AC-3", "AAC"]));
    assert_eq!(result["audioChannels"], json!(["6 channels", "2.0"]));
    assert_eq!(result["resolution"], 1080);
    assert_eq!(result["videoCodec"], "hevc");
}

#[test]
fn html_and_markdown_tables_normalize_language_aliases_and_regions() {
    let result = evaluate(
        r#"return [S.releaseEvidence('<table><tr><td>Audio</td><td>Spanish[LAT] / Portuguese[BR] / eng-US</td><td>AAC 2.0</td></tr><tr><td>Subtitles</td><td>fra / de-DE / es-419</td></tr></table>'),S.releaseEvidence('| Audio | jpn / eng |\n| Subtitles | French / Croatian |')];"#,
    );
    assert_eq!(result[0]["audioLanguages"], json!(["es", "pt", "en"]));
    assert_eq!(result[0]["subtitleLanguages"], json!(["fr", "de", "es"]));
    assert_eq!(result[0]["audioCodecs"], json!(["AAC"]));
    assert_eq!(result[0]["audioChannels"], json!(["2.0"]));
    assert_eq!(result[1]["audioLanguages"], json!(["ja", "en"]));
    assert_eq!(result[1]["subtitleLanguages"], json!(["fr", "hr"]));
}

#[test]
fn prose_links_groups_and_unnamed_multi_claims_supply_no_track_evidence() {
    let result = evaluate(
        r#"return ['A Japanese boy meets an English girl in a French village. HEVC powers their video.', '[AV1] Example 01 [1080p]', 'Audio: Multi / Dual\nSubtitles: Multi-Subs', 'Audio: [English](https://example.org)\nSubtitles: <a href="https://example.org">French</a>', 'Audio:\nThe English village is quiet.\nJapanese heroes arrive.', 'Group: AV1\nPlot: German and French friends'].map(S.releaseEvidence);"#,
    );
    assert_eq!(result, json!([null, null, null, null, null, null]));
}

#[test]
fn conflicting_scalar_evidence_stays_unknown_after_later_merges() {
    let result = evaluate(
        r#"const conflict=S.releaseEvidence('Video: AVC / HEVC\nVideo: HEVC\nResolution: 720p\nResolution: 1080p\nSource: WEB-DL\nSource: Blu-ray');return S.mergeReleaseEvidence(conflict,{videoCodec:'hevc',resolution:1080,releaseSource:'WEB-DL',audioLanguages:['en']});"#,
    );
    assert_eq!(
        result,
        json!({"videoCodec":null,"resolution":null,"releaseSource":null,"audioLanguages":["en"]})
    );
}

#[test]
fn bluray_remux_is_a_compatible_source_refinement() {
    let result = evaluate(
        r#"const evidence=S.releaseEvidence('Source: Blu-ray REMUX');return [evidence,S.mergeReleaseEvidence(evidence,{releaseSource:'Blu-ray'}),S.mergeReleaseEvidence(evidence,{releaseSource:'WEB-DL'})];"#,
    );
    assert_eq!(result[0]["releaseSource"], "Blu-ray REMUX");
    assert_eq!(result[1]["releaseSource"], "Blu-ray REMUX");
    assert!(result[2]["releaseSource"].is_null());
}

#[test]
fn explicit_vp9_is_known_but_mixed_codec_claims_stay_unknown() {
    let result = evaluate(
        r#"return ['Video: VP9 / AV1','Video: VP9','Video: MPEG-4 AVC'].map(S.releaseEvidence);"#,
    );
    assert!(result[0]["videoCodec"].is_null());
    assert_eq!(result[1]["videoCodec"], "vp9");
    assert_eq!(result[2]["videoCodec"], "avc");
}

#[test]
fn standalone_specs_and_inline_track_labels_are_explicit_evidence() {
    let result = evaluate(
        r#"return [S.releaseEvidence('1080p HEVC 10bit AAC 2.0 WEB-DL'),S.releaseEvidence('Audio: Japanese - English / Subtitles: English - French')];"#,
    );
    assert_eq!(
        result[0],
        json!({"videoCodec":"hevc","resolution":1080,"audioCodecs":["AAC"],"audioChannels":["2.0"],"releaseSource":"WEB-DL"})
    );
    assert_eq!(result[1]["audioLanguages"], json!(["ja", "en"]));
    assert_eq!(result[1]["subtitleLanguages"], json!(["en", "fr"]));
}

#[test]
fn evidence_validation_bounds_fields_and_merge_deduplicates_only_matching_domains() {
    let result = evaluate(
        r#"return {accepted:S.validReleaseEvidence({videoCodec:null,resolution:null,releaseSource:null,audioLanguages:['en'],subtitleLanguages:[]}),rejected:[{rawDescription:'Audio: English'},{audioLanguages:['eng']},{videoCodec:'future-unknown-codec'},{audioLanguages:['en','en']},{audioCodecs:['AAC'.repeat(100)]},{resolution:99999},{audioChannels:['1'.repeat(100)]},{subtitleLanguages:Array(33).fill('en')}].map(S.validReleaseEvidence),oversized:S.releaseEvidence('Audio: English\n'+'x'.repeat(65536)),merged:S.mergeReleaseEvidence({audioLanguages:['ja'],subtitleLanguages:['fr']},{audioLanguages:['en','ja'],subtitleLanguages:['en']})};"#,
    );
    assert_eq!(result["accepted"], true);
    assert_eq!(
        result["rejected"],
        json!([false, false, false, false, false, false, false, false])
    );
    assert!(result["oversized"].is_null());
    assert_eq!(
        result["merged"],
        json!({"audioLanguages":["ja","en"],"subtitleLanguages":["fr","en"]})
    );
}

#[test]
fn judas_encode_and_mixed_supported_subtitle_inventory_keep_known_facts() {
    let result = evaluate(
        r#"return S.releaseEvidence('HEVC 10bit SoftSubbed - 1920 x 1080\nAudio: Japanese - English\nSubtitles: English - French - German - Italian - Spanish[ESP] - Spanish[LAT] - Portuguese[BR] - Arabic - Simplified_CR - Traditional_CR - Japanese - HIN - Indonesian - Korean - Malay - Dutch - Polish - Thai - Turkish');"#,
    );
    assert_eq!(result["videoCodec"], "hevc");
    assert_eq!(result["resolution"], 1080);
    assert_eq!(result["audioLanguages"], json!(["ja", "en"]));
    assert_eq!(
        result["subtitleLanguages"],
        json!([
            "en", "fr", "de", "it", "es", "pt", "ar", "ja", "hi", "id", "ko", "nl", "pl", "th",
            "tr"
        ])
    );
}

#[test]
fn labelled_language_prose_and_sentence_items_are_not_inventories() {
    let result = evaluate(
        r#"return ['Audio: An English actor meets a Japanese friend', 'Subtitles: English - A French actor meets a Japanese friend - German', 'Audio: English / a story about Japanese heroes', 'Video: HardSubbed HEVC 720p'].map(S.releaseEvidence);"#,
    );
    assert!(result[0].is_null());
    assert!(result[1].is_null());
    assert!(result[2].is_null());
    assert_eq!(result[3], json!({"videoCodec":"hevc","resolution":720}));
}
