//! Subscriptions and privacy.

use gpui::{AnyElement, App, ClipboardItem, Context, Entity, EventEmitter, FontWeight, IntoElement, Window, div, prelude::*, px};
use oniko_core::model::{AccountStatus, Subscription, SubscriptionId, plan_label};
use oniko_core::telemetry::TelemetryState;

use crate::icons::{Icon, icon};
use crate::state::{run, run_then, store};
use crate::theme::theme;
use crate::ui::text_field::{TextField, TextFieldEvent};
use crate::ui::{ButtonSize, Variant, badge, button, code, field_frame, icon_button, status_dot, switch, tooltip_view};
use crate::views::thread::login_command;
use crate::views::{Nav, account_color, relative_time, shorten_home};

pub struct SettingsView {
    selected: Option<SubscriptionId>,
    label: Entity<TextField>,
    /// The subscription the label field was filled from.
    label_for: Option<SubscriptionId>,
    reveal_email: bool,
    telemetry: Option<TelemetryState>,
}

impl EventEmitter<Nav> for SettingsView {}

impl SettingsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> SettingsView {
        let label = cx.new(|cx| TextField::new(window, cx).placeholder("Subscription name"));
        cx.subscribe_in(&label, window, |this, field, event, window, cx| match event {
            TextFieldEvent::Submit | TextFieldEvent::Blurred => this.commit_label(cx),
            TextFieldEvent::Escape => {
                this.label_for = None;
                window.blur();
                cx.notify();
            }
            TextFieldEvent::Changed => {
                let _ = field;
                cx.notify();
            }
            TextFieldEvent::Focused => {}
        })
        .detach();
        cx.observe(&store(cx), |_, _, cx| cx.notify()).detach();
        let this = cx.entity().downgrade();
        run_then(cx, |c| c.telemetry(), move |state, cx| {
            let _ = this.update(cx, |this, cx| {
                this.telemetry = Some(state);
                cx.notify();
            });
        });
        SettingsView { selected: None, label, label_for: None, reveal_email: false, telemetry: None }
    }

    fn selected<'a>(&self, cx: &'a App) -> Option<&'a Subscription> {
        let subs = &store(cx).read(cx).state.subscriptions;
        // A removed or not yet chosen selection falls back to the active row, then the first.
        self.selected.as_deref().and_then(|id| subs.get(id)).or(subs.active()).or(subs.subscriptions.first())
    }

    fn commit_label(&mut self, cx: &mut Context<Self>) {
        let Some(sub) = self.selected(cx).cloned() else { return };
        let draft = self.label.read(cx).text().trim().to_owned();
        if !draft.is_empty() && draft != sub.label {
            run(cx, |c| c.rename_subscription(sub.id.clone(), draft));
        } else if draft.is_empty() {
            self.label_for = None;
        }
        cx.notify();
    }

    fn account_summary(&self, account: &AccountStatus, with_email: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        match account {
            AccountStatus::LoggedIn { email, org_name, plan, .. } => {
                let personal = email.as_ref().map(|e| format!("{e}'s Organization"));
                let org = org_name.clone().filter(|o| Some(o) != personal.as_ref());
                let extras: Vec<String> = [plan_label(plan.as_deref()), org].into_iter().flatten().collect();
                let show_email = with_email && email.is_some();
                let reveal = self.reveal_email;
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .child(if show_email { "Authenticated as" } else { "Authenticated" })
                    .when(show_email, |d| {
                        // Hidden until clicked, so a screenshot or a screen share does not leak the address.
                        let shown = if reveal { email.clone().unwrap_or_default() } else { "••••••••@••••".to_owned() };
                        d.child(
                            div()
                                .id("reveal-email")
                                .cursor_pointer()
                                .font_family(crate::theme::MONO)
                                .text_color(if reveal { t.foreground } else { t.muted_foreground })
                                .tooltip(move |_, cx| tooltip_view(if reveal { "Click to hide email" } else { "Click to reveal email" }.into(), cx))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.reveal_email = !this.reveal_email;
                                    cx.notify();
                                }))
                                .child(shown),
                        )
                    })
                    .children(extras.into_iter().map(|part| div().child(format!("· {part}"))))
                    .into_any_element()
            }
            other => {
                let headline = match other {
                    AccountStatus::Unknown => "Not checked yet".to_owned(),
                    AccountStatus::Checking => "Checking login…".to_owned(),
                    AccountStatus::LoggedOut { .. } => "Logged out. Sign in to use it.".to_owned(),
                    AccountStatus::Error { message, .. } => message.clone(),
                    AccountStatus::LoggedIn { .. } => String::new(),
                };
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .min_w_0()
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .when(matches!(other, AccountStatus::LoggedOut { .. } | AccountStatus::Error { .. }), |d| d.child(status_dot(account_color(other, &t))))
                    .child(div().truncate().child(headline))
                    .into_any_element()
            }
        }
    }
}

fn section(title: impl IntoElement, action: Option<AnyElement>, body: impl IntoElement, cx: &App) -> impl IntoElement {
    let t = theme(cx);
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .min_h(px(28.))
                .child(div().flex().items_center().gap_2().text_sm().font_weight(FontWeight::SEMIBOLD).child(title))
                .children(action),
        )
        .child(div().flex().flex_col().w_full().overflow_hidden().rounded_xl().border_1().border_color(t.border).bg(t.background).child(body))
}

fn row(title: &str, description: Option<String>, status: Option<AnyElement>, control: Option<AnyElement>, cx: &App) -> impl IntoElement {
    let t = theme(cx);
    div()
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .px_4()
        .py_3()
        .border_b_1()
        .border_color(t.border)
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .flex_1()
                .min_w_0()
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title.to_owned()))
                .when_some(description, |d, text| d.child(div().text_xs().text_color(t.muted_foreground).child(text)))
                .children(status),
        )
        .children(control.map(|c| div().flex_none().child(c)))
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let home = store(cx).read(cx).home.clone();
        let subscriptions = store(cx).read(cx).state.subscriptions.clone();
        let subs = subscriptions.subscriptions.clone();
        let active_id = subscriptions.active_subscription_id.clone();
        let selected = self.selected(cx).cloned();
        // Fill the name field from the selected subscription unless it is being edited.
        if let Some(sub) = &selected
            && !self.label.read(cx).is_focused(window)
            && (self.label_for.as_ref() != Some(&sub.id) || self.label.read(cx).text() != sub.label)
        {
            self.label.update(cx, |field, cx| field.set_text(sub.label.clone(), cx));
            self.label_for = Some(sub.id.clone());
        }

        let refreshing = subs.iter().any(|s| s.account == AccountStatus::Checking);
        let last = subs
            .iter()
            .filter_map(|s| match &s.account {
                AccountStatus::LoggedIn { checked_at, .. } | AccountStatus::LoggedOut { checked_at } | AccountStatus::Error { checked_at, .. } => Some(*checked_at),
                _ => None,
            })
            .max();
        let refresh_label = if refreshing {
            "Checking logins".to_owned()
        } else {
            match last.map(|at| relative_time(at, oniko_core::util::now_ms())) {
                None => "Check logins".into(),
                Some(since) if since == "now" => "Checked just now".into(),
                Some(since) => format!("Checked {since} ago"),
            }
        };
        let actions = div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                button("refresh-all", refresh_label)
                    .icon(Icon::RefreshCw)
                    .size(ButtonSize::Xs)
                    .muted()
                    .disabled(refreshing || subs.is_empty())
                    .on_click(|_, _, cx| run(cx, |c| c.refresh_subscriptions(None))),
            )
            .child(icon_button("add-subscription", Icon::Plus).muted().tooltip("Add subscription").on_click(cx.listener(|_, _, _, cx| cx.emit(Nav::AddSubscription))))
            .into_any_element();

        let list = div().flex().flex_col().w(px(260.)).flex_none().border_r_1().border_color(t.border).p_1().children(subs.iter().map(|sub| {
            let id = sub.id.clone();
            let is_selected = selected.as_ref().map(|s| &s.id) == Some(&sub.id);
            div()
                .id(gpui::SharedString::from(format!("sub-{}", sub.id)))
                .flex()
                .items_center()
                .gap_2()
                .p_2()
                .rounded_lg()
                .cursor_pointer()
                .when(is_selected, |d| d.bg(t.accent))
                .hover(|s| s.bg(t.accent))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.selected = Some(id.clone());
                    this.label_for = None;
                    cx.notify();
                }))
                .child(icon(Icon::Claude, t.foreground))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).child(sub.label.clone()))
                        .child(div().truncate().text_xs().text_color(t.muted_foreground).font_family(crate::theme::MONO).child(shorten_home(&sub.config_dir, &home)))
                        .child(self.account_summary(&sub.account, false, cx)),
                )
                .when(Some(&sub.id) == active_id.as_ref(), |d| d.child(badge("Active").filled()))
        }));

        let editor = selected.as_ref().map(|sub| {
            let used_by = store(cx).read(cx).state.threads.values().filter(|th| th.subscription_id == sub.id).count();
            let dir = shorten_home(&sub.config_dir, &home);
            let checking = sub.account == AccountStatus::Checking;
            let remove_reason = (used_by > 0).then(|| format!("Used by {used_by} thread{}. Remove those threads first, or keep it.", if used_by == 1 { "" } else { "s" }));
            let (refresh_id, remove_id, active_id_for) = (sub.id.clone(), sub.id.clone(), sub.id.clone());
            let folder = sub.config_dir.clone();
            let is_active = Some(&sub.id) == active_id.as_ref();
            let label_focused = self.label.read(cx).is_focused(window);
            let header_action = div()
                .flex()
                .items_center()
                .gap_1()
                .child(code(dir.clone(), cx))
                .child(
                    icon_button("refresh-sub", Icon::RefreshCw)
                        .muted()
                        .disabled(checking)
                        .tooltip("Refresh login status")
                        .on_click(move |_, _, cx| run(cx, |c| c.refresh_subscriptions(Some(refresh_id.clone())))),
                )
                .child(
                    icon_button("remove-sub", Icon::Trash)
                        .muted()
                        .disabled(remove_reason.is_some())
                        .tooltip(remove_reason.clone().unwrap_or_else(|| "Remove subscription".into()))
                        .on_click(move |_, _, cx| run(cx, |c| c.remove_subscription(remove_id.clone()))),
                )
                .into_any_element();
            let body = div()
                .flex()
                .flex_col()
                .child(row(
                    "Display name",
                    None,
                    Some(self.account_summary(&sub.account, true, cx)),
                    Some(div().w(px(220.)).child(field_frame(label_focused, false, cx).child(self.label.clone())).into_any_element()),
                    cx,
                ))
                .child(row(
                    "New threads",
                    Some("New threads start on the active subscription. A thread stays on the one it started with.".into()),
                    None,
                    Some(if is_active {
                        div().flex().items_center().gap_1().text_xs().text_color(t.muted_foreground).child(icon(Icon::Check, t.muted_foreground)).child("Active").into_any_element()
                    } else {
                        button("make-active", "Make active")
                            .variant(Variant::Outline)
                            .size(ButtonSize::Xs)
                            .on_click(move |_, _, cx| run(cx, |c| c.set_active_subscription(active_id_for.clone())))
                            .into_any_element()
                    }),
                    cx,
                ))
                .child(row(
                    "Config folder",
                    Some("Holds this subscription's login and its sessions, so threads resume only here.".into()),
                    None,
                    Some(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(code(dir, cx))
                            .child(icon_button("copy-folder", Icon::Copy).muted().tooltip("Copy folder path").on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(folder.clone()))
                            }))
                            .into_any_element(),
                    ),
                    cx,
                ))
                .child(row(
                    "Threads",
                    Some(if used_by > 0 { format!("{used_by} thread{} run on this subscription.", if used_by == 1 { "" } else { "s" }) } else { "No threads use it yet.".into() }),
                    None,
                    None,
                    cx,
                ));
            let sign_in = matches!(sub.account, AccountStatus::LoggedOut { .. }).then(|| {
                section(
                    "Sign in",
                    None,
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .px_4()
                        .py_3()
                        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Log in from a terminal"))
                        .child(div().text_xs().text_color(t.muted_foreground).child("The app does not run the login flow itself. Run this for the folder, then refresh."))
                        .child(login_command(sub, cx)),
                    cx,
                )
            });
            div()
                .flex()
                .flex_col()
                .gap_6()
                .flex_1()
                .min_w_0()
                .p_4()
                .child(section(
                    div().flex().items_center().gap_2().child(icon(Icon::Claude, t.foreground)).child(sub.label.clone()),
                    Some(header_action),
                    body,
                    cx,
                ))
                .children(sign_in)
        });

        let subscriptions_body = if subs.is_empty() {
            div()
                .flex()
                .flex_col()
                .items_start()
                .gap_3()
                .p_4()
                .child(div().text_sm().text_color(t.muted_foreground).child("No subscriptions yet. Add a Claude config folder to start a thread."))
                .child(button("add-first", "Add subscription").icon(Icon::Plus).variant(Variant::Outline).on_click(cx.listener(|_, _, _, cx| cx.emit(Nav::AddSubscription))))
                .into_any_element()
        } else {
            div().flex().w_full().min_h(px(320.)).child(list).children(editor).into_any_element()
        };

        let telemetry = self.telemetry;
        let available = telemetry.is_some_and(|s| s.available);
        let this = cx.entity();
        let privacy = row(
            "Share anonymous usage data and crash reports",
            Some(if telemetry.is_some() && !available {
                "Not available in this build.".into()
            } else {
                "Which features get used, error reports and basic hardware info. Never what you write or the code you work on.".into()
            }),
            None,
            Some(
                switch("telemetry-toggle", telemetry.is_some_and(|s| s.enabled), !available, cx, move |enabled, _, cx| {
                    run(cx, |c| c.set_telemetry_enabled(enabled));
                    this.update(cx, |this, cx| {
                        if let Some(state) = this.telemetry.as_mut() {
                            state.enabled = enabled;
                        }
                        cx.notify();
                    });
                })
                .into_any_element(),
            ),
            cx,
        );

        div().id("settings").size_full().overflow_y_scroll().child(
            div()
                .w_full()
                .max_w(px(960.))
                .mx_auto()
                .p_8()
                .flex()
                .flex_col()
                .gap_8()
                .child(section("Subscriptions", Some(actions), subscriptions_body, cx))
                .child(section("Privacy", None, privacy, cx)),
        )
    }
}
