//! Oniko: a desktop chat for coding agents over ACP, in Rust on gpui.

mod assets;
mod icons;
mod markdown;
mod notify;
mod state;
mod theme;
mod ui;
mod views;
mod workspace;

use std::sync::{Arc, OnceLock};

use gpui::{
    App, Application, Bounds, KeyBinding, Menu, MenuItem, TitlebarOptions, WindowBounds, WindowOptions, actions, point, prelude::*, px, size,
};
use oniko_core::core::{StartOptions, start};

use crate::state::{GlobalStore, Store};
use crate::theme::{Theme, sync_theme};
use crate::workspace::Workspace;

actions!(oniko, [Quit]);

fn main() {
    let handle_slot: Arc<OnceLock<oniko_core::core::CoreHandle>> = Arc::default();
    let options = StartOptions::from_env(env!("CARGO_PKG_VERSION"), notify::notifier(handle_slot.clone()));
    let (core, events) = match start(options) {
        Ok(started) => started,
        Err(error) => {
            eprintln!("oniko: could not start: {error:#}");
            std::process::exit(1);
        }
    };
    let _ = handle_slot.set(core.clone());

    Application::new().with_assets(assets::Assets).run(move |cx: &mut App| {
        cx.set_global(Theme::light());
        ui::text_field::bind_keys(cx);
        workspace::bind_keys(cx);
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        cx.on_action(|_: &Quit, cx| quit(cx));
        cx.set_menus(vec![Menu {
            name: "Oniko".into(),
            items: vec![
                MenuItem::action("New Thread", workspace::NewThread),
                MenuItem::action("Settings…", workspace::OpenSettings),
                MenuItem::separator(),
                MenuItem::action("Quit Oniko", Quit),
            ],
        }]);

        let store = cx.new(|cx| Store::new(core.clone(), events, cx));
        cx.set_global(GlobalStore(store));

        let bounds = Bounds::centered(None, size(px(1280.), px(840.)), cx);
        let window = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(760.), px(520.))),
                titlebar: Some(TitlebarOptions { title: Some("Oniko".into()), appears_transparent: true, traffic_light_position: Some(point(px(14.), px(14.))) }),
                app_id: Some("oniko".into()),
                ..Default::default()
            },
            |window, cx| {
                sync_theme(window, cx);
                cx.new(|cx| Workspace::new(window, cx))
            },
        );
        if let Err(error) = window {
            eprintln!("oniko: could not open the window: {error:#}");
            cx.quit();
        }
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                quit(cx);
            }
        })
        .detach();
        cx.activate(true);
    });
}

/// Stops every agent and lets telemetry flush before the process exits.
fn quit(cx: &mut App) {
    let shutdown = state::core(cx).shutdown();
    cx.spawn(async move |cx| {
        shutdown.await;
        let _ = cx.update(|cx| cx.quit());
    })
    .detach();
}
