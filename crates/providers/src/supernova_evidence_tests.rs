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
fn unsupported_codecs_do_not_turn_mixed_claims_into_a_preferred_codec() {
    let result = evaluate(
        r#"return ['Video: VP9 / AV1','Video: VP9','Video: MPEG-4 AVC'].map(S.releaseEvidence);"#,
    );
    assert!(result[0]["videoCodec"].is_null());
    assert!(result[1]["videoCodec"].is_null());
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
        r#"return {accepted:S.validReleaseEvidence({videoCodec:null,resolution:null,releaseSource:null,audioLanguages:['en'],subtitleLanguages:[]}),rejected:[{rawDescription:'Audio: English'},{audioLanguages:['eng']},{videoCodec:'vp9'},{audioLanguages:['en','en']},{audioCodecs:['AAC'.repeat(100)]},{resolution:99999},{audioChannels:['1'.repeat(100)]},{subtitleLanguages:Array(33).fill('en')}].map(S.validReleaseEvidence),oversized:S.releaseEvidence('Audio: English\n'+'x'.repeat(65536)),merged:S.mergeReleaseEvidence({audioLanguages:['ja'],subtitleLanguages:['fr']},{audioLanguages:['en','ja'],subtitleLanguages:['en']})};"#,
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
