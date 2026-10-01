//! Android system-back navigation (headless).
//!
//! The Android backend maps the system back gesture/button to Slint
//! `Key::Back`. AppWindow captures press/release before focused inputs and
//! routes each press to the active screen; screens pop their nearest layer,
//! while Home's root backgrounds the app through `exit_to_background`.
//! Synthetic key events go through `Window::dispatch_event`, so this runs
//! headless. One test function: the testing backend initializes once per
//! process, so phases run sequentially on a single window.

use i_slint_backend_testing::ElementHandle;
use slint::{ComponentHandle, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

fn back(app: &nova::AppWindow) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Back.into(),
        });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased {
            text: slint::platform::Key::Back.into(),
        });
}

fn back_with_repeat(app: &nova::AppWindow) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed {
            text: slint::platform::Key::Back.into(),
        });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressRepeated {
            text: slint::platform::Key::Back.into(),
        });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased {
            text: slint::platform::Key::Back.into(),
        });
}

fn after(ms: u64, body: impl FnOnce() + 'static) {
    slint::Timer::single_shot(std::time::Duration::from_millis(ms), body);
}

#[test]
fn system_back_pops_one_layer_at_a_time() {
    i_slint_backend_testing::init_integration_test_with_system_time();
    let app = nova::AppWindow::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(360, 800));
    app.window().show().unwrap();
    // Home is the default landing page; clear it so the modal and settings
    // phases own the screen (a set home flag would keep SettingsPage
    // unbuilt, exactly as in the real app).
    app.set_show_home(false);

    let failures: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let fail = |failures: &Rc<RefCell<Vec<String>>>, cond: bool, msg: &str| {
        if !cond {
            failures.borrow_mut().push(msg.to_string());
        }
    };

    // ---- Detail modal: streams → episodes → closed ---------------------
    // Mirrors the `episodes_back` wiring in `run.rs`, trimmed for headless.
    app.on_episodes_back({
        let app = app.as_weak();
        move || {
            app.upgrade().unwrap().set_modal_episodes(true);
        }
    });
    app.set_modal_visible(true);
    app.set_modal_episodes(false);
    app.set_episode_context(SharedString::from("S1 E1"));

    // ---- Settings landing → home ---------------------------------------
    // (Opened in-phase, see below: same-tick create+focus races destroy.)
    let went_home = Rc::new(RefCell::new(false));
    app.on_home_picked({
        let went_home = went_home.clone();
        move || *went_home.borrow_mut() = true
    });

    // ---- Player overlay closes ------------------------------------------
    // Mirrors `run.rs`: closing the player drops the overlay.
    app.on_close_player({
        let app = app.as_weak();
        move || {
            app.upgrade().unwrap().set_player_open(false);
        }
    });

    // ---- Home root screen backgrounds the app ---------------------------
    let background_count = Rc::new(RefCell::new(0usize));
    app.on_exit_to_background({
        let background_count = background_count.clone();
        move || *background_count.borrow_mut() += 1
    });

    app.on_resume_cancel({
        let app = app.as_weak();
        move || app.upgrade().unwrap().set_resume_prompt_visible(false)
    });

    // NOTE: each phase gets its own tick. Creating a page focuses its key
    // scope, but destroying the previous page clears focus afterwards when
    // both happen in the same tick — so setup-for-next-phase and dispatch
    // must not share a tick.
    app.set_modal_visible(true);
    app.set_modal_episodes(false);
    app.set_episode_context(SharedString::from("S1 E1"));
    app.set_resume_prompt_visible(true);

    let app1 = app.as_weak();
    let failures1 = failures.clone();
    after(400, move || {
        let app = app1.upgrade().unwrap();

        back(&app);
        fail(
            &failures1,
            !app.get_resume_prompt_visible() && app.get_modal_visible(),
            "Back in the resume prompt must dismiss only the prompt",
        );
        fail(
            &failures1,
            !app.get_modal_episodes(),
            "dismissing the resume prompt must keep detail on streams",
        );
        back(&app);
        fail(
            &failures1,
            app.get_modal_episodes(),
            "Back in streams must show episodes",
        );
        fail(
            &failures1,
            app.get_modal_visible(),
            "modal closed too early",
        );
        back(&app);
        fail(
            &failures1,
            !app.get_modal_visible(),
            "Back in episodes must close modal",
        );

        // Next tick: open the settings landing (fresh focus).
        let app2 = app.as_weak();
        let failures2 = failures1.clone();
        after(400, move || {
            let app = app2.upgrade().unwrap();
            app.set_show_settings(true);

            let app3 = app.as_weak();
            let failures3 = failures2.clone();
            after(400, move || {
                let app = app3.upgrade().unwrap();
                // Real flow: the user taps into the page (focus follows the
                // tap) and opens a subpage then backs out twice. The first
                // Back closes the subpage (`close_page`, as the top-left
                // chevron does); the second lands home (observable here).
                let link = i_slint_backend_testing::ElementHandle::find_by_element_type_name(
                    &app,
                    "SettingsLink",
                )
                .next();
                let Some(link) = link else {
                    fail(&failures3, false, "DIAG: no SettingsLink found at all");
                    fail(
                        &failures3,
                        false,
                        &format!(
                            "DIAG state: settings={} modal={} player={} home={} lib={} episodes={} ctx={:?}",
                            app.get_show_settings(),
                            app.get_modal_visible(),
                            app.get_player_open(),
                            app.get_show_home(),
                            app.get_show_library(),
                            app.get_modal_episodes(),
                            app.get_episode_context().to_string(),
                        ),
                    );
                    slint::quit_event_loop().unwrap();
                    return;
                };
                let app4 = app.as_weak();
                let failures4 = failures3.clone();
                slint::spawn_local(async move {
                    link.single_click(slint::platform::PointerEventButton::Left)
                        .await;
                    after(400, move || {
                        let app = app4.upgrade().unwrap();
                        // First Back closes the subpage; `sel` resets only
                        // after the 280ms slide-out, so the second Back (to
                        // landing → home) needs its own tick, like a real
                        // double-press.
                        back(&app);
                        let app4b = app.as_weak();
                        let failures4b = failures4.clone();
                        after(500, move || {
                            let app = app4b.upgrade().unwrap();
                            back(&app);
                            fail(
                                &failures4b,
                                *went_home.borrow(),
                                "Back out of a settings subpage must land home",
                            );
                            app.set_show_settings(false);
                            app.set_player_open(true);

                            let app5 = app.as_weak();
                            let failures5 = failures4.clone();
                            after(400, move || {
                                let app = app5.upgrade().unwrap();
                                back(&app);
                                fail(
                                    &failures5,
                                    !app.get_player_open(),
                                    "Back must close the player",
                                );

                                app.set_show_home(true);
                                // A remembered "see all" subpage: the first
                                // Back pops it to the landing, the next one
                                // backgrounds the app.
                                app.set_home_view(1);
                                let app6 = app.as_weak();
                                let failures6 = failures5.clone();
                                after(400, move || {
                                    let app = app6.upgrade().unwrap();
                                    back_with_repeat(&app);
                                    fail(
                                        &failures6,
                                        app.get_home_view() == 0,
                                        "Back in a Home subpage must return to the landing",
                                    );
                                    fail(
                                        &failures6,
                                        *background_count.borrow() == 0,
                                        "a held Back in a Home subpage must not background the app",
                                    );

                                    let app6b = app.as_weak();
                                    let failures6b = failures6.clone();
                                    after(200, move || {
                                        let app = app6b.upgrade().unwrap();
                                        back(&app);
                                        fail(
                                            &failures6b,
                                            *background_count.borrow() == 1,
                                            "Back on the root screen must background the app",
                                        );

                                        // Deep-linked detail streams (Home Continue
                                        // opens the resume episode's streams without
                                        // visiting the episode list): Back must close
                                        // the modal instead of revealing the skipped
                                        // list. Setup and dispatch stay on separate
                                        // ticks so the recreated page owns focus.
                                        app.set_modal_visible(true);
                                        app.set_modal_episodes(false);
                                        app.set_episode_context(SharedString::from("S1 E1"));
                                        app.set_detail_deep_stream(true);
                                        app.set_categories_modal(true);
                                        let app7 = app.as_weak();
                                        let failures7 = failures6b.clone();
                                        after(400, move || {
                                            let app = app7.upgrade().unwrap();
                                            back(&app);
                                            fail(
                                                &failures7,
                                                !app.get_categories_modal() && app.get_modal_visible(),
                                                "Back in the category picker must dismiss only the picker",
                                            );
                                            back(&app);
                                            fail(
                                                &failures7,
                                                !app.get_modal_visible(),
                                                "Back in deep-linked streams must close modal",
                                            );
                                            fail(
                                                &failures7,
                                                !app.get_modal_episodes(),
                                                "Back in deep-linked streams must not show episodes",
                                            );
                                            app.set_show_home(false);

                                            // A focused search field must first
                                            // yield focus; it must not swallow
                                            // Back or send the user out of Discover.
                                            let app8 = app.as_weak();
                                            let failures8 = failures7.clone();
                                            let went_home8 = went_home.clone();
                                            after(350, move || {
                                                let app = app8.upgrade().unwrap();
                                                let Some(search) = ElementHandle::find_by_element_type_name(
                                                    &app,
                                                    "SearchField",
                                                )
                                                .next() else {
                                                    fail(&failures8, false, "Discover search field must be present");
                                                    slint::quit_event_loop().unwrap();
                                                    return;
                                                };
                                                let app9 = app.as_weak();
                                                let failures9 = failures8.clone();
                                                slint::spawn_local(async move {
                                                    search
                                                        .single_click(slint::platform::PointerEventButton::Left)
                                                        .await;
                                                    after(100, move || {
                                                        let app = app9.upgrade().unwrap();
                                                        fail(
                                                            &failures9,
                                                            app.get_discover_search_focused(),
                                                            "tapping the search field must focus it",
                                                        );
                                                        *went_home8.borrow_mut() = false;
                                                        back(&app);
                                                        fail(
                                                            &failures9,
                                                            !app.get_discover_search_focused(),
                                                            "Back with search focused must return focus to Discover",
                                                        );
                                                        fail(
                                                            &failures9,
                                                            !*went_home8.borrow(),
                                                            "Back with search focused must not leave Discover",
                                                        );
                                                        back(&app);
                                                        fail(
                                                            &failures9,
                                                            *went_home8.borrow(),
                                                            "the next Back with search unfocused must leave Discover",
                                                        );
                                                        *went_home8.borrow_mut() = false;
                                                        app.set_type_names(
                                                            Rc::new(VecModel::from(vec![
                                                                SharedString::from("Movie"),
                                                                SharedString::from("Series"),
                                                            ]))
                                                            .into(),
                                                        );
                                                        let Some(dropdown) = ElementHandle::find_by_element_type_name(
                                                            &app,
                                                            "Dropdown",
                                                        )
                                                        .next() else {
                                                            fail(&failures9, false, "Discover type dropdown must be present");
                                                            slint::quit_event_loop().unwrap();
                                                            return;
                                                        };
                                                        let text_count_before = ElementHandle::find_by_element_type_name(
                                                            &app,
                                                            "Text",
                                                        )
                                                        .count();
                                                        let back_request_before = app.get_system_back_request();
                                                        let app10 = app.as_weak();
                                                        let failures10 = failures9.clone();
                                                        slint::spawn_local(async move {
                                                            dropdown
                                                                .single_click(slint::platform::PointerEventButton::Left)
                                                                .await;
                                                            after(100, move || {
                                                                let app = app10.upgrade().unwrap();
                                                                let popup_text_count = ElementHandle::find_by_element_type_name(
                                                                    &app,
                                                                    "Text",
                                                                )
                                                                .count();
                                                                fail(
                                                                    &failures10,
                                                                    popup_text_count > text_count_before,
                                                                    "opening the type dropdown must show its popup rows",
                                                                );
                                                                back(&app);
                                                                fail(
                                                                    &failures10,
                                                                    app.get_system_back_request() == back_request_before,
                                                                    "Back in a dropdown popup must not route to the underlying page",
                                                                );
                                                                fail(
                                                                    &failures10,
                                                                    !*went_home8.borrow(),
                                                                    "Back in a dropdown popup must not leave Discover",
                                                                );
                                                                after(100, move || {
                                                                    let app = app10.upgrade().unwrap();
                                                                    fail(
                                                                        &failures10,
                                                                        ElementHandle::find_by_element_type_name(
                                                                            &app,
                                                                            "Text",
                                                                        )
                                                                        .count()
                                                                            == text_count_before,
                                                                        "Back must close the dropdown popup",
                                                                    );
                                                                    slint::quit_event_loop().unwrap();
                                                                });
                                                            });
                                                        })
                                                        .unwrap();
                                                    });
                                                })
                                                .unwrap();
                                            });
                                        });
                                    });
                                });
                            });
                        });
                    });
                })
                .unwrap();
            });
        });
    });

    slint::run_event_loop().unwrap();

    let failures = failures.borrow();
    assert!(
        failures.is_empty(),
        "back-nav failures:\n  {}",
        failures.join("\n  ")
    );
}
