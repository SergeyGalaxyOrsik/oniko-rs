//! Agent replies as Markdown: parsed once into blocks, then drawn with gpui text.

use std::ops::Range;

use gpui::{AnyElement, App, FontStyle, FontWeight, HighlightStyle, IntoElement, StrikethroughStyle, StyledText, UnderlineStyle, div, prelude::*, px};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::theme::{MONO, theme};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Marks {
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
    link: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Inline {
    text: String,
    spans: Vec<(Range<usize>, Marks)>,
}

impl Inline {
    fn push(&mut self, text: &str, marks: Marks) {
        let start = self.text.len();
        self.text.push_str(text);
        if marks != Marks::default() {
            self.spans.push((start..self.text.len(), marks));
        }
    }

    fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Paragraph(Inline),
    Heading(u8, Inline),
    Code { text: String },
    List { start: Option<u64>, items: Vec<Vec<Block>> },
    Quote(Vec<Block>),
    Table { head: Vec<Inline>, rows: Vec<Vec<Inline>> },
    Rule,
}

enum Frame {
    Blocks(Vec<Block>),
    Quote(Vec<Block>),
    List { start: Option<u64>, items: Vec<Vec<Block>> },
    Item(Vec<Block>),
}

impl Frame {
    fn blocks(&mut self) -> Option<&mut Vec<Block>> {
        match self {
            Frame::Blocks(b) | Frame::Quote(b) | Frame::Item(b) => Some(b),
            Frame::List { .. } => None,
        }
    }
}

#[derive(Default)]
struct Table {
    head: Vec<Inline>,
    rows: Vec<Vec<Inline>>,
    in_head: bool,
}

pub fn parse(text: &str) -> Vec<Block> {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut stack: Vec<Frame> = vec![Frame::Blocks(vec![])];
    let mut inline: Option<Inline> = None;
    let mut marks = Marks::default();
    let mut code: Option<String> = None;
    let mut heading: Option<u8> = None;
    let mut table: Option<Table> = None;

    fn flush(stack: &mut [Frame], inline: &mut Option<Inline>) {
        if let Some(text) = inline.take().filter(|i| !i.is_empty())
            && let Some(blocks) = stack.last_mut().and_then(Frame::blocks)
        {
            blocks.push(Block::Paragraph(text));
        }
    }

    fn push_block(stack: &mut [Frame], block: Block) {
        if let Some(blocks) = stack.last_mut().and_then(Frame::blocks) {
            blocks.push(block);
        }
    }

    for event in Parser::new_ext(text, options) {
        match event {
            Event::Start(Tag::Paragraph) => {
                flush(&mut stack, &mut inline);
                inline = Some(Inline::default());
            }
            Event::End(TagEnd::Paragraph) => flush(&mut stack, &mut inline),
            Event::Start(Tag::Heading { level, .. }) => {
                flush(&mut stack, &mut inline);
                heading = Some(match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    _ => 4,
                });
                inline = Some(Inline::default());
            }
            Event::End(TagEnd::Heading(_)) => {
                let text = inline.take().unwrap_or_default();
                push_block(&mut stack, Block::Heading(heading.take().unwrap_or(3), text));
            }
            Event::Start(Tag::CodeBlock(_)) => {
                flush(&mut stack, &mut inline);
                code = Some(String::new());
            }
            Event::End(TagEnd::CodeBlock) => {
                let text = code.take().unwrap_or_default();
                push_block(&mut stack, Block::Code { text: text.trim_end_matches('\n').to_owned() });
            }
            Event::Start(Tag::List(start)) => {
                flush(&mut stack, &mut inline);
                stack.push(Frame::List { start, items: vec![] });
            }
            Event::End(TagEnd::List(_)) => {
                flush(&mut stack, &mut inline);
                if let Some(Frame::List { start, items }) = stack.pop() {
                    push_block(&mut stack, Block::List { start, items });
                }
            }
            Event::Start(Tag::Item) => stack.push(Frame::Item(vec![])),
            Event::End(TagEnd::Item) => {
                flush(&mut stack, &mut inline);
                if let Some(Frame::Item(blocks)) = stack.pop()
                    && let Some(Frame::List { items, .. }) = stack.last_mut()
                {
                    items.push(blocks);
                }
            }
            Event::Start(Tag::BlockQuote(_)) => {
                flush(&mut stack, &mut inline);
                stack.push(Frame::Quote(vec![]));
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                flush(&mut stack, &mut inline);
                if let Some(Frame::Quote(blocks)) = stack.pop() {
                    push_block(&mut stack, Block::Quote(blocks));
                }
            }
            Event::Start(Tag::Table(_)) => {
                flush(&mut stack, &mut inline);
                table = Some(Table::default());
            }
            Event::End(TagEnd::Table) => {
                if let Some(t) = table.take() {
                    push_block(&mut stack, Block::Table { head: t.head, rows: t.rows });
                }
            }
            Event::Start(Tag::TableHead) => {
                if let Some(t) = table.as_mut() {
                    t.in_head = true;
                }
            }
            Event::End(TagEnd::TableHead) => {
                if let Some(t) = table.as_mut() {
                    t.in_head = false;
                }
            }
            Event::Start(Tag::TableRow) => {
                if let Some(t) = table.as_mut() {
                    t.rows.push(vec![]);
                }
            }
            Event::Start(Tag::TableCell) => inline = Some(Inline::default()),
            Event::End(TagEnd::TableCell) => {
                let cell = inline.take().unwrap_or_default();
                if let Some(t) = table.as_mut() {
                    if t.in_head {
                        t.head.push(cell);
                    } else if let Some(row) = t.rows.last_mut() {
                        row.push(cell);
                    }
                }
            }
            Event::Start(Tag::Strong) => marks.bold = true,
            Event::End(TagEnd::Strong) => marks.bold = false,
            Event::Start(Tag::Emphasis) => marks.italic = true,
            Event::End(TagEnd::Emphasis) => marks.italic = false,
            Event::Start(Tag::Strikethrough) => marks.strike = true,
            Event::End(TagEnd::Strikethrough) => marks.strike = false,
            Event::Start(Tag::Link { .. }) => marks.link = true,
            Event::End(TagEnd::Link) => marks.link = false,
            Event::Rule => {
                flush(&mut stack, &mut inline);
                push_block(&mut stack, Block::Rule);
            }
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                if let Some(code) = code.as_mut() {
                    code.push_str(&text);
                } else {
                    inline.get_or_insert_with(Inline::default).push(&text, marks);
                }
            }
            Event::Code(text) => inline.get_or_insert_with(Inline::default).push(&text, Marks { code: true, ..marks }),
            Event::SoftBreak => inline.get_or_insert_with(Inline::default).push(" ", marks),
            Event::HardBreak => inline.get_or_insert_with(Inline::default).push("\n", marks),
            Event::TaskListMarker(done) => inline.get_or_insert_with(Inline::default).push(if done { "☑ " } else { "☐ " }, marks),
            _ => {}
        }
    }
    flush(&mut stack, &mut inline);
    while stack.len() > 1 {
        match stack.pop() {
            Some(Frame::Quote(blocks) | Frame::Item(blocks) | Frame::Blocks(blocks)) => {
                for block in blocks {
                    push_block(&mut stack, block);
                }
            }
            Some(Frame::List { start, items }) => push_block(&mut stack, Block::List { start, items }),
            None => break,
        }
    }
    match stack.pop() {
        Some(Frame::Blocks(blocks)) => blocks,
        _ => vec![],
    }
}

fn styled(inline: &Inline, cx: &App) -> StyledText {
    let t = theme(cx);
    let highlights: Vec<(Range<usize>, HighlightStyle)> = inline
        .spans
        .iter()
        .map(|(range, m)| {
            let mut style = HighlightStyle::default();
            if m.bold {
                style.font_weight = Some(FontWeight::SEMIBOLD);
            }
            if m.italic {
                style.font_style = Some(FontStyle::Italic);
            }
            if m.strike {
                style.strikethrough = Some(StrikethroughStyle { thickness: px(1.), color: None });
            }
            if m.code {
                style.background_color = Some(t.code_background);
                style.color = Some(if t.dark { t.foreground } else { gpui::rgb(0xc2410c).into() });
            }
            if m.link {
                style.color = Some(gpui::rgb(0x2563eb).into());
                style.underline = Some(UnderlineStyle { thickness: px(1.), color: None, wavy: false });
            }
            (range.clone(), style)
        })
        .collect();
    StyledText::new(inline.text.clone()).with_highlights(highlights)
}

pub fn render(blocks: &[Block], cx: &App) -> AnyElement {
    div().flex().flex_col().gap_3().children(blocks.iter().map(|b| block(b, cx))).into_any_element()
}

fn block(block: &Block, cx: &App) -> AnyElement {
    let t = theme(cx);
    match block {
        Block::Paragraph(inline) => div().child(styled(inline, cx)).into_any_element(),
        Block::Heading(level, inline) => div()
            .font_weight(FontWeight::SEMIBOLD)
            .when(*level == 1, |d| d.text_xl())
            .when(*level == 2, |d| d.text_lg())
            .when(*level >= 3, |d| d.text_base())
            .child(styled(inline, cx))
            .into_any_element(),
        Block::Code { text } => div()
            .w_full()
            .p_3()
            .rounded_lg()
            .bg(t.code_background)
            .border_1()
            .border_color(t.border)
            .font_family(MONO)
            .text_xs()
            .line_height(px(18.))
            .child(text.clone())
            .into_any_element(),
        Block::List { start, items } => div()
            .flex()
            .flex_col()
            .gap_1()
            .children(items.iter().enumerate().map(|(i, item)| {
                let marker = match start {
                    Some(first) => format!("{}.", first + i as u64),
                    None => "•".into(),
                };
                div()
                    .flex()
                    .gap_2()
                    .child(div().flex_none().min_w(px(14.)).text_color(t.muted_foreground).child(marker))
                    .child(div().flex_1().min_w_0().flex().flex_col().gap_1().children(item.iter().map(|b| self::block(b, cx))))
            }))
            .into_any_element(),
        Block::Quote(blocks) => div()
            .pl_3()
            .border_l_2()
            .border_color(t.border)
            .text_color(t.muted_foreground)
            .flex()
            .flex_col()
            .gap_2()
            .children(blocks.iter().map(|b| self::block(b, cx)))
            .into_any_element(),
        Block::Table { head, rows } => {
            let row = |cells: &Vec<Inline>, header: bool| {
                div()
                    .flex()
                    .border_b_1()
                    .border_color(t.border)
                    .when(header, |d| d.font_weight(FontWeight::SEMIBOLD).bg(t.muted))
                    .children(cells.iter().map(|cell| div().flex_1().min_w_0().px_2().py_1().child(styled(cell, cx))))
            };
            div()
                .w_full()
                .rounded_md()
                .border_1()
                .border_color(t.border)
                .text_sm()
                .child(row(head, true))
                .children(rows.iter().map(|r| row(r, false)))
                .into_any_element()
        }
        Block::Rule => div().h(px(1.)).bg(t.border).into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_common_shapes() {
        let blocks = parse("# Title\n\nSome **bold** and `code`.\n\n- one\n- two\n\n```rust\nfn main() {}\n```\n\n> quoted\n\n---");
        assert!(matches!(&blocks[0], Block::Heading(1, i) if i.text == "Title"));
        let Block::Paragraph(p) = &blocks[1] else { panic!("{blocks:?}") };
        assert_eq!(p.text, "Some bold and code.");
        assert_eq!(p.spans.len(), 2);
        assert!(matches!(&blocks[2], Block::List { start: None, items } if items.len() == 2));
        assert_eq!(blocks[3], Block::Code { text: "fn main() {}".into() });
        assert!(matches!(&blocks[4], Block::Quote(q) if q.len() == 1));
        assert_eq!(blocks[5], Block::Rule);
    }

    #[test]
    fn keeps_partial_streams_readable() {
        let blocks = parse("1. first\n2. sec");
        assert!(matches!(&blocks[0], Block::List { start: Some(1), items } if items.len() == 2));
        let blocks = parse("| a | b |\n|---|---|\n| 1 | 2 |");
        assert!(matches!(&blocks[0], Block::Table { head, rows } if head.len() == 2 && rows.len() == 1));
    }
}
