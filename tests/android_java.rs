//! The download service's Java source is rendered from nova-android's template.
//! The committed copy is what cargo-apk2 compiles, so it must stay equal to the
//! rendering. Set `NOVA_UPDATE_JAVA=1` to rewrite it after changing the names.
use nova_android::java::Names;
use std::path::Path;

const NAMES: Names = Names {
    package: "dev.misob.nova",
    prefix: "Nova",
    channel_id: "nova.background",
    wake_lock_tag: "nova:downloads",
};

#[test]
fn background_service_matches_the_shared_template() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("android/java/dev/misob/nova")
        .join(NAMES.background_service_file());
    let rendered = NAMES.background_service();
    if std::env::var_os("NOVA_UPDATE_JAVA").is_some() {
        std::fs::write(&path, &rendered).unwrap();
    }
    let committed = std::fs::read_to_string(&path).unwrap();
    assert!(
        committed == rendered,
        "{} differs from the shared template; run with NOVA_UPDATE_JAVA=1",
        path.display()
    );
}
