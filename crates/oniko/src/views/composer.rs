//! The prompt box under a thread and on the new-thread screen.

use std::rc::Rc;

use gpui::{AnyElement, App, Corner, Entity, IntoElement, SharedString, Window, div, prelude::*, px};
use oniko_core::model::{ConfigControl, Subscription};

use crate::icons::{Icon, icon};
use crate::theme::theme;
use crate::ui::text_field::TextField;
use crate::ui::{ButtonSize, MenuEntry, Variant, dropdown, icon_button, item, tooltip_view};
use crate::views::{LOCKED_HINT, account_color};

/// Only these reach the composer; the rest (fast mode, sub-agents, ...) keep the agent's value.
const SHOWN_CATEGORIES: [&str; 3] = ["mode", "model", "thought_level"];
/// Values of these categories read on their own ("Opus 5.5", "Medium"); anything else needs its label.
const SELF_DESCRIBING: [&str; 3] = ["model", "mode", "thought_level"];

type Callback<T> = Rc<dyn Fn(T, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct Composer {
    pub field: Entity<TextField>,
    pub config: Vec<ConfigControl>,
    pub busy: bool,
    pub disabled: bool,
    pub leading: AnyElement,
    /// Which config menu is open, by control id.
    pub open_menu: Option<SharedString>,
    pub set_menu: Callback<Option<SharedString>>,
    pub on_config: Callback<(String, String)>,
    pub on_send: Callback<()>,
    pub on_stop: Callback<()>,
}

impl RenderOnce for Composer {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx);
        let focused = self.field.read(cx).is_focused(window);
        let empty = self.field.read(cx).text().trim().is_empty();
        let shown: Vec<ConfigControl> =
            self.config.into_iter().filter(|c| c.category.as_deref().is_some_and(|cat| SHOWN_CATEGORIES.contains(&cat))).collect();
        let controls = shown.into_iter().map(|control| {
            let open = crate::ui::is_open(&self.open_menu, control.id.as_str());
            let current = control.options.iter().find(|o| o.value == control.value).map(|o| o.label.clone()).unwrap_or(control.value.clone());
            let prefix = if control.category.as_deref().is_some_and(|c| SELF_DESCRIBING.contains(&c)) { String::new() } else { format!("{}: ", control.label) };
            let set_menu = self.set_menu.clone();
            let id: SharedString = control.id.clone().into();
            let trigger = div()
                .flex()
                .items_center()
                .gap_1()
                .h(px(28.))
                .px_2()
                .rounded_lg()
                .text_xs()
                .text_color(t.muted_foreground)
                .hover(|s| s.bg(t.accent).text_color(t.foreground))
                .when(self.disabled, |d| d.opacity(0.5))
                .child(format!("{prefix}{current}"))
                .child(icon(Icon::ChevronDown, t.muted_foreground).size(px(12.)));
            let on_config = self.on_config.clone();
            let disabled = self.disabled;
            let menu_id = id.clone();
            dropdown(
                SharedString::from(format!("config-{}", control.id)),
                open && !disabled,
                Corner::BottomLeft,
                trigger,
                move || {
                    control
                        .options
                        .iter()
                        .map(|option| {
                            let on_config = on_config.clone();
                            let (config_id, value) = (control.id.clone(), option.value.clone());
                            let mut entry = item(option.label.clone(), move |window, cx| on_config((config_id.clone(), value.clone()), window, cx))
                                .checked(option.value == control.value);
                            if let Some(description) = &option.description {
                                entry = entry.detail(description.clone());
                            }
                            entry.entry()
                        })
                        .collect()
                },
                move |open, window, cx| set_menu(open.then(|| menu_id.clone()), window, cx),
                cx,
            )
        });
        let (glyph, variant, tip, action) = if self.busy {
            (Icon::Square, Variant::Primary, "Stop (Esc)", self.on_stop.clone())
        } else {
            (Icon::ArrowUp, Variant::Primary, "Send (Enter)", self.on_send.clone())
        };
        let send_disabled = !self.busy && (self.disabled || empty);
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_2xl()
            .border_1()
            .border_color(if focused { t.ring } else { t.border })
            .bg(t.background)
            .shadow_sm()
            .child(div().px_1().text_sm().line_height(px(21.)).child(self.field.clone()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(self.leading)
                    .children(controls)
                    .child(div().flex_1())
                    .child(
                        icon_button("composer-send", glyph)
                            .variant(variant)
                            .size(ButtonSize::Icon)
                            .disabled(send_disabled)
                            .tooltip(tip)
                            .on_click(move |_, window, cx| action((), window, cx)),
                    ),
            )
    }
}

/// The composer's subscription chip: fixed for a thread, a picker for a new one when there is a choice.
pub fn subscription_chip(
    current: Option<&Subscription>,
    all: &[Subscription],
    read_only: bool,
    open: bool,
    set_open: impl Fn(bool, &mut Window, &mut App) + 'static,
    on_pick: impl Fn(String, &mut Window, &mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let t = theme(cx);
    let label = current.map_or("No subscription".to_owned(), |s| s.label.clone());
    let chip = div()
        .id("composer-subscription")
        .flex()
        .items_center()
        .gap_1p5()
        .h(px(28.))
        .px_2()
        .rounded_lg()
        .text_xs()
        .text_color(t.muted_foreground)
        .child(icon(Icon::Claude, t.muted_foreground).size(px(13.)))
        .child(label);
    if read_only || all.len() < 2 {
        let tip: SharedString = if read_only { LOCKED_HINT.into() } else { current.map_or(String::new(), |s| s.config_dir.clone()).into() };
        return chip
            .when(read_only, |d| d.child(icon(Icon::Lock, t.muted_foreground).size(px(11.))))
            .tooltip(move |_, cx| tooltip_view(tip.clone(), cx))
            .into_any_element();
    }
    let on_pick = Rc::new(on_pick);
    let entries: Vec<(String, String, Option<String>, gpui::Hsla, bool)> = all
        .iter()
        .map(|s| {
            let account = match &s.account {
                oniko_core::model::AccountStatus::LoggedIn { email, plan, .. } => {
                    let parts: Vec<String> = [email.clone(), oniko_core::model::plan_label(plan.as_deref())].into_iter().flatten().collect();
                    (!parts.is_empty()).then(|| parts.join(" · "))
                }
                _ => None,
            };
            (s.id.clone(), s.label.clone(), account, account_color(&s.account, &t), Some(&s.id) == current.map(|c| &c.id))
        })
        .collect();
    dropdown(
        "composer-subscription-menu",
        open,
        Corner::BottomLeft,
        chip.hover(|s| s.bg(t.accent)).child(icon(Icon::ChevronDown, t.muted_foreground).size(px(12.))),
        move || {
            let mut rows = vec![MenuEntry::Label("Start new threads on".into()), MenuEntry::Separator];
            rows.extend(entries.into_iter().map(|(id, label, detail, dot, checked)| {
                let on_pick = on_pick.clone();
                let mut row = item(label, move |window, cx| on_pick(id.clone(), window, cx)).dot(dot).checked(checked);
                if let Some(detail) = detail {
                    row = row.detail(detail);
                }
                row.entry()
            }));
            rows
        },
        set_open,
        cx,
    )
}
