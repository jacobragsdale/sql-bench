//! The object tree: one row per visible node, two spaces of indent per
//! level, `▸` for a branch that is closed and `▾` for one that is open.
//!
//! Only the window is formatted, the way the grid formats only its window: a
//! schema with ten thousand objects in it costs the rows on screen.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::theme::Theme;
use super::{placeholder, titled};
use crate::app::objects::{INDENT, Objects};
use crate::app::{App, Focus};

pub(super) fn render(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let focused = app.shell.focus == Focus::Objects;
    let (title_style, border_style) = if focused {
        (theme.accent, theme.accent)
    } else {
        (theme.dim, theme.border)
    };
    let objects = app.tab().map(|tab| &tab.objects);
    let title = match objects.map(|objects| (objects.filter(), objects.indexing())) {
        Some((filter, _)) if !filter.is_empty() => format!(" Objects /{filter} "),
        Some((_, true)) => " Objects · indexing… ".to_owned(),
        _ => " Objects ".to_owned(),
    };
    let block = titled(&title, title_style, border_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(objects) = objects else {
        return;
    };
    if objects.is_empty() {
        frame.render_widget(
            Paragraph::new(placeholder("press c to connect", theme)),
            inner,
        );
        return;
    }
    let visible = objects.visible();
    if visible.is_empty() {
        frame.render_widget(
            Paragraph::new(placeholder("no objects match", theme)),
            inner,
        );
        return;
    }
    let height = usize::from(inner.height);
    let top = objects.window(height);
    let width = usize::from(inner.width);
    let lines: Vec<Line> = visible
        .into_iter()
        .skip(top)
        .take(height)
        .map(|index| row(objects, index, theme, width))
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// One row: the indent, the glyph, the name, and what the node is doing.
fn row(objects: &Objects, index: usize, theme: &Theme, width: usize) -> Line<'static> {
    let node = &objects.nodes()[index];
    let glyph = match (node.item.parent(), node.expanded) {
        (true, true) => "▾ ",
        (true, false) => "▸ ",
        (false, _) => "  ",
    };
    let mut text = format!(
        "{:indent$}{glyph}{}",
        "",
        node.item.label(),
        indent = node.depth * INDENT,
    );
    if node.loading {
        text.push_str(" …");
    }
    let mut spans = vec![Span::styled(
        crate::app::results::cut(&text, width).into_owned(),
        if index == objects.cursor() {
            theme.cursor
        } else {
            ratatui::style::Style::default()
        },
    )];
    if let Some(error) = &node.error {
        let room = width.saturating_sub(text.chars().count() + 1);
        spans.push(Span::styled(
            format!(
                " ✗ {}",
                crate::app::results::cut(error, room.saturating_sub(2))
            ),
            theme.error,
        ));
    }
    Line::from(spans)
}
