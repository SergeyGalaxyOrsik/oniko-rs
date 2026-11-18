//! The two dialogs: adding a subscription and creating a task.

use gpui::{App, AsyncApp, Context, Entity, EventEmitter, FontWeight, IntoElement, PathPromptOptions, WeakEntity, Window, div, prelude::*, px};
use oniko_core::model::{AgentId, SubscriptionId, TaskStatus, TaskStatusId, account_line, project_key};

use crate::icons::{Icon, icon};
use crate::state::{core, run_then, store};
use crate::theme::theme;
use crate::ui::text_field::{EnterKey, TextField, TextFieldEvent};
use crate::ui::{Variant, badge, button, field_frame, modal};
use crate::views::tasks::status_menu;
use crate::views::thread::login_hint;
use crate::views::{account_color, account_label, shorten_home, status_icon};

pub enum DialogEvent {
    Close,
}

enum Phase {
    Editing { error: Option<String> },
    Adding,
    Added(SubscriptionId),
}

pub struct AddSubscriptionDialog {
    path: Entity<TextField>,
    label: Entity<TextField>,
    suggestions: Vec<String>,
    phase: Phase,
}

impl EventEmitter<DialogEvent> for AddSubscriptionDialog {}

impl AddSubscriptionDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> AddSubscriptionDialog {
        let path = cx.new(|cx| TextField::new(window, cx).placeholder("~/.claude-work"));
        let label = cx.new(|cx| TextField::new(window, cx).placeholder("Defaults to the folder name"));
        for field in [&path, &label] {
            cx.subscribe_in(field, window, |this, _, event, _, cx| match event {
                TextFieldEvent::Submit => this.submit(cx),
                TextFieldEvent::Escape => cx.emit(DialogEvent::Close),
                _ => cx.notify(),
            })
            .detach();
        }
        path.read(cx).focus(window);
        cx.observe(&store(cx), |_, _, cx| cx.notify()).detach();
        let this = cx.entity().downgrade();
        run_then(cx, |c| c.detect_subscriptions(), move |suggestions, cx| {
            let _ = this.update(cx, |this, cx| {
                this.suggestions = suggestions;
                cx.notify();
            });
        });
        AddSubscriptionDialog { path, label, suggestions: vec![], phase: Phase::Editing { error: None } }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.phase, Phase::Editing { .. }) {
            return;
        }
        let path = self.path.read(cx).text().trim().to_owned();
        if path.is_empty() {
            self.phase = Phase::Editing { error: Some("Enter the folder the subscription lives in".into()) };
            return cx.notify();
        }
        let label = Some(self.label.read(cx).text().trim().to_owned()).filter(|l| !l.is_empty());
        self.phase = Phase::Adding;
        cx.notify();
        let added = core(cx).add_subscription(AgentId::Claude, path, label);
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = added.await;
            let _ = this.update(cx, |this, cx| {
                this.phase = match result {
                    Ok(id) => Phase::Added(id),
                    Err(error) => Phase::Editing { error: Some(error) },
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn browse(&mut self, cx: &mut Context<Self>) {
        let home = store(cx).read(cx).home.clone();
        let paths = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Choose".into()) });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let _ = this.update(cx, |this, cx| this.path.update(cx, |f, cx| f.set_text(shorten_home(&path.to_string_lossy(), &home), cx)));
        })
        .detach();
    }
}

impl Render for AddSubscriptionDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let home = store(cx).read(cx).home.clone();
        let close = cx.entity().downgrade();
        let dismiss = move |_: &mut Window, cx: &mut App| {
            let _ = close.update(cx, |_, cx| cx.emit(DialogEvent::Close));
        };
        let added = match &self.phase {
            Phase::Added(id) => store(cx).read(cx).state.subscriptions.get(id).cloned(),
            _ => None,
        };
        let header = |title: &str, description: &str| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(title.to_owned()))
                .child(div().text_sm().text_color(t.muted_foreground).child(description.to_owned()))
        };
        let body = if let Some(sub) = added {
            div()
                .flex()
                .flex_col()
                .gap_4()
                .p_6()
                .child(header("Subscription added", "New threads can start on it now."))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .p_3()
                        .rounded_lg()
                        .border_1()
                        .border_color(t.border)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(div().font_weight(FontWeight::MEDIUM).child(sub.label.clone()))
                                .child(badge(account_label(&sub.account)).dot(account_color(&sub.account, &t))),
                        )
                        .child(div().text_xs().text_color(t.muted_foreground).child(shorten_home(&sub.config_dir, &home)))
                        .children(account_line(&sub.account).map(|line| div().text_xs().text_color(t.muted_foreground).child(line))),
                )
                .when(matches!(sub.account, oniko_core::model::AccountStatus::LoggedOut { .. }), |d| d.child(login_hint(&sub, cx)))
                .child(
                    div().flex().justify_end().child(
                        button("add-subscription-done", "Done").variant(Variant::Primary).on_click(cx.listener(|_, _, _, cx| cx.emit(DialogEvent::Close))),
                    ),
                )
                .into_any_element()
        } else {
            let error = match &self.phase {
                Phase::Editing { error } => error.clone(),
                _ => None,
            };
            let adding = matches!(self.phase, Phase::Adding);
            let path_focused = self.path.read(cx).is_focused(window);
            let label_focused = self.label.read(cx).is_focused(window);
            let suggestions = self.suggestions.iter().enumerate().map(|(i, dir)| {
                let shown = shorten_home(dir, &home);
                let fill = shown.clone();
                div()
                    .id(("suggestion", i))
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .border_1()
                    .border_color(t.border)
                    .font_family(crate::theme::MONO)
                    .text_xs()
                    .cursor_pointer()
                    .hover(|s| s.bg(t.accent))
                    .on_click(cx.listener(move |this, _, _, cx| this.path.update(cx, |f, cx| f.set_text(fill.clone(), cx))))
                    .child(shown)
            });
            div()
                .flex()
                .flex_col()
                .gap_4()
                .p_6()
                .child(header("Add a subscription", "Point to a Claude config folder, like ~/.claude or ~/.claude-work. Each folder keeps its own login."))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Folder"))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(div().flex_1().child(field_frame(path_focused, error.is_some(), cx).child(self.path.clone())))
                                .child(button("browse", "Browse…").icon(Icon::FolderOpen).variant(Variant::Outline).on_click(cx.listener(|this, _, _, cx| this.browse(cx)))),
                        )
                        .when(!self.suggestions.is_empty(), |d| {
                            d.child(div().flex().flex_wrap().items_center().gap_1p5().text_xs().text_color(t.muted_foreground).child("Detected:").children(suggestions))
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .child(div().flex().gap_1().text_sm().font_weight(FontWeight::MEDIUM).child("Name").child(div().text_color(t.muted_foreground).font_weight(FontWeight::NORMAL).child("(optional)")))
                        .child(field_frame(label_focused, false, cx).child(self.label.clone())),
                )
                .children(error.map(|e| div().text_sm().text_color(t.destructive).child(e)))
                .child(
                    div().flex().justify_end().child(
                        button("add-subscription-submit", if adding { "Checking login…" } else { "Add subscription" })
                            .variant(Variant::Primary)
                            .disabled(adding)
                            .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                    ),
                )
                .into_any_element()
        };
        modal("add-subscription-dialog", 520., body, dismiss, cx)
    }
}

pub struct NewTaskDialog {
    project: String,
    status: TaskStatusId,
    title: Entity<TextField>,
    description: Entity<TextField>,
    status_open: bool,
    saving: bool,
}

impl EventEmitter<DialogEvent> for NewTaskDialog {}

impl NewTaskDialog {
    pub fn new(project: String, status: TaskStatusId, window: &mut Window, cx: &mut Context<Self>) -> NewTaskDialog {
        let title = cx.new(|cx| TextField::new(window, cx).placeholder("Task title"));
        let description = cx.new(|cx| TextField::new(window, cx).multiline(4, 14, EnterKey::Breaks).placeholder("Add description…"));
        for field in [&title, &description] {
            cx.subscribe_in(field, window, |this, _, event, _, cx| match event {
                TextFieldEvent::Submit => this.submit(cx),
                TextFieldEvent::Escape => cx.emit(DialogEvent::Close),
                _ => cx.notify(),
            })
            .detach();
        }
        title.read(cx).focus(window);
        NewTaskDialog { project, status, title, description, status_open: false, saving: false }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let title = self.title.read(cx).text().trim().to_owned();
        if title.is_empty() || self.saving {
            return;
        }
        self.saving = true;
        let description = self.description.read(cx).text().to_owned();
        let this = cx.entity().downgrade();
        let create = core(cx).create_task(self.project.clone(), title, description, self.status.clone());
        cx.spawn(async move |_, cx: &mut AsyncApp| {
            let result = create.await;
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                match result {
                    Ok(_) => cx.emit(DialogEvent::Close),
                    Err(error) => store(cx).update(cx, |s, cx| s.report(error, cx)),
                }
            });
        })
        .detach();
    }
}

impl Render for NewTaskDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let statuses: Vec<TaskStatus> = store(cx).read(cx).state.tasks.project_statuses(&self.project).into_iter().cloned().collect();
        let status = statuses.iter().find(|s| s.id == self.status).cloned();
        let close = cx.entity().downgrade();
        let dismiss = move |_: &mut Window, cx: &mut App| {
            let _ = close.update(cx, |_, cx| cx.emit(DialogEvent::Close));
        };
        let entity = cx.entity();
        let pick = entity.clone();
        let status_picker = status.map(|status| {
            status_menu(
                "new-task-status",
                self.status_open,
                statuses.clone(),
                self.status.clone(),
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .h(px(26.))
                    .px_2()
                    .rounded_md()
                    .border_1()
                    .border_color(t.border)
                    .text_xs()
                    .hover(|s| s.bg(t.accent))
                    .child(status_icon(&status, cx))
                    .child(status.name.clone()),
                move |id, _, cx| pick.update(cx, |this, cx| {
                    this.status = id;
                    cx.notify();
                }),
                move |open, _, cx| entity.update(cx, |this, cx| {
                    this.status_open = open;
                    cx.notify();
                }),
                cx,
            )
        });
        let can_create = !self.title.read(cx).text().trim().is_empty() && !self.saving;
        let body = div()
            .flex()
            .flex_col()
            .gap_3()
            .p_5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .text_sm()
                    .child(badge(project_key(&self.project)))
                    .child(icon(Icon::ChevronRight, t.muted_foreground).size(px(14.)))
                    .child(div().font_weight(FontWeight::MEDIUM).child("New task")),
            )
            .child(div().text_lg().font_weight(FontWeight::MEDIUM).child(self.title.clone()))
            .child(div().text_sm().child(self.description.clone()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pt_2()
                    .border_t_1()
                    .border_color(t.border)
                    .children(status_picker)
                    .child(div().flex_1())
                    .child(div().text_xs().text_color(t.muted_foreground).child("⌘↵ to create"))
                    .child(
                        button("task-save", "Create task")
                            .variant(Variant::Primary)
                            .disabled(!can_create)
                            .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                    ),
            );
        modal("new-task-dialog", 640., body, dismiss, cx)
    }
}
