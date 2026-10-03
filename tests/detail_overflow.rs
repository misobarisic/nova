//! Detail-page horizontal overflow regression test (headless).
//!
//! The entry screen (banner + tabs + episode picker, overview or episodes
//! tab) must never extend past the viewport width: on touch devices any
//! horizontal overflow lets the whole page wiggle left/right. Feeds the
//! page adversarial content (long titles, many genres, long stream lines)
//! and asserts nothing reaches past the window edge.

use slint::{ComponentHandle, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

fn s(v: &str) -> SharedString {
    SharedString::from(v)
}

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

/// Rightmost edge over every element: anything past the window width means
/// the page can pan horizontally. Returns the max edge plus a short list of
/// the worst offenders for diagnostics.
fn max_right_edge(app: &nova::AppWindow) -> (f32, Vec<String>) {
    use i_slint_backend_testing::{ElementHandle, ElementQuery};
    // Season content deliberately extends inside its clipped horizontal
    // viewport. Its offscreen cards must not count as whole-page overflow.
    let rail = ElementHandle::find_by_element_id(app, "DetailPage::season_flick").next();
    let rail_children = rail
        .as_ref()
        .map(|rail| {
            rail.query_descendants()
                .match_predicate(|_: &ElementHandle| true)
                .find_all()
        })
        .unwrap_or_default();
    let mut all: Vec<(f32, f32, f32, String)> = ElementQuery::from_root(app)
        .match_predicate(|_: &ElementHandle| true)
        .find_all()
        .into_iter()
        .map(|e| {
            let sz = e.size();
            let p = e.absolute_position();
            let t = e
                .type_name()
                .map(|x| x.to_string())
                .unwrap_or_else(|| "?".to_string());
            let mut right = p.x + sz.width;
            if let Some(rail) = rail.as_ref()
                && rail_children.iter().any(|child| {
                    child.id() == e.id() && child.absolute_position() == p && child.size() == sz
                })
            {
                right = right.min(rail.absolute_position().x + rail.size().width);
            }
            (right, p.x, sz.width, t)
        })
        .collect();
    all.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    let max = all.first().map(|e| e.0).unwrap_or(0.0);
    let offenders: Vec<String> = all
        .iter()
        .take(8)
        .map(|(r, x, w, t)| format!("right={r:.1} x={x:.1} w={w:.1} {t}"))
        .collect();
    (max, offenders)
}

fn episode_row(i: usize) -> nova::EpisodeRow {
    nova::EpisodeRow {
        text: s(&format!(
            "S1 E{i} · An Extremely Long Episode Title That Must Wrap Or Elide"
        )),
        details: s("A synopsis with averylongunbrokenwordthatcantwrapanywhere inside it."),
        lines: 4,
        thumb: slint::Image::default(),
        has_thumb: false,
        watched: false,
        progress: 0.0,
        ep_no: s(&format!("S1 E{i}")),
        date: s("January 1st, 2024, Extra Long Date Label"),
        runtime: Default::default(),
    }
}

fn show_detail(app: &nova::AppWindow) {
    app.set_modal_visible(true);
    app.set_detail_tab(3);
    app.set_modal_episodes(true);
    app.set_episode_context(s(""));
    app.set_selected_title(s(
        "Mushoku Tensei: Jobless Reincarnation Supercalifragilistic",
    ));
    app.set_selected_year(s("2021-"));
    app.set_selected_description(s(
        "A very long description with averylongunbrokenwordthatcantwrapanywhere in the middle of it.",
    ));
    app.set_selected_genre_list(
        Rc::new(VecModel::from(vec![
            s("Fantasy"),
            s("Drama"),
            s("Animation"),
            s("Adventure"),
            s("Anime"),
            s("Romance"),
            s("Supercalifragilisticexpialidocious"),
        ]))
        .into(),
    );
    app.set_season_names(
        Rc::new(VecModel::from(vec![
            s("Season 1"),
            s("Season 2"),
            s("Season 3"),
            s("Specials"),
        ]))
        .into(),
    );
    app.set_season_combo_idx(0);
    app.set_season_cards(
        Rc::new(VecModel::from(
            ["Season 1", "Season 2", "Season 3", "Specials"]
                .iter()
                .map(|n| nova::SeasonCard {
                    name: s(n),
                    thumb: slint::Image::default(),
                    has_thumb: false,
                    watched: false,
                    progress: 0.0,
                })
                .collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.set_episode_rows(
        Rc::new(VecModel::from(
            (1..=10).map(episode_row).collect::<Vec<_>>(),
        ))
        .into(),
    );
    app.set_episode_filter(s(""));
    app.set_streams(
        Rc::new(VecModel::from(vec![nova::StreamRow {
            id: s("stream-test"),
            text: s("Torrentio\nHDR 1080p averylongunbrokenfilename without any spaces at all.mkv\n[Tracker] udp://tracker.opentrackr.org:1337/announce info_hash=ABCDEF1234567890"),
            details: s(""),
            lines: 5,
            is_download: false,
            download_progress: 0.0,
            download_action: 0,
        }]))
        .into(),
    );
    app.set_streams_hint(s("1 stream(s) from 1 add-on(s)."));
}

#[test]
fn detail_screen_has_no_horizontal_overflow() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    show_detail(&app);

    let failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let app1 = app.as_weak();
    let failures1 = failures.clone();
    after(500, move || {
        let app = app1.upgrade().unwrap();
        // Episodes tab, episode list showing.
        let (edge, offenders) = max_right_edge(&app);
        if edge > 361.0 {
            failures1.borrow_mut().push(format!(
                "360px episodes list: content reaches {edge:.1}, want <= 360\n  {}",
                offenders.join("\n  ")
            ));
        }
        // Same content, overview tab.
        app.set_detail_tab(0);
        let app2 = app.as_weak();
        let failures2 = failures1.clone();
        after(400, move || {
            let app = app2.upgrade().unwrap();
            let (edge, offenders) = max_right_edge(&app);
            if edge > 361.0 {
                failures2.borrow_mut().push(format!(
                    "360px overview: content reaches {edge:.1}, want <= 360\n  {}",
                    offenders.join("\n  ")
                ));
            }
            // Picked-episode streams state.
            app.set_detail_tab(3);
            app.set_modal_episodes(false);
            app.set_episode_context(s("S1 E1 · Pilot With A Long Title"));
            let app3 = app.as_weak();
            let failures3 = failures2.clone();
            after(400, move || {
                let app = app3.upgrade().unwrap();
                let (edge, offenders) = max_right_edge(&app);
                if edge > 361.0 {
                    failures3.borrow_mut().push(format!(
                        "360px episode streams: content reaches {edge:.1}, want <= 360\n  {}",
                        offenders.join("\n  ")
                    ));
                }
                // Small phone: tab labels are the widest fixed row; they
                // must squeeze (elide) instead of pushing the page wide.
                // Back in list state so the top-bar context pill stays out.
                app.set_modal_episodes(true);
                app.set_episode_context(s(""));
                app.window().set_size(slint::PhysicalSize::new(320, 800));
                let app4 = app.as_weak();
                let failures4 = failures3.clone();
                after(400, move || {
                    let app = app4.upgrade().unwrap();
                    let (edge, offenders) = max_right_edge(&app);
                    if edge > 321.0 {
                        failures4.borrow_mut().push(format!(
                            "320px episodes list: content reaches {edge:.1}, want <= 320\n  {}",
                            offenders.join("\n  ")
                        ));
                    }
                    slint::quit_event_loop().unwrap();
                });
            });
        });
    });

    slint::run_event_loop().unwrap();

    let failures = failures.borrow();
    assert!(
        failures.is_empty(),
        "overflow failures:\n  {}",
        failures.join("\n  ")
    );
}
