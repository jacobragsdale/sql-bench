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
use super::{buttons, placeholder, placeholder_button, titled};
use crate::app::objects::{INDENT, Objects};
use crate::app::pointer::Hits;
use crate::app::{App, Focus};

pub(super) fn render(frame: &mut Frame, app: &App, theme: &Theme, area: Rect, hits: &mut Hits) {
    let focused = app.shell.focus == Focus::Objects;
    let (title_style, border_style) = if focused {
        (theme.accent, theme.accent)
    } else {
        (theme.dim, theme.border)
    };
    let Some(tab) = app.tab() else {
        frame.render_widget(titled(" Objects ", title_style, border_style), area);
        return;
    };
    let objects = &tab.objects;
    // Esc cancels a running query before it reaches the filter.
    let clears = !objects.filter().is_empty() && !tab.results.running();
    let title = match objects.filter() {
        "" => " Objects ".to_owned(),
        filter => format!(" Objects /{filter} "),
    };
    let block = titled(&title, title_style, border_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if objects.is_empty() {
        placeholder_button(frame, inner, "Connect", (Focus::Objects, "c"), theme, hits);
        return;
    }
    let mut chips = vec![("⟳ Reload", "r")];
    // While the filter is being typed there is nothing for `/` to open.
    if !objects.filtering() {
        chips.push(("/ Filter", "/"));
    }
    if clears {
        chips.push(("×", "Esc"));
    }
    buttons(
        frame,
        area,
        &title,
        &chips,
        Focus::Objects,
        title_style,
        hits,
    );
    let visible = objects.visible();
    if visible.is_empty() {
        frame.render_widget(
            Paragraph::new(placeholder("no objects match", theme)),
            inner,
        );
        if clears {
            let below = Rect {
                y: inner.y.saturating_add(1),
                height: inner.height.saturating_sub(1),
                ..inner
            };
            placeholder_button(
                frame,
                below,
                "Clear filter",
                (Focus::Objects, "Esc"),
                theme,
                hits,
            );
        }
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
