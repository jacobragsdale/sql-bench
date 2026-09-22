//! The result grid: a header, the column types when there is room for them,
//! and the rows that are on screen.
//!
//! Only the window is formatted — `rows[top .. top + visible]` and the
//! columns that fit across — so a frame costs the same whether the query
//! returned ten rows or a hundred thousand, which is the budget
//! `docs/DESIGN.md` sets.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::theme::Theme;
use super::{buttons, placeholder, placeholder_button, titled};
use crate::app::pointer::Hits;
use crate::app::results::{Results, Source, Status, cut, grouped_u64, shown};
use crate::app::{App, Focus, TabState};
use crate::db::model::{Cell, Column};
use crate::export::pad;

/// A pane taller than this gets a second header row with the column types.
/// Shorter than that and the types would cost a fifth of the rows on screen.
const TYPES_ABOVE: u16 = 8;

/// The gap between two columns, in characters.
const GAP: usize = 2;

pub(super) fn render(frame: &mut Frame, app: &App, theme: &Theme, area: Rect, hits: &mut Hits) {
    let focused = app.shell.focus == Focus::Results;
    let (title_style, border_style) = if focused {
        (theme.accent, theme.accent)
    } else {
        (theme.dim, theme.border)
    };
    let title = app
        .tab()
        .map_or_else(|| "Results".to_owned(), |tab| tab.results.title());
    let title = format!(" {title} ");
    let block = titled(&title, title_style, border_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(tab) = app.tab() else {
        return;
    };
    let results = &tab.results;
    buttons(
        frame,
        area,
        &title,
        &chips(results),
        Focus::Results,
        title_style,
        hits,
    );
    // A connection that never opened is what the pane has to say about,
    // whatever the last query on it did. The button goes first because how
    // far down the message wraps to is the paragraph's to know.
    if let TabState::Failed(message) = &tab.state {
        placeholder_button(frame, inner, "Retry", (Focus::Results, "c"), theme, hits);
        let below = Rect {
            y: inner.y.saturating_add(1),
            height: inner.height.saturating_sub(1),
            ..inner
        };
        frame.render_widget(
            Paragraph::new(Span::styled(message.clone(), theme.error)).wrap(Wrap { trim: false }),
            below,
        );
        return;
    }
    // `s` on a procedure: the text that made it, read only.
    if let Some(source) = results.source() {
        text_view(frame, source, theme, inner);
        return;
    }
    // The driver's own words, wrapped: a complaint is as long as it is.
    if let Some(error) = results.failure() {
        frame.render_widget(
            Paragraph::new(Span::styled(error.to_string(), theme.error)).wrap(Wrap { trim: false }),
            inner,
        );
        return;
    }
    if results.columns().is_empty() {
        let mut lines = vec![match (results.rows_affected, &results.status) {
            (Some(affected), _) => Line::from(Span::raw(format!(
                "{} rows affected",
                grouped_u64(affected)
            ))),
            (None, Status::Idle) => placeholder("nothing has run yet", theme),
            (None, _) => placeholder("no rows", theme),
        }];
        if let Some(summary) = results.summary() {
            lines.push(Line::from(Span::styled(summary, theme.dim)));
        }
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }
    grid(frame, results, theme, inner, area.height > TYPES_ABOVE);
}

/// The title's buttons, each only where its key does what it says: `[`, `]`,
/// `m` and `e` do nothing over an object's source.
fn chips(results: &Results) -> Vec<(&'static str, &'static str)> {
    let mut chips = Vec::new();
    if results.source().is_none() {
        if !results.columns().is_empty() {
            chips.push(("Export", "e"));
        }
        if results.truncated() {
            chips.push(("+10k", "m"));
        }
        if results.sets() > 1 {
            chips.extend([("◀", "["), ("▶", "]")]);
        }
    }
    if results.running() {
        chips.push(("■ Cancel", "Esc"));
    }
    chips
}

/// An object's source: a line number gutter and the lines that fit, which is
/// every line the pane costs however long the package is.
fn text_view(frame: &mut Frame, source: &Source, theme: &Theme, area: Rect) {
    let height = usize::from(area.height);
    let top = source.scroll.min(source.lines.len().saturating_sub(height));
    let digits = source.lines.len().to_string().len();
    let width = usize::from(area.width).saturating_sub(digits + 1);
    let lines: Vec<Line> = source
        .lines
        .iter()
        .enumerate()
        .skip(top)
        .take(height)
        .map(|(number, text)| {
            Line::from(vec![
                Span::styled(format!("{:>digits$} ", number + 1), theme.dim),
                Span::raw(cut(text, width).into_owned()),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

fn grid(frame: &mut Frame, results: &Results, theme: &Theme, area: Rect, types: bool) {
    let summary = results.summary();
    let width = usize::from(area.width);
    let header = 1 + usize::from(types);
    let visible = usize::from(area.height)
        .saturating_sub(header + usize::from(summary.is_some()))
        .max(1);
    let (top, left) = results.window(visible, width);
    let columns = results.columns();
    let widths = results.widths();
    let showing = fitting(widths, left, width);
    let (selected_row, selected_column) = results.selected();

    let mut lines = Vec::with_capacity(header + visible + 1);
    lines.push(head(columns, widths, &showing, theme.accent, |column| {
        &column.name
    }));
    if types {
        lines.push(head(columns, widths, &showing, theme.dim, |column| {
            &column.type_name
        }));
    }
    for (number, row) in results.rows().iter().enumerate().skip(top).take(visible) {
        let mut spans = Vec::with_capacity(showing.len() * 2);
        for (index, column) in showing.iter().copied().enumerate() {
            if index > 0 {
                spans.push(Span::raw(" ".repeat(GAP)));
            }
            let cell = row.get(column);
            let right = matches!(
                results.align().get(column),
                Some(crate::app::results::Align::Right)
            );
            let style = if (number, column) == (selected_row, selected_column) {
                theme.cursor
            } else if matches!(cell, Some(Cell::Null) | None) {
                theme.dim
            } else {
                Style::default()
            };
            spans.push(Span::styled(padded(cell, widths[column], right), style));
        }
        lines.push(Line::from(spans));
    }
    if let Some(summary) = summary {
        while lines.len() < header + visible {
            lines.push(Line::default());
        }
        lines.push(Line::from(Span::styled(summary, theme.dim)));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// One header row: the same columns, padded the same way.
fn head(
    columns: &[Column],
    widths: &[usize],
    showing: &[usize],
    style: Style,
    what: impl Fn(&Column) -> &str,
) -> Line<'static> {
    let mut spans = Vec::with_capacity(showing.len() * 2);
    for (index, column) in showing.iter().copied().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" ".repeat(GAP)));
        }
        let width = widths[column];
        let text = columns.get(column).map(&what).unwrap_or_default();
        spans.push(Span::styled(pad(&cut(text, width), width, false), style));
    }
    Line::from(spans)
}

/// The columns from `left` that fit across `width` characters. Always at
/// least one, so a column wider than the pane is cut rather than dropped.
fn fitting(widths: &[usize], left: usize, width: usize) -> Vec<usize> {
    let mut showing = Vec::new();
    let mut used = 0;
    for (column, column_width) in widths.iter().enumerate().skip(left) {
        let wants = if showing.is_empty() {
            *column_width
        } else {
            column_width + GAP
        };
        if !showing.is_empty() && used + wants > width {
            break;
        }
        used += wants;
        showing.push(column);
    }
    showing
}

/// One cell, cut to the column and padded to it in terminal columns —
/// numbers to the right, so a column of them lines up on the digit that
/// matters, and a wide glyph moves nothing along.
fn padded(cell: Option<&Cell>, width: usize, right: bool) -> String {
    let text = cell.map_or(std::borrow::Cow::Borrowed(""), shown);
    pad(&cut(&text, width), width, right)
}
