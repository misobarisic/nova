//! The real shared controls consume the current palette, including alpha and
//! gradient brushes, without remounting or changing their geometry.

use slint::{Brush, Color, ComponentHandle};

slint::slint! {
    import { Theme } from "../crates/ui/theme.slint";
    export { Theme } from "../crates/ui/theme.slint";
    import { Anim } from "../crates/ui/anim.slint";
    export { Anim } from "../crates/ui/anim.slint";
    import { PillButton, SettingsCard } from "../crates/ui/settings-controls.slint";
    import { SearchField } from "../crates/ui/searchfield.slint";
    import { SideNav } from "../crates/ui/sidenav.slint";

    export component ThemeHarness inherits Window {
        width: 480px;
        height: 360px;
        out property <brush> card_fill: card.background;
        out property <brush> card_border: card.border-color;
        out property <brush> button_fill: button.background;
        out property <brush> button_border: button.border-color;
        out property <brush> input_fill: input.background;
        out property <brush> nav_fill: nav.panel-background;
        out property <length> button_height: button.height;
        out property <length> input_height: input.height;

        card := SettingsCard { x: 80px; y: 10px; width: 280px; height: 240px; }
        button := PillButton { x: 100px; y: 30px; label: "Theme test"; primary: true; focused: true; }
        input := SearchField { x: 100px; y: 100px; width: 220px; }
        nav := SideNav { viewport-width: root.width; }
    }
}

#[test]
fn existing_theme_defaults_and_live_palette_replacement_reach_shared_controls() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let harness = ThemeHarness::new().unwrap();
    harness.global::<Anim>().set_enabled(false);
    harness.global::<Anim>().set_hover(false);
    let theme = harness.global::<Theme>();
    let original = theme.get_current();

    assert_eq!(
        harness.get_card_fill(),
        Brush::from(Color::from_rgb_u8(17, 24, 39))
    );
    assert_eq!(
        harness.get_card_border(),
        Brush::from(Color::from_argb_u8(20, 255, 255, 255))
    );
    assert_eq!(
        harness.get_button_fill(),
        Brush::from(Color::from_rgb_u8(139, 92, 246))
    );
    assert_eq!(harness.get_button_border(), harness.get_button_fill());
    assert_eq!(
        harness.get_input_fill(),
        Brush::from(Color::from_rgb_u8(28, 28, 34))
    );
    assert_eq!(
        harness.get_nav_fill(),
        Brush::from(Color::from_rgb_u8(16, 16, 25))
    );
    assert!(matches!(original.primary_button, Brush::LinearGradient(_)));
    assert!(matches!(original.progress_fill, Brush::LinearGradient(_)));
    assert_eq!(original.scrim_artwork, original.artwork_canvas);
    assert_eq!(original.scrim_episode_card, original.episode_card);

    let heights = (harness.get_button_height(), harness.get_input_height());
    let mut changed = original.clone();
    // Feed a real theme gradient through a mounted card, rather than just
    // reading the global back: Slint must invalidate the consuming binding.
    changed.card = changed.primary_button.clone();
    changed.border_soft = Color::from_argb_u8(96, 30, 60, 90);
    changed.accent = Color::from_rgb_u8(12, 34, 56);
    changed.focus_ring = Color::from_rgb_u8(78, 90, 123);
    changed.popup = Color::from_rgb_u8(45, 67, 89);
    changed.nav_rail = Color::from_rgb_u8(10, 20, 30);
    theme.set_current(changed.clone());

    assert_eq!(harness.get_card_fill(), changed.primary_button);
    assert_eq!(harness.get_card_border(), Brush::from(changed.border_soft));
    assert_eq!(harness.get_button_fill(), Brush::from(changed.accent));
    assert_eq!(harness.get_button_border(), Brush::from(changed.focus_ring));
    assert_eq!(harness.get_input_fill(), Brush::from(changed.popup));
    assert_eq!(harness.get_nav_fill(), Brush::from(changed.nav_rail));
    assert_eq!(
        (harness.get_button_height(), harness.get_input_height()),
        heights
    );

    theme.set_current(original.clone());
    assert_eq!(harness.get_card_fill(), original.card);
    assert_eq!(harness.get_card_border(), Brush::from(original.border_soft));

    // AppWindow re-exports the same palette API used by the backend. Globals
    // belong to each component tree; editing a harness never changes the app.
    let app = nova::AppWindow::new().unwrap();
    let app_theme = app.global::<nova::Theme>().get_current();
    assert_eq!(app_theme.card, original.card);
    assert_eq!(app_theme.accent, original.accent);
    assert_eq!(app_theme.primary_button, original.primary_button);
    nova_ui::apply_theme(&app, true);
    let black_theme = app.global::<nova::Theme>().get_current();
    let black = Color::from_rgb_u8(0, 0, 0);
    for surface in [
        black_theme.canvas,
        black_theme.artwork_canvas,
        black_theme.nav_bottom,
        black_theme.nav_rail,
        black_theme.scrim_artwork,
        black_theme.episode_card,
        black_theme.scrim_episode_card,
    ] {
        assert_eq!(surface, black);
    }
    assert_eq!(black_theme.accent, original.accent);
    assert_eq!(black_theme.card, original.card);
    assert_eq!(black_theme.primary_button, original.primary_button);
    assert!(app.global::<nova::Theme>().get_true_black());
    nova_ui::apply_theme(&app, false);
    assert_eq!(app.global::<nova::Theme>().get_current(), app_theme);
    assert!(!app.global::<nova::Theme>().get_true_black());
}
