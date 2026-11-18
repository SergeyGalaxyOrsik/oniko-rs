//! "What should we build in <folder>?": picks the project, then the first prompt starts a thread.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::{AnyElement, App, AsyncApp, Context, Corner, Entity, EventEmitter, FontWeight, IntoElement, PathPromptOptions, SharedString, WeakEntity, Window, div, prelude::*, px};
use oniko_core::model::ConfigControl;
use oniko_core::model::config::{new_thread_config, with_value};

use crate::icons::{Icon, icon};
use crate::state::{run, run_then, store};
use crate::theme::theme;
use crate::ui::text_field::{EnterKey, TextField, TextFieldEvent};
use crate::ui::{MenuEntry, Variant, button, dropdown, item};
use crate::views::composer::{Composer, subscription_chip};
use crate::views::{Nav, basename};

pub struct NewThreadView {
    composer: Entity<TextField>,
    cwd: Option<String>,
    recent: Vec<String>,
    /// Picks made before the session exists; the template's own values fill whatever was not touched.
    picked: HashMap<String, String>,
    open_menu: Option<SharedString>,
}

impl EventEmitter<Nav> for NewThreadView {}

impl NewThreadView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> NewThreadView {
        let composer = cx.new(|cx| TextField::new(window, cx).multiline(2, 10, EnterKey::Submits).placeholder("Ask for changes, send follow-ups…"));
        cx.subscribe_in(&composer, window, |this, _, event, window, cx| match event {
            TextFieldEvent::Submit => this.send(window, cx),
            TextFieldEvent::Changed => cx.notify(),
            _ => {}
        })
        .detach();
        cx.observe(&store(cx), |_, _, cx| cx.notify()).detach();
        let mut view = NewThreadView { composer, cwd: None, recent: vec![], picked: HashMap::new(), open_menu: None };
        view.reload_recent(cx);
        view
    }

    pub fn reload_recent(&mut self, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        run_then(cx, |c| c.recent_folders(), move |recent, cx| {
            let _ = this.update(cx, |this, cx| {
                if this.cwd.is_none() {
                    this.cwd = recent.first().cloned();
                }
                this.recent = recent;
                cx.notify();
            });
        });
    }

    pub fn focus(&self, window: &mut Window, cx: &App) {
        self.composer.read(cx).focus(window);
    }

    fn config(&self, cx: &App) -> Vec<ConfigControl> {
        let state = &store(cx).read(cx).state;
        let Some(active) = state.subscriptions.active() else { return vec![] };
        let template = new_thread_config(state.threads.values(), active.agent_id, &active.id);
        template.iter().fold(template.clone(), |acc, control| match self.picked.get(&control.id) {
            Some(value) if control.options.iter().any(|o| &o.value == value) => with_value(&acc, &control.id, value),
            _ => acc,
        })
    }

    fn send(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).text().trim().to_owned();
        let Some(cwd) = self.cwd.clone() else { return };
        let Some(active) = store(cx).read(cx).state.subscriptions.active().map(|s| s.id.clone()) else { return };
        if text.is_empty() {
            return;
        }
        let values: Vec<(String, String)> = self.config(cx).into_iter().map(|c| (c.id, c.value)).collect();
        self.composer.update(cx, |field, cx| field.clear(cx));
        let this = cx.entity().downgrade();
        run_then(cx, |c| c.create_thread(cwd, Some(active), values), move |id, cx| {
            run(cx, |c| c.prompt(id.clone(), text));
            let _ = this.update(cx, |this, cx| {
                this.picked.clear();
                this.reload_recent(cx);
                cx.emit(Nav::OpenThread(id));
            });
        });
    }

    fn choose_folder(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Open project".into()) });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let path = path.to_string_lossy().into_owned();
            let _ = this.update(cx, |this, cx| {
                run(cx, |c| c.remember_folder(path.clone()));
                this.cwd = Some(path);
                cx.notify();
            });
        })
        .detach();
    }

    fn set_menu(&mut self, menu: Option<SharedString>, cx: &mut Context<Self>) {
        self.open_menu = menu;
        cx.notify();
    }

    fn folder_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        let label = self.cwd.as_deref().map_or("a project".to_owned(), basename);
        let recent = self.recent.clone();
        let entity = cx.entity();
        let pick = {
            let entity = entity.clone();
            Rc::new(move |path: String, cx: &mut App| entity.update(cx, |this, cx| {
                this.cwd = Some(path);
                cx.notify();
            }))
        };
        let choose = entity.clone();
        dropdown(
            "folder-picker",
            crate::ui::is_open(&self.open_menu, "folder"),
            Corner::TopLeft,
            div()
                .flex()
                .items_center()
                .gap_1()
                .px_1()
                .rounded_md()
                .text_color(t.foreground)
                .hover(|s| s.bg(t.accent))
                .child(div().border_b_1().border_dashed().border_color(t.muted_foreground).child(label))
                .child(icon(Icon::ChevronDown, t.muted_foreground).size(px(20.))),
            move || {
                let mut rows: Vec<MenuEntry> = recent
                    .iter()
                    .map(|path| {
                        let pick = pick.clone();
                        let path = path.clone();
                        item(basename(&path), {
                            let path = path.clone();
                            move |_, cx| pick(path.clone(), cx)
                        })
                        .detail(path)
                        .icon(Icon::Folder)
                        .entry()
                    })
                    .collect();
                if !rows.is_empty() {
                    rows.push(MenuEntry::Separator);
                }
                rows.push(item("Choose folder…", move |_, cx| choose.update(cx, |this, cx| this.choose_folder(cx))).icon(Icon::FolderOpen).entry());
                rows
            },
            move |open, _, cx| entity.update(cx, |this, cx| this.set_menu(open.then(|| "folder".into()), cx)),
            cx,
        )
    }
}

impl Render for NewThreadView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let state = &store(cx).read(cx).state;
        let subscriptions = state.subscriptions.subscriptions.clone();
        let active = state.subscriptions.active().cloned();
        let config = self.config(cx);
        let entity = cx.entity();
        let leading = {
            let set = entity.clone();
            subscription_chip(
                active.as_ref(),
                &subscriptions,
                false,
                crate::ui::is_open(&self.open_menu, "subscription"),
                move |open, _, cx| set.update(cx, |this, cx| this.set_menu(open.then(|| "subscription".into()), cx)),
                |id, _, cx| run(cx, |c| c.set_active_subscription(id)),
                cx,
            )
        };
        let set_menu = {
            let entity = entity.clone();
            Rc::new(move |menu: Option<SharedString>, _: &mut Window, cx: &mut App| entity.update(cx, |this, cx| this.set_menu(menu, cx)))
        };
        let on_config = {
            let entity = entity.clone();
            Rc::new(move |(id, value): (String, String), _: &mut Window, cx: &mut App| entity.update(cx, |this, cx| {
                this.picked.insert(id, value);
                cx.notify();
            }))
        };
        let on_send = {
            let entity = entity.clone();
            Rc::new(move |_: (), window: &mut Window, cx: &mut App| entity.update(cx, |this, cx| this.send(window, cx)))
        };
        let no_subscription = active.is_none().then(|| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .p_4()
                .rounded_2xl()
                .border_1()
                .border_color(t.border)
                .bg(t.muted.opacity(0.4))
                .text_sm()
                .child("Add a subscription in Settings to start a thread.")
                .child(
                    button("open-settings", "Open Settings")
                        .icon(Icon::Settings)
                        .variant(Variant::Outline)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(Nav::OpenSettings))),
                )
        });
        div()
            .flex()
            .flex_col()
            .flex_1()
            .size_full()
            .items_center()
            .justify_center()
            .px_6()
            .pb(px(96.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .justify_center()
                    .gap_1()
                    .text_3xl()
                    .font_weight(FontWeight::MEDIUM)
                    .child("What should we build in")
                    .child(self.folder_picker(cx))
                    .child("?"),
            )
            .child(
                div()
                    .mt_2()
                    .max_w(px(768.))
                    .truncate()
                    .text_sm()
                    .text_color(t.muted_foreground)
                    .child(self.cwd.clone().unwrap_or_else(|| "Pick a project folder to start".into())),
            )
            .child(
                div()
                    .mt_8()
                    .w_full()
                    .max_w(px(768.))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .children(no_subscription)
                    .child(Composer {
                        field: self.composer.clone(),
                        config,
                        busy: false,
                        disabled: self.cwd.is_none() || active.is_none(),
                        leading,
                        open_menu: self.open_menu.clone(),
                        set_menu,
                        on_config,
                        on_send,
                        on_stop: Rc::new(|_, _, _| {}),
                    }),
            )
    }
}
