slint::include_modules!();

/// The consumer's model must use the library's actual generated row type.
pub fn set_addons(app: &SettingsProbe, rows: slint::ModelRc<nova_settings_module_probe::AddonRow>) {
    app.set_addons(rows);
}

/// Also exercise the imported global's Rust API rather than only its markup.
pub fn apply_palette(app: &SettingsProbe, palette: nova_settings_module_probe::ThemePalette) {
    use slint::ComponentHandle;
    app.global::<nova_settings_module_probe::Theme>()
        .set_current(palette);
}
