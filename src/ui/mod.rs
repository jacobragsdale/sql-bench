//! Rendering: state in, a `ratatui::Frame` out, and the theme the frame is
//! drawn with. Never reads a database and never decides anything.

mod objects;
mod results;
pub mod theme;

#[cfg(test)]
mod tests;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};

use crate::app::scratch::Scratch;
use crate::app::{App, Focus, TabState, keys_for};
use theme::Theme;

/// Smaller than this and the three panes are narrower than their own titles,
/// so one message is more use than a layout nobody can read.
pub const MIN_WIDTH: u16 = 60;
pub const MIN_HEIGHT: u16 = 15;

/// What a first run is told, when `config.toml` names no connection.
const NO_CONNECTIONS: &[&str] = &[
    "No connections yet.",
    "",
    "Write ~/.config/sql-bench/config.toml, or whatever",
    "$SQL_BENCH_CONFIG names, with one [[connection]] each:",
    "",
    "  [[connection]]",
    "  name = \"local-mssql\"",
    "  kind = \"mssql\"       # or \"oracle\"",
    "  host = \"localhost\"",
    "  database = \"bench\"   # \"service\" for oracle",
    "  user = \"sa\"",
    "  password = \"...\"     # or password_env, password_cmd",
    "q quits.",
];

pub fn render(frame: &mut Frame, app: &App, theme: &Theme) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        let message = format!("sql-bench needs {MIN_WIDTH}x{MIN_HEIGHT}");
        frame.render_widget(
            Paragraph::new(Span::styled(message, theme.dim)).alignment(Alignment::Center),
            middle_row(area),
        );
        return;
    }
    if app.tabs.is_empty() {
        let lines: Vec<Line> = NO_CONNECTIONS
            .iter()
            .map(|line| Line::from(Span::styled(*line, theme.dim)))
            .collect();
        frame.render_widget(
            Paragraph::new(lines).block(titled(" sql-bench ", theme.accent, theme.border)),
            area,
        );
        return;
    }

    let [bar, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let [objects, right] =
        Layout::horizontal([Constraint::Percentage(30), Constraint::Min(20)]).areas(body);
    let [scratch, results] =
        Layout::vertical([Constraint::Percentage(40), Constraint::Min(3)]).areas(right);

    frame.render_widget(tab_bar(app, theme), bar);
    objects::render(frame, app, theme, objects);
    scratch_pane(frame, app, theme, scratch);
    results::render(frame, app, theme, results);
    frame.render_widget(footer_line(app, theme, area.width), footer);
    if app.shell.help {
        render_help(frame, area, app, theme);
    }
}

/// `1 local-mssql ●  2 local-oracle ○`, the tab showing in the accent colour.
fn tab_bar(app: &App, theme: &Theme) -> Line<'static> {
    let mut spans = Vec::with_capacity(app.tabs.len() * 2);
    for (index, tab) in app.tabs.iter().enumerate() {
        spans.push(Span::raw(if index == 0 { " " } else { "  " }));
        spans.push(Span::styled(
            format!("{} {} {}", index + 1, tab.name, app.shell.mark(&tab.state)),
            if index == app.shell.active_tab {
                theme.accent
            } else {
                theme.dim
            },
        ));
    }
    Line::from(spans)
}

/// The scratch pad: a line number gutter, the text with the cursor cell and
/// the selection painted on it, and `[modified]` while the file is behind.
///
/// It draws its own block rather than going through [`pane`], because it is
/// the one pane whose body has to know how wide the inside is.
fn scratch_pane(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let focused = app.shell.focus == Focus::Scratch;
    let (title_style, border_style) = if focused {
        (theme.accent, theme.accent)
    } else {
        (theme.dim, theme.border)
    };
    let scratch = app.tab().map(|tab| &tab.scratch);
    let modified = scratch.is_some_and(Scratch::modified);
    let title = if modified {
        " Scratch [modified] "
    } else {
        " Scratch "
    };
    let block = titled(title, title_style, border_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(scratch) = scratch else {
        return;
    };
    if scratch.is_empty() && !focused {
        frame.render_widget(
            Paragraph::new(placeholder("your SQL goes here", theme)),
            inner,
        );
        return;
    }
    frame.render_widget(
        Paragraph::new(scratch_lines(scratch, theme, inner, focused)),
        inner,
    );
}

/// The rows of the pad that are on screen, gutter and all.
fn scratch_lines(
    scratch: &Scratch,
    theme: &Theme,
    area: Rect,
    focused: bool,
) -> Vec<Line<'static>> {
    let height = usize::from(area.height);
    let lines = scratch.lines();
    let digits = lines.len().to_string().len();
    // The gutter is the widest number and the space after it.
    let width = usize::from(area.width).saturating_sub(digits + 1).max(1);
    let (top, left) = scratch.window(height, width);
    let (cursor_line, cursor_column) = scratch.cursor();
    let selection = scratch.selection();
    let flagged = scratch.flagged();
    lines
        .iter()
        .enumerate()
        .skip(top)
        .take(height)
        .map(|(number, text)| {
            let characters: Vec<char> = text.chars().collect();
            // Only as far as there is something to paint: a cursor past the
            // end of the line, the end of the selection, or the text.
            let mut last = characters.len();
            if focused && number == cursor_line {
                last = last.max(cursor_column + 1);
            }
            if flagged.is_some_and(|lines| lines.contains(&number)) {
                last = last.max(left + width);
            }
            if let Some((_, (end_line, end_column))) = selection
                && number == end_line
            {
                last = last.max(end_column);
            }
            let mut spans = vec![Span::styled(
                format!("{:>digits$} ", number + 1, digits = digits),
                theme.dim,
            )];
            let mut run = String::new();
            let mut run_style = Style::default();
            for column in left..last.min(left + width) {
                let style = if focused && (number, column) == (cursor_line, cursor_column) {
                    theme.cursor
                } else if selection
                    .is_some_and(|(from, to)| (number, column) >= from && (number, column) < to)
                {
                    theme.selection
                } else if flagged.is_some_and(|lines| lines.contains(&number)) {
                    theme.flagged
                } else {
                    Style::default()
                };
                if style != run_style && !run.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut run), run_style));
                }
                run_style = style;
                run.push(characters.get(column).copied().unwrap_or(' '));
            }
            if !run.is_empty() {
                spans.push(Span::styled(run, run_style));
            }
            Line::from(spans)
        })
        .collect()
}

fn placeholder(text: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(text.to_owned(), theme.dim))
}

/// Key hints on the left — or the error, or the status — and where the tab's
/// connection is on the right.
fn footer_line(app: &App, theme: &Theme, width: u16) -> Line<'static> {
    let disconnected = TabState::Disconnected;
    let tab = app.tab();
    let state = tab.map_or(&disconnected, |tab| &tab.state);
    // `● connected 4ms`: how long it took, while it is up.
    let took = tab
        .and_then(|tab| tab.connect_ms)
        .filter(|_| *state == TabState::Connected)
        .map(|ms| format!(" {ms}ms"))
        .unwrap_or_default();
    let right = Span::styled(
        format!("{} {}{took} ", app.shell.mark(state), state.label()),
        match state {
            TabState::Connected => theme.ok,
            TabState::Failed(_) => theme.error,
            _ => theme.dim,
        },
    );
    let budget = usize::from(width).saturating_sub(right.width() + 1);
    let left = match (&app.shell.error, app.shell.status.as_str()) {
        (Some(error), _) => Span::styled(cut(error, budget), theme.error),
        (None, "") => Span::styled(hints(app.shell.focus, budget), theme.dim),
        (None, status) => Span::raw(cut(status, budget)),
    };
    let gap = usize::from(width).saturating_sub(left.width() + right.width());
    Line::from(vec![left, Span::raw(" ".repeat(gap)), right])
}

/// ` text`, cut to `budget` columns with an ellipsis. The footer's right end
/// says where the connection is, and a message long enough to push it off
/// the screen has taken the footer over rather than used it.
fn cut(text: &str, budget: usize) -> String {
    let room = budget.saturating_sub(1);
    if text.chars().count() <= room {
        return format!(" {text}");
    }
    let kept: String = text.chars().take(room.saturating_sub(1)).collect();
    format!(" {kept}…")
}

/// As many of this pane's keys as fit, in the order [`KEYS`] lists them.
fn hints(focus: Focus, budget: usize) -> String {
    let mut text = String::new();
    for (key, _, does) in keys_for(focus) {
        let hint = format!("{key} {does}");
        let separator = if text.is_empty() { " " } else { "  " };
        if text.chars().count() + separator.len() + hint.chars().count() > budget {
            break;
        }
        text.push_str(separator);
        text.push_str(&hint);
    }
    text
}

/// The keys that work in the focused pane, from the one table the footer
/// hints come from too. It never grows past the screen: what does not fit
/// scrolls, and the title says which rows are showing.
fn render_help(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let focus = app.shell.focus;
    let rows: Vec<&(&str, &str, &str)> = keys_for(focus).collect();
    let key_width = rows
        .iter()
        .map(|(key, _, _)| key.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    #[allow(clippy::cast_possible_truncation)]
    let height = (rows.len() as u16 + 2).min(area.height.saturating_sub(2));
    let overlay = centered(area, 56, height);
    let showing = usize::from(height.saturating_sub(2));
    let top = app.shell.help_scroll.min(rows.len() - showing);
    let lines: Vec<Line> = rows[top..top + showing]
        .iter()
        .map(|(key, _, does)| {
            Line::from(vec![
                Span::styled(format!("{key:<key_width$}"), theme.accent),
                Span::raw((*does).to_owned()),
            ])
        })
        .collect();
    let title = if showing < rows.len() {
        format!(
            " Help · {} ({}-{} of {}) ",
            focus.title(),
            top + 1,
            top + showing,
            rows.len()
        )
    } else {
        format!(" Help · {} ", focus.title())
    };
    frame.render_widget(Clear, overlay);
    frame.render_widget(
        Paragraph::new(lines).block(titled(&title, theme.accent, theme.accent)),
        overlay,
    );
}

fn titled(title: &str, title_style: Style, border_style: Style) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .padding(Padding::horizontal(1))
        .border_style(border_style)
        .title(Span::styled(title.to_owned(), title_style))
}

/// The one row in the middle of `area`, for a message that is all there is.
fn middle_row(area: Rect) -> Rect {
    Rect {
        y: area.y + area.height / 2,
        height: 1.min(area.height),
        ..area
    }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}
