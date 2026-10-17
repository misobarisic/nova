//! The real shared controls consume the current palette, including alpha and
//! gradient brushes, without remounting or changing their geometry.

use slint::{Brush, Color, ComponentHandle};

slint::slint! {
    import { Theme, Anim } from "@NovaUiShared";
    export { Theme, Anim } from "@NovaUiShared";
    import { PillButton, SettingsCard } from "../crates/ui/settings-controls.slint";
    import { Dropdown } from "../crates/ui/dropdown.slint";
    import { SearchField } from "../crates/ui/searchfield.slint";
    import { SideNav } from "../crates/ui/sidenav.slint";

    export component ThemeHarness inherits Window {
        width: 480px;
        height: 360px;
        out property <length> card_radius: card.border-radius;
        out property <length> button_radius: button.border-radius;
        out property <length> dropdown_radius: dropdown.border-radius;
        out property <brush> card_fill: card.background;
        out property <brush> card_border: card.border-color;
        out property <brush> button_fill: button.background;
        out property <brush> button_border: button.border-color;
        out property <brush> input_fill: input.background;
        out property <brush> secondary_fill: secondary.background;
        out property <brush> secondary_border: secondary.border-color;
        out property <length> secondary_border_width: secondary.border-width;
        out property <length> secondary_height: secondary.height;
        out property <brush> nav_fill: nav.panel-background;
        out property <length> button_height: button.height;
        out property <length> input_height: input.height;

        card := SettingsCard { x: 80px; y: 10px; width: 280px; height: 240px; }
        button := PillButton { x: 100px; y: 30px; label: "Theme test"; primary: true; focused: true; }
        input := SearchField { x: 100px; y: 100px; width: 220px; }
        secondary := PillButton { x: 100px; y: 170px; label: "Secondary"; }
        dropdown := Dropdown { x: 100px; y: 290px; width: 200px; model: ["One", "Two"]; current-index: 0; }
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
    assert_eq!(harness.get_card_radius(), 10.0);
    assert_eq!(harness.get_button_radius(), 8.0);
    assert_eq!(harness.get_dropdown_radius(), 8.0);
    theme.set_button_corner_radius(0.0);
    assert_eq!(harness.get_button_radius(), 0.0);
    assert_eq!(harness.get_dropdown_radius(), 0.0);
    theme.set_button_corner_radius(24.0);
    assert_eq!(harness.get_button_radius(), 22.0);
    assert_eq!(harness.get_dropdown_radius(), 16.0);
    theme.set_button_corner_radius(8.0);
    for radius in [0.0, 24.0, 10.0] {
        theme.set_card_corner_radius(radius);
        assert_eq!(harness.get_card_radius(), radius);
    }

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
    assert_eq!(black_theme.card, Brush::from(black));
    for surface in [
        black_theme.popup,
        black_theme.control,
        black_theme.settings_control,
        black_theme.stream_card,
        black_theme.season_card,
        black_theme.episode_toolbar,
        black_theme.tracking_panel,
        black_theme.player_menu,
    ] {
        assert_eq!(surface, black);
    }
    let secondary_height = harness.get_secondary_height();
    theme.set_true_black(true);
    // The harness uses the canonical compiled palette type with its own globals.
    // Feed it the app's actual surface values rather than recreating the mode.
    let mut harness_black = original.clone();
    harness_black.card = black_theme.card.clone();
    harness_black.popup = black_theme.popup;
    harness_black.nav_rail = black_theme.nav_rail;
    harness_black.settings_control = black_theme.settings_control;
    harness_black.control_border = black_theme.control_border;
    harness_black.border_soft = black_theme.border_soft;
    theme.set_current(harness_black);
    assert_eq!(harness.get_card_fill(), Brush::from(black));
    assert_eq!(harness.get_input_fill(), Brush::from(black));
    assert_eq!(harness.get_secondary_fill(), Brush::from(black));
    assert_eq!(harness.get_secondary_border_width(), 1.0);
    assert_eq!(
        harness.get_secondary_border(),
        Brush::from(black_theme.control_border)
    );
    assert_eq!(harness.get_button_fill(), Brush::from(original.accent));
    assert_eq!(
        (harness.get_button_height(), harness.get_input_height()),
        heights
    );
    assert_eq!(harness.get_secondary_height(), secondary_height);
    theme.set_true_black(false);
    theme.set_current(original.clone());
    assert_eq!(harness.get_secondary_border_width(), 0.0);
    assert_eq!(
        harness.get_secondary_fill(),
        Brush::from(original.settings_control)
    );
    assert_eq!(harness.get_card_fill(), original.card);
    assert_eq!(black_theme.primary_button, original.primary_button);
    assert!(app.global::<nova::Theme>().get_true_black());
    nova_ui::apply_theme(&app, false);
    assert_eq!(app.global::<nova::Theme>().get_current(), app_theme);
    assert!(!app.global::<nova::Theme>().get_true_black());

    // Presets reach real mounted controls without touching semantic colors or
    // surface mode. Their bright action fills need dark foreground ink.
    let contrast = |a: Color, b: Color| {
        let luminance = |c: Color| {
            let linear = |v: u8| {
                let v = f64::from(v) / 255.0;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(c.red()) + 0.7152 * linear(c.green()) + 0.0722 * linear(c.blue())
        };
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    };
    for index in 1..8 {
        app.set_accent_color(index);
        for black_mode in [false, true] {
            nova_ui::apply_theme(&app, black_mode);
            let palette = app.global::<nova::Theme>().get_current();
            assert_ne!(palette.accent, original.accent);
            assert_eq!(palette.text_error, original.text_error);
            assert_eq!(palette.text_destructive, original.text_destructive);
            assert_eq!(palette.scope_local_fill, original.scope_local_fill);
            assert_eq!(palette.watched_badge, original.watched_badge);
            assert_eq!(
                palette.canvas,
                if black_mode { black } else { original.canvas }
            );
            assert!(contrast(palette.on_accent, palette.accent) >= 4.5);
            assert!(contrast(palette.on_accent, palette.accent_pressed) >= 4.5);
            assert!(contrast(palette.text_selected, palette.selection_fill) >= 4.5);
            assert!(contrast(palette.text_bright, palette.episode_number_badge) >= 4.5);
            for brush in [&palette.primary_button, &palette.primary_button_hover] {
                let Brush::LinearGradient(gradient) = brush else {
                    panic!("preset keeps action gradients")
                };
                for stop in gradient.stops() {
                    assert!(contrast(palette.on_accent, stop.color) >= 4.5);
                }
            }
            theme.set_current(palette.clone());
            assert_eq!(harness.get_button_fill(), Brush::from(palette.accent));
            assert_eq!(harness.get_nav_fill(), Brush::from(palette.nav_rail));
            assert_eq!(
                (harness.get_button_height(), harness.get_input_height()),
                heights
            );
        }
    }
    app.set_background_style_index(1);
    let mut canvases = Vec::new();
    for index in 0..8 {
        app.set_accent_color(index);
        nova_ui::apply_theme(&app, false);
        let global = app.global::<nova::Theme>();
        let tinted = global.get_current();
        assert!(global.get_accent_tinted());
        assert_ne!(tinted.canvas, original.canvas);
        assert_eq!(tinted.canvas, tinted.artwork_canvas);
        assert_eq!(tinted.canvas, tinted.artwork_backdrop);
        assert_eq!(tinted.canvas, tinted.nav_rail);
        assert_eq!(tinted.canvas, tinted.nav_bottom);
        assert_eq!(tinted.scrim_artwork, tinted.artwork_canvas);
        assert_eq!(tinted.scrim_episode_card, tinted.episode_card);
        assert_ne!(tinted.card, Brush::from(tinted.canvas));
        assert_ne!(tinted.popup, tinted.canvas);
        assert_eq!(tinted.scrim_player, original.scrim_player);
        assert_eq!(tinted.scrim_settings, original.scrim_settings);
        assert_eq!(tinted.text_error, original.text_error);
        for (shape, image) in [
            global.get_home_bottom_scrim(),
            global.get_home_side_scrim(),
            global.get_detail_bottom_scrim(),
            global.get_detail_side_scrim(),
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!((image.size().width, image.size().height), (1024, 1024));
            let pixels = image.to_rgba8().expect("baked fade pixels are readable");
            let pixel = pixels.as_slice()[if shape % 2 == 0 {
                1023 * 1024 + 512
            } else {
                512 * 1024
            }];
            for (actual, expected) in [
                (pixel.r, tinted.canvas.red()),
                (pixel.g, tinted.canvas.green()),
                (pixel.b, tinted.canvas.blue()),
            ] {
                assert!(
                    actual.abs_diff(expected) <= 1,
                    "fade edge matches its canvas for accent {index}"
                );
            }
            assert!(pixel.a >= 239);
        }
        for background in [
            tinted.canvas,
            tinted.popup,
            tinted.control,
            tinted.episode_card,
        ] {
            assert!(contrast(tinted.text_primary, background) >= 4.5);
        }
        theme.set_current(tinted.clone());
        assert_eq!(harness.get_nav_fill(), Brush::from(tinted.nav_rail));
        canvases.push(tinted.canvas);
        nova_ui::apply_theme(&app, true);
        assert!(!global.get_accent_tinted());
        assert_eq!(global.get_current().canvas, black);
    }
    for pair in canvases.windows(2) {
        assert_ne!(pair[0], pair[1]);
    }
    app.set_background_style_index(0);
    app.set_accent_color(0);
    nova_ui::apply_theme(&app, false);
    assert_eq!(app.global::<nova::Theme>().get_current(), original);
}
