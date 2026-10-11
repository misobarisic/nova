slint::include_modules!();

/// Compile-only demonstration of keeping the page's generated visual tree in
/// its own crate. Nova also needs settings, viewport, global and focus wiring.
pub fn settings_factory() -> slint::ComponentFactory {
    slint::ComponentFactory::new(|_context| {
        let page = nova_settings_module_probe::ProbeSettingsPage::new().ok()?;
        page.set_external_navigation(true);
        Some(page)
    })
}

/// Exercise the generated host setter as well as the factory's return type.
pub fn install(app: &SettingsContainerProbe) {
    app.set_page_factory(settings_factory());
}
