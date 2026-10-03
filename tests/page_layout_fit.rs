//! Container-fit and hover regressions for Home, Discover and My Library.
//!
//! Query the intended containers instead of every on-screen right edge:
//! carousels deliberately instantiate cards outside their clipped viewport.

use i_slint_backend_testing::{ElementHandle, ElementQuery};
use slint::{ComponentHandle, LogicalPosition, ModelRc, SharedString, VecModel};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

fn model<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    Rc::new(VecModel::from(rows)).into()
}

fn settle(app: &nova::AppWindow) {
    for _ in 0..5 {
        // The testing backend has no renderer to read layout bindings each
        // frame. Evaluate geometry before advancing change handlers/timers.
        for element in ElementQuery::from_root(app)
            .match_predicate(|_| true)
            .find_all()
        {
            let _ = (element.size(), element.absolute_position());
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
    }
}

#[derive(Clone, Copy, Debug)]
struct Bounds {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

impl Bounds {
    fn of(element: &ElementHandle) -> Self {
        let p = element.absolute_position();
        let s = element.size();
        Self {
            left: p.x,
            top: p.y,
            right: p.x + s.width,
            bottom: p.y + s.height,
        }
    }

    fn contains(self, element: &ElementHandle) -> bool {
        let b = Self::of(element);
        b.left >= self.left - 1.0
            && b.top >= self.top - 1.0
            && b.right <= self.right + 1.0
            && b.bottom <= self.bottom + 1.0
    }
}

fn record(failures: &RefCell<Vec<String>>, condition: bool, message: impl Into<String>) {
    if !condition {
        failures.borrow_mut().push(message.into());
    }
}

fn text_fits(failures: &RefCell<Vec<String>>, container: &ElementHandle, case: &str) {
    let bounds = Bounds::of(container);
    for text in container
        .query_descendants()
        .match_type_name("Text")
        .find_all()
    {
        // The unloaded poster placeholder is intentionally clipped by the
        // poster at very high density; it is artwork, not a card caption.
        if text.computed_opacity() < 0.1 || text.accessible_label().is_some_and(|s| s == "🎬") {
            continue;
        }
        record(
            failures,
            bounds.contains(&text),
            format!(
                "{case}: {:?} {:?} exceeds its container {:?}",
                text.accessible_label(),
                Bounds::of(&text),
                bounds,
            ),
        );
    }
}

fn content_bounds(width: u32, height: u32, left: f32, right: f32) -> Bounds {
    Bounds {
        left: 18.0
            + left
            + if width >= 1200 {
                240.0
            } else if width >= 700 {
                76.0
            } else {
                0.0
            },
        top: 18.0,
        right: width as f32 - 18.0 - right,
        bottom: height as f32 - 18.0,
    }
}

fn heading_fits(
    app: &nova::AppWindow,
    failures: &RefCell<Vec<String>>,
    id: &str,
    bounds: Bounds,
    case: &str,
) {
    let heading = ElementHandle::find_by_element_id(app, id).next().expect(id);
    record(
        failures,
        bounds.contains(&heading),
        format!("{case}: heading outside page"),
    );
    text_fits(failures, &heading, case);
}

fn grid_fits(
    app: &nova::AppWindow,
    failures: &RefCell<Vec<String>>,
    id: &str,
    scroll_id: &str,
    case: &str,
) {
    let grid = ElementHandle::find_by_element_id(app, id).next().expect(id);
    let scroll = ElementHandle::find_by_element_id(app, scroll_id)
        .next()
        .expect(scroll_id);
    let g = Bounds::of(&grid);
    let s = Bounds::of(&scroll);
    record(
        failures,
        g.left >= s.left - 1.0 && g.right <= s.right + 1.0,
        format!(
            "{case}: grid [{:.1}, {:.1}] exceeds viewport [{:.1}, {:.1}]",
            g.left, g.right, s.left, s.right
        ),
    );
    record(
        failures,
        if s.right - s.left <= 400.0 {
            (g.right - g.left - (s.right - s.left)).abs() < 1.0
        } else {
            g.right - g.left >= 284.0
        },
        format!("{case}: grid is unexpectedly narrower than viewport: {g:?} / {s:?}"),
    );
}

fn safe_area(app: &nova::AppWindow, left: f32, right: f32) {
    // Exercise AppWindow's real safe-area forwarding, with no test-only UI API.
    i_slint_core::window::WindowInner::from_pub(app.window()).set_window_item_safe_area(
        i_slint_core::lengths::LogicalEdges::new(0.0, 0.0, left, right),
    );
}

fn pointer(app: &nova::AppWindow, kind: &str, position: LogicalPosition) {
    use slint::platform::{PointerEventButton, WindowEvent};
    app.window().dispatch_event(match kind {
        "down" => WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        },
        "up" => WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        },
        _ => WindowEvent::PointerMoved { position },
    });
}

fn key(app: &nova::AppWindow, key: slint::platform::Key) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.into() });
    settle(app);
}

fn focused_card_fits(
    app: &nova::AppWindow,
    failures: &RefCell<Vec<String>>,
    grid_id: &str,
    card_selector: &str,
    scroll_id: &str,
    index: i32,
    case: &str,
) {
    let grid = ElementHandle::find_by_element_id(app, grid_id)
        .next()
        .unwrap();
    // Queries skip clipped delegates, so locate the focused model title
    // instead of counting from the start. Ignore Home's covered landing cards.
    let query = ElementQuery::from_root(app);
    let query = if card_selector.contains("::") {
        query.match_id(card_selector)
    } else {
        query.match_type_name(card_selector)
    };
    let title = media(index as usize).title;
    let cards = query.find_all();
    let card = cards
        .iter()
        .filter(|e| e.computed_opacity() > 0.1)
        .find(|card| {
            card.query_descendants()
                .match_type_name("Text")
                .find_all()
                .iter()
                .any(|text| text.accessible_label().is_some_and(|label| label == title))
        })
        .unwrap_or_else(|| panic!("{case}: focused card {index} is not visible"));
    let scroll = ElementHandle::find_by_element_id(app, scroll_id)
        .next()
        .unwrap();
    record(
        failures,
        Bounds::of(&scroll).contains(card) && Bounds::of(card).left >= Bounds::of(&grid).left - 1.0,
        format!("{case}: keyboard card {} outside viewport", index),
    );
}

fn media(i: usize) -> nova::MediaCard {
    nova::MediaCard {
        id: format!("media:{i}").into(),
        title: format!("Media {i} has a very long title and anunbrokentokenfortesting").into(),
        year: "2020–2026 · Extended release information".into(),
        badge: "12 episodes left".into(),
        ..Default::default()
    }
}

#[test]
fn pages_fit_translations_cutouts_density_and_touch() {
    i_slint_backend_testing::init_integration_test_with_mock_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().show().unwrap();
    app.global::<nova::Anim>().set_enabled(false);
    app.set_touch_menus(true);
    app.set_library(model((0..120).map(media).collect()));
    app.set_catalog(model((0..24).map(media).collect()));
    app.set_search_results(model((0..24).map(media).collect()));
    app.set_home_continue(model(
        (0..24)
            .map(|i| nova::ContinueRow {
                id: format!("continue:{i}").into(),
                title: media(i).title,
                subtitle: "S01 E12 · An unusually long episode subtitle".into(),
                badge: 1 + i as i32 % 2,
                ..Default::default()
            })
            .collect(),
    ));
    app.set_home_upcoming(model(
        (0..24)
            .map(|i| nova::UpcomingRow {
                id: format!("upcoming:{i}").into(),
                title: media(i).title,
                subtitle: "S01 E13 · Another unusually long episode subtitle".into(),
                date: "Za 12 dana".into(),
                index: i as i32,
                ..Default::default()
            })
            .collect(),
    ));
    app.set_home_featured_title("The Extremely Long Featured Title With A Long Final Part".into());
    app.set_home_featured_type("movie".into());
    app.set_home_featured_rating("8.4".into());
    app.set_home_featured_runtime("142 min".into());
    app.set_home_featured_release_info("2016–2024".into());
    app.set_home_featured_tagline(
        "A long optional tagline that wraps across multiple lines in the phone layout.".into(),
    );
    app.set_home_featured_description("A long description that must wrap inside its column, stay clear of both actions, and remain readable at the desktop breakpoint.".into());
    app.set_home_featured_count(15);
    app.set_home_featured_revision(1);
    app.set_library_category_names(model(vec![
        SharedString::from("Watching"),
        SharedString::from("A custom category with a long label"),
    ]));
    let failures = RefCell::new(Vec::new());

    for language in ["en", "hr"] {
        if let Err(error) = slint::select_bundled_translation(language) {
            #[cfg(not(feature = "live-preview"))]
            panic!("AOT build must bundle {language}: {error}");
            #[cfg(feature = "live-preview")]
            {
                assert!(matches!(
                    error,
                    slint::SelectBundledTranslationError::NoTranslationsBundled
                ));
                continue;
            }
        }
        for (width, height, left, right) in [
            (320, 800, 0.0, 0.0),
            (360, 800, 0.0, 0.0),
            (390, 800, 0.0, 0.0),
            (360, 800, 24.0, 16.0),
            (699, 800, 0.0, 0.0),
            (700, 800, 0.0, 0.0),
            (1100, 800, 0.0, 0.0),
            (780, 360, 44.0, 24.0),
        ] {
            app.window()
                .set_size(slint::PhysicalSize::new(width, height));
            safe_area(&app, left, right);
            let bounds = content_bounds(width, height, left, right);
            for columns in [2, 3, 6] {
                let case =
                    format!("{language} {width}x{height} cutout {left}/{right}, {columns} cols");
                app.set_kb_active(false);
                app.set_library_min_cols(columns);
                app.set_discover_min_cols(columns);
                app.set_show_home(false);
                app.set_show_library(true);
                app.set_library_scroll_y(0.0);
                app.set_library_kb_zone(0);
                settle(&app);
                heading_fits(
                    &app,
                    &failures,
                    "LibraryPage::library_heading",
                    bounds,
                    &case,
                );
                grid_fits(
                    &app,
                    &failures,
                    "LibraryPage::library_grid",
                    "LibraryPage::lib_scroll",
                    &case,
                );
                for card in ElementHandle::find_by_element_id(&app, "LibraryPage::library_lift")
                    .take(columns as usize)
                {
                    text_fits(&failures, &card, &case);
                }

                app.set_show_library(false);
                for whole_page in [false, true] {
                    app.set_discover_scroll_page(whole_page);
                    app.set_discover_scroll_y(0.0);
                    app.set_discover_search_scroll_y(0.0);
                    for results in [false, true] {
                        app.set_discover_search_open(results);
                        settle(&app);
                        let header =
                            ElementHandle::find_by_element_type_name(&app, "DiscoverHeader")
                                .next()
                                .unwrap();
                        record(
                            &failures,
                            bounds.contains(&header),
                            format!("{case}: Discover header exceeds page"),
                        );
                        // Filter rails intentionally overflow; the results
                        // heading itself must fit and grow vertically.
                        if results {
                            heading_fits(
                                &app,
                                &failures,
                                "DiscoverHeader::results_heading",
                                bounds,
                                &format!("{case}, results whole_page={whole_page}"),
                            );
                        }
                        grid_fits(
                            &app,
                            &failures,
                            "DiscoverGrid::grid_box",
                            if whole_page {
                                "DiscoverPage::page_scroll"
                            } else {
                                "DiscoverPage::grid_scroll"
                            },
                            &case,
                        );
                        for card in ElementHandle::find_by_element_id(&app, "DiscoverGrid::lift")
                            .take(columns as usize)
                        {
                            text_fits(&failures, &card, &case);
                        }
                    }
                }

                app.set_show_home(true);
                app.set_home_view(0);
                app.set_home_scroll_y(0.0);
                settle(&app);
                let banner = ElementHandle::find_by_element_type_name(&app, "FeaturedShowcase")
                    .next()
                    .unwrap();
                text_fits(&failures, &banner, &case);
                for action in banner
                    .query_descendants()
                    .match_type_name("FeaturedActionButton")
                    .find_all()
                {
                    record(
                        &failures,
                        Bounds::of(&banner).contains(&action),
                        format!("{case}: featured action exceeds banner"),
                    );
                }
                // AOT can eliminate the inline HorizontalLayout; its actual
                // buttons retain geometry in both compiled and preview builds.
                // In a short landscape viewport, bring the bottom controls
                // into view before querying the clipped descendants.
                let extra = (Bounds::of(&banner).bottom - bounds.bottom).max(0.0);
                if extra > 0.0 {
                    app.window()
                        .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                            position: LogicalPosition::new(
                                (bounds.left + bounds.right) / 2.0,
                                (bounds.top + bounds.bottom) / 2.0,
                            ),
                            delta_x: 0.0,
                            delta_y: -extra - 8.0,
                        });
                    settle(&app);
                }
                let pagers = ElementHandle::find_by_element_type_name(&app, "FeaturedPagerButton")
                    .collect::<Vec<_>>();
                assert_eq!(pagers.len(), 2, "{case}: featured paging buttons");
                let action =
                    ElementHandle::find_by_element_id(&app, "FeaturedShowcase::featured_actions")
                        .next()
                        .unwrap();
                let p = Bounds::of(&pagers[0]);
                let a = Bounds::of(&action);
                record(
                    &failures,
                    pagers
                        .iter()
                        .all(|pager| Bounds::of(&banner).contains(pager)),
                    format!("{case}: pager exceeds banner"),
                );
                record(
                    &failures,
                    a.right + 10.0 <= p.left || a.bottom + 2.0 <= p.top,
                    format!("{case}: featured controls overlap"),
                );
                for heading in ElementHandle::find_by_element_type_name(&app, "SectionHeader") {
                    text_fits(&failures, &heading, &case);
                }
                for view in [1, 2] {
                    app.set_home_view(view);
                    app.set_home_all_scroll_y(0.0);
                    settle(&app);
                    heading_fits(&app, &failures, "HomePage::sub_heading", bounds, &case);
                    for toggle in ElementHandle::find_by_element_type_name(&app, "CalToggle") {
                        record(
                            &failures,
                            bounds.contains(&toggle),
                            format!("{case}: calendar toggle exceeds page"),
                        );
                    }
                    grid_fits(
                        &app,
                        &failures,
                        if view == 1 {
                            "HomePage::continue_grid"
                        } else {
                            "HomePage::upcoming_grid"
                        },
                        "HomePage::sub_scroll",
                        &case,
                    );
                    for kind in ["ContinueCard", "UpcomingCard"] {
                        for card in ElementHandle::find_by_element_type_name(&app, kind) {
                            if card.computed_opacity() > 0.1 {
                                text_fits(&failures, &card, &case);
                            }
                        }
                    }
                }
            }
        }
    }

    // Short drags can latch TouchArea.has-hover without opening an item.
    // The same binding must still permit the desktop pointer lift, with
    // enough headroom above the first row for its rounded poster corners.
    slint::select_bundled_translation("en").ok();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    safe_area(&app, 0.0, 0.0);
    app.set_library_min_cols(2);
    app.set_discover_min_cols(2);
    let picks = Rc::new(Cell::new(0));
    let captured = picks.clone();
    app.on_library_item_picked(move |_| captured.set(captured.get() + 1));
    let captured = picks.clone();
    app.on_item_selected(move |_, _| captured.set(captured.get() + 1));
    for page in ["library", "discover", "discover-touch"] {
        app.set_show_home(false);
        app.set_show_library(page == "library");
        app.set_discover_scroll_page(page == "discover-touch");
        app.set_discover_search_open(false);
        app.set_library_scroll_y(0.0);
        app.set_discover_scroll_y(0.0);
        app.set_touch_menus(true);
        settle(&app);
        let id = if page == "library" {
            "LibraryPage::library_lift"
        } else {
            "DiscoverGrid::lift"
        };
        let lift = ElementHandle::find_by_element_id(&app, id).next().unwrap();
        let baseline = Bounds::of(&lift);
        let start = LogicalPosition::new(baseline.left + 30.0, baseline.top + 80.0);
        pointer(&app, "down", start);
        pointer(&app, "move", LogicalPosition::new(start.x + 3.0, start.y));
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
        pointer(
            &app,
            "move",
            LogicalPosition::new(start.x + 40.0, start.y + 20.0),
        );
        pointer(
            &app,
            "move",
            LogicalPosition::new(start.x + 150.0, start.y + 20.0),
        );
        pointer(
            &app,
            "up",
            LogicalPosition::new(start.x + 150.0, start.y + 20.0),
        );
        settle(&app);
        record(
            &failures,
            (Bounds::of(&lift).top - baseline.top).abs() < 0.5,
            format!("{page}: touch drag left a card lifted"),
        );
        record(
            &failures,
            picks.get() == 0,
            format!("{page}: drag opened an item"),
        );
        pointer(&app, "move", LogicalPosition::new(5.0, 5.0));
        app.set_touch_menus(false);
        settle(&app);
        pointer(&app, "move", start);
        settle(&app);
        record(
            &failures,
            (Bounds::of(&lift).top - baseline.top + 6.0).abs() < 0.5,
            format!(
                "{page}: desktop hover no longer lifts ({baseline:?} -> {:?}, start {start:?})",
                Bounds::of(&lift)
            ),
        );
        let scroll_id = match page {
            "library" => "LibraryPage::lib_scroll",
            "discover" => "DiscoverPage::grid_scroll",
            _ => "DiscoverPage::page_scroll",
        };
        let scroll = ElementHandle::find_by_element_id(&app, scroll_id)
            .next()
            .unwrap();
        record(
            &failures,
            Bounds::of(&lift).top >= Bounds::of(&scroll).top,
            format!("{page}: hovered first row clips above viewport"),
        );
    }

    // Keyboard follow uses each grid's actual origin, including hover space
    // and the whole-page Discover header. Walk down far enough to scroll,
    // then return to the first row without pulling it above the viewport.
    app.set_touch_menus(true);
    safe_area(&app, 24.0, 16.0);
    pointer(&app, "move", LogicalPosition::new(5.0, 5.0));
    for page in ["library", "discover", "discover-touch", "home"] {
        app.window().set_size(slint::PhysicalSize::new(360, 800));
        app.set_show_library(page == "library");
        app.set_show_home(page == "home");
        app.set_home_view(1);
        app.set_discover_scroll_page(page == "discover-touch");
        app.set_library_scroll_y(0.0);
        app.set_discover_scroll_y(0.0);
        app.set_home_all_scroll_y(0.0);
        app.set_library_kb_idx(0);
        app.set_discover_kb_idx(0);
        app.set_home_kb_idx(0);
        settle(&app);
        app.set_library_kb_zone(2);
        app.set_discover_kb_zone(3);
        app.set_home_kb_zone(1);
        app.set_kb_active(true);
        for _ in 0..6 {
            key(&app, slint::platform::Key::DownArrow);
        }
        let (grid, card, scroll, index) = match page {
            "library" => (
                "LibraryPage::library_grid",
                "LibraryPage::library_lift",
                "LibraryPage::lib_scroll",
                app.get_library_kb_idx(),
            ),
            "home" => (
                "HomePage::continue_grid",
                "ContinueCard",
                "HomePage::sub_scroll",
                app.get_home_kb_idx(),
            ),
            _ => (
                "DiscoverGrid::grid_box",
                "DiscoverGrid::lift",
                if page == "discover-touch" {
                    "DiscoverPage::page_scroll"
                } else {
                    "DiscoverPage::grid_scroll"
                },
                app.get_discover_kb_idx(),
            ),
        };
        record(
            &failures,
            index > 0,
            format!("{page}: Down did not move focus"),
        );
        focused_card_fits(&app, &failures, grid, card, scroll, index, page);
        for _ in 0..6 {
            key(&app, slint::platform::Key::UpArrow);
        }
        focused_card_fits(&app, &failures, grid, card, scroll, 0, page);
        // Resize while the same page remains mounted: viewport width mirrors
        // must update independently of page creation and model replacement.
        app.window().set_size(slint::PhysicalSize::new(390, 800));
        settle(&app);
        grid_fits(
            &app,
            &failures,
            grid,
            scroll,
            &format!("{page} after resize"),
        );
    }

    // Discover's keyboard write-back must not echo native scroll samples
    // into the Flickable and cancel its momentum on the next frame.
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.set_show_home(false);
    app.set_show_library(false);
    app.set_kb_active(false);
    for whole_page in [false, true] {
        app.set_discover_scroll_page(whole_page);
        app.set_discover_scroll_y(0.0);
        settle(&app);
        let scroll = ElementHandle::find_by_element_id(
            &app,
            if whole_page {
                "DiscoverPage::page_scroll"
            } else {
                "DiscoverPage::grid_scroll"
            },
        )
        .next()
        .unwrap();
        let bounds = Bounds::of(&scroll);
        let start = LogicalPosition::new(bounds.left + 30.0, bounds.top + 200.0);
        pointer(&app, "down", start);
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(120));
        for step in 1..=4 {
            pointer(
                &app,
                "move",
                LogicalPosition::new(start.x, start.y - 25.0 * step as f32),
            );
            i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
        }
        pointer(&app, "up", LogicalPosition::new(start.x, start.y - 100.0));
        let released = app.get_discover_scroll_y();
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(16));
        record(
            &failures,
            app.get_discover_scroll_y() < released - 10.0,
            format!("Discover whole_page={whole_page}: scroll mirroring cancelled inertia"),
        );
    }

    // Empty models have long hints instead of card grids. Keep those hints
    // inside the same cutout-cleared page bounds as the populated cases.
    let hint = "A lengthy empty-state message that wraps onto several lines and stays inside the page content.";
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.set_kb_active(false);
    app.set_library(model(Vec::new()));
    app.set_catalog(model(Vec::new()));
    app.set_search_results(model(Vec::new()));
    app.set_home_continue(model(Vec::new()));
    app.set_home_upcoming(model(Vec::new()));
    app.set_home_featured_count(0);
    app.set_library_empty_hint(hint.into());
    app.set_empty_hint(hint.into());
    app.set_search_empty_hint(hint.into());
    app.set_home_empty_hint(hint.into());
    for page in ["library", "discover", "discover-search", "home"] {
        app.set_show_library(page == "library");
        app.set_show_home(page == "home");
        app.set_home_view(0);
        app.set_discover_scroll_page(true);
        app.set_discover_search_open(page == "discover-search");
        settle(&app);
        let text = ElementHandle::find_by_accessible_label(&app, hint)
            .next()
            .expect("empty hint");
        record(
            &failures,
            content_bounds(360, 800, 24.0, 16.0).contains(&text),
            format!("{page}: empty hint exceeds page"),
        );
    }

    // A taller incoming caption must fit throughout a crossfade, while both
    // playback and paging controls stay below the outgoing and incoming text layers.
    app.set_show_library(false);
    app.set_show_home(true);
    app.set_home_featured_title("Short title".into());
    app.set_home_featured_count(15);
    app.set_home_featured_revision(2);
    settle(&app);
    app.global::<nova::Anim>().set_enabled(true);
    app.set_home_featured_title("The Extremely Long Featured Title With A Long Final Part".into());
    app.set_home_featured_revision(3);
    for phase in ["during fade", "after fade"] {
        settle(&app);
        let banner = ElementHandle::find_by_element_type_name(&app, "FeaturedShowcase")
            .next()
            .unwrap();
        text_fits(&failures, &banner, phase);
        let actions = ElementHandle::find_by_element_id(&app, "FeaturedShowcase::featured_actions")
            .next()
            .unwrap();
        for id in ["previous_caption", "current_caption"] {
            let caption =
                ElementHandle::find_by_element_id(&app, &format!("FeaturedShowcase::{id}"))
                    .next()
                    .unwrap();
            if caption.computed_opacity() > 0.1 {
                record(
                    &failures,
                    Bounds::of(&banner).contains(&caption)
                        && Bounds::of(&caption).bottom + 11.0 <= Bounds::of(&actions).top,
                    format!("{phase}: caption overlaps actions or exceeds banner"),
                );
            }
        }
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(400));
    }
    let failures = failures.borrow();
    assert!(
        failures.is_empty(),
        "page layout failures ({}):\n{}",
        failures.len(),
        failures
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
