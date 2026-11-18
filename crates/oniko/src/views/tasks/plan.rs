//! The plan: waves of tasks left to right, with an arrow from each blocker to the tasks that wait on it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{AnyElement, Bounds, Context, FontWeight, Hsla, IntoElement, PathBuilder, Pixels, SharedString, Window, canvas, div, point, prelude::*, px};
use oniko_core::model::{Task, TaskId};

use super::TasksView;
use crate::state::store;
use crate::theme::theme;
use crate::views::task_page::{model_choices, model_name};
use crate::views::{Nav, Progress, status_icon};

/// Card bounds as laid out this frame, so the arrows can be drawn between them.
type CardBounds = Rc<RefCell<HashMap<TaskId, Bounds<Pixels>>>>;

const CARD_WIDTH: f32 = 240.;

impl TasksView {
    pub(super) fn plan_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        let state = store(cx).read(cx).state.clone();
        let waves = state.tasks.plan_waves(&self.project);
        if waves.is_empty() {
            return div().p_6().text_sm().text_color(t.muted_foreground).child("No tasks to plan yet.").into_any_element();
        }
        let bounds: CardBounds = Rc::default();
        let edges: Vec<(TaskId, TaskId)> = waves.iter().flatten().flat_map(|task| task.blocked_by.iter().map(|b| (b.clone(), task.id.clone()))).collect();
        let columns = waves.iter().enumerate().map(|(i, wave)| {
            let cards: Vec<AnyElement> = wave.iter().map(|task| self.plan_card(task, &bounds, cx)).collect();
            div()
                .flex()
                .flex_col()
                .flex_none()
                .w(px(CARD_WIDTH))
                .gap_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .h(px(24.))
                        .px_1()
                        .text_xs()
                        .text_color(t.muted_foreground)
                        .child(div().font_weight(FontWeight::MEDIUM).child(format!("Wave {}", i + 1)))
                        .child(wave.len().to_string()),
                )
                .children(cards)
        }).collect::<Vec<_>>();
        let color = t.muted_foreground.opacity(0.55);
        // The arrows are painted after every card's prepaint has recorded where it landed this frame,
        // and before the cards themselves, so they run underneath.
        let arrows = canvas(|_, _, _| {}, move |_, _, window, _| {
            let bounds = bounds.borrow();
            for (from, to) in &edges {
                if let (Some(from), Some(to)) = (bounds.get(from), bounds.get(to)) {
                    paint_arrow(*from, *to, color, window);
                }
            }
        })
        .absolute()
        .size_full();
        div()
            .id("tasks-plan")
            .size_full()
            .overflow_scroll()
            .child(div().relative().flex().items_start().gap(px(72.)).p_4().child(arrows).children(columns))
            .into_any_element()
    }

    fn plan_card(&self, task: &Task, bounds: &CardBounds, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        let state = &store(cx).read(cx).state;
        let status = state.tasks.status(&task.status_id).cloned();
        let model = task.model.as_deref().map(|m| model_name(m, model_choices(task, state).as_deref()));
        let progress = Progress::of(task, &state.tasks);
        let record = (bounds.clone(), task.id.clone());
        let open = task.id.clone();
        let project = self.project.clone();
        div()
            .id(SharedString::from(format!("plan-card-{}", task.id)))
            .relative()
            .flex()
            .flex_col()
            .gap_1p5()
            .px_3()
            .py_2p5()
            .rounded_lg()
            .border_1()
            .border_color(t.border.opacity(0.7))
            .bg(t.background)
            .shadow_sm()
            .cursor_pointer()
            .hover(|s| s.border_color(t.border))
            .on_click(cx.listener(move |_, _, _, cx| cx.emit(Nav::OpenTasks { project: project.clone(), task: Some(open.clone()) })))
            .child(canvas(move |b, _, _| { record.0.borrow_mut().insert(record.1.clone(), b); }, |_, _, _, _| {}).absolute().inset_0())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_none().text_xs().text_color(t.muted_foreground).child(task.key()))
                    .child(div().flex_1())
                    .children(status.map(|s| status_icon(&s, cx).into_any_element())),
            )
            .child(div().text_sm().line_clamp(2).child(task.title.clone()))
            .when(model.is_some() || progress.is_some(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_x_2()
                        .gap_y_1()
                        .min_w_0()
                        .children(model.map(|m| div().flex_none().h(px(18.)).px_2().rounded_full().bg(t.muted).text_xs().line_height(px(18.)).child(m)))
                        .children(progress.map(|p| p.render(&format!("plan-{}", task.id), cx))),
                )
            })
            .into_any_element()
    }
}

/// A curve from the blocker's right edge to the dependent's left edge, ending in an arrowhead.
fn paint_arrow(from: Bounds<Pixels>, to: Bounds<Pixels>, color: Hsla, window: &mut Window) {
    let start = point(from.right(), from.center().y);
    let tip = point(to.left() - px(2.), to.center().y);
    let end = point(tip.x - px(6.), tip.y);
    let bend = (end.x - start.x) / 2.;
    let mut curve = PathBuilder::stroke(px(1.5));
    curve.move_to(start);
    curve.cubic_bezier_to(end, point(start.x + bend, start.y), point(end.x - bend, end.y));
    if let Ok(path) = curve.build() {
        window.paint_path(path, color);
    }
    let mut head = PathBuilder::fill();
    head.add_polygon(&[tip, point(tip.x - px(7.), tip.y - px(4.)), point(tip.x - px(7.), tip.y + px(4.))], true);
    if let Ok(path) = head.build() {
        window.paint_path(path, color);
    }
}
