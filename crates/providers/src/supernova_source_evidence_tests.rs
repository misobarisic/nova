//! Source evidence is normalized locally; these fixtures never use the network.
use crate::{MemoryProviderHost, PluginManifest, PluginRuntime};
use serde_json::{Value, json};

fn evaluate(script: &str) -> Value {
    let definition = crate::bundled_providers()
        .iter()
        .find(|provider| provider.url == crate::SUPERNOVA_PROVIDER_URL)
        .unwrap();
    let manifest: PluginManifest = serde_json::from_str(definition.plugin_manifest).unwrap();
    let source = format!(
        "{}\nglobalThis.novaProvider={{handle(){{const S=globalThis.Supernova;{script}}}}};",
        definition.source
    );
    PluginRuntime::new(manifest, source)
        .invoke(MemoryProviderHost::new(), &json!({"op":"streams"}))
        .unwrap()
}

#[test]
fn source_descriptions_propagate_normalized_evidence_without_raw_text() {
    let result = evaluate(
        r#"
        const hash='0123456789012345678901234567890123456789';
        const description='Audio: Japanese, English\nSubtitles: French\nVideo: HEVC 1080p\nhttps://example.org/private?token=secret';
        return [S.sources.apibay.parse(JSON.stringify([{id:'123',name:'Example',info_hash:hash,descr:description}]))[0],
          S.sources.knaben.parse(JSON.stringify({hits:[{title:'Example',hash,description}]}))[0],
          S.sources.animetosho.parse(JSON.stringify([{id:5,title:'Example',info_hash:hash,description}]))[0]];
        "#,
    );
    for row in result.as_array().unwrap() {
        assert_eq!(row["sourceMetadata"]["audioLanguages"], json!(["ja", "en"]));
        assert_eq!(row["sourceMetadata"]["subtitleLanguages"], json!(["fr"]));
        assert_eq!(row["sourceMetadata"]["videoCodec"], "hevc");
        assert_eq!(row["sourceMetadata"]["resolution"], 1080);
        assert!(!row.to_string().contains("token=secret"));
        assert!(row.get("description").is_none());
    }
}

#[test]
fn apibay_detail_requires_valid_source_id_and_matching_response_identity() {
    let result = evaluate(
        r#"
        const hash='0123456789012345678901234567890123456789',descriptor={source:'apibay',id:123};
        const row={id:'123',info_hash:hash,descr:'Audio: Japanese\nSubtitles: English'};
        const detail=value=>S.sources.apibay.detailEvidence(JSON.stringify(value),descriptor,hash);
        return {valid:detail(row),wrongHash:detail({...row,info_hash:'f'.repeat(40)}),
          wrongId:detail({...row,id:'124'}),missingId:detail({info_hash:hash,descr:row.descr}),
          request:S.sources.apibay.detailRequest(descriptor),
          injected:S.sources.apibay.detailRequest({source:'apibay',id:'123&url=https://example.org'}),
          foreign:S.sources.apibay.detailRequest({source:'nyaa',id:123})};
        "#,
    );
    assert_eq!(result["valid"]["audioLanguages"], json!(["ja"]));
    assert_eq!(result["request"]["url"], "https://apibay.org/t.php?id=123");
    for field in ["wrongHash", "wrongId", "missingId", "injected", "foreign"] {
        assert!(result[field].is_null(), "{field}: {result}");
    }
}

#[test]
fn animetosho_uses_native_nyaa_reference_and_ignores_external_article_text() {
    let result = evaluate(
        r#"
        const row={id:5,title:'Example',info_hash:'0123456789012345678901234567890123456789',
          nyaa_id:123,article_title:'Audio: French',website_url:'https://example.org/description'};
        return [row,{...row,nyaa_subdom:'sukebei'},{...row,nyaa_id:'123?x=1'}]
          .map(value=>S.sources.animetosho.parse(JSON.stringify([value]))[0]);
        "#,
    );
    assert_eq!(result[0]["details"], json!([{"source":"nyaa","id":123}]));
    assert_eq!(result[1]["details"], json!([]));
    assert_eq!(result[2]["details"], json!([]));
    for row in result.as_array().unwrap() {
        assert!(row["sourceMetadata"].is_null());
    }
}

#[test]
fn stremthru_media_attributes_preserve_track_roles_and_description() {
    let result = evaluate(
        r#"
        const feed='<rss xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel><item>'+
          '<title>Example</title><description>Subtitles: French</description>'+
          '<torznab:attr name="infohash" value="0123456789012345678901234567890123456789"/>'+
          '<torznab:attr name="audio" value="AAC 2.0"/><torznab:attr name="video" value="HEVC"/>'+
          '<torznab:attr name="resolution" value="1080"/><torznab:attr name="language" value="English"/>'+
          '</item></channel></rss>';
        return S.sources.stremthru.parse(feed)[0].sourceMetadata;
        "#,
    );
    assert_eq!(result["subtitleLanguages"], json!(["fr"]));
    assert_eq!(result["audioCodecs"], json!(["AAC"]));
    assert_eq!(result["audioChannels"], json!(["2.0"]));
    assert_eq!(result["videoCodec"], "hevc");
    assert_eq!(result["resolution"], 1080);
    assert!(result.get("audioLanguages").is_none());
}

#[test]
fn conflicting_optional_attributes_do_not_discard_playable_source_rows() {
    let result = evaluate(
        r#"
      const feed='<rss xmlns:torznab="http://torznab.com/schemas/2015/feed"><channel><item><title>Example</title>'+
        '<torznab:attr name="infohash" value="0123456789012345678901234567890123456789"/>'+
        '<torznab:attr name="video" value="HEVC"/><torznab:attr name="video" value="AVC"/>'+
        '<torznab:attr name="resolution" value="720"/><torznab:attr name="resolution" value="1080"/>'+
        '</item></channel></rss>';return S.sources.stremthru.parse(feed);
    "#,
    );
    assert_eq!(result.as_array().unwrap().len(), 1);
    assert!(result[0]["sourceMetadata"]["videoCodec"].is_null());
    assert!(result[0]["sourceMetadata"]["resolution"].is_null());
}
