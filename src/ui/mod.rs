//! Rendering: state in, a `ratatui::Frame` out, and the theme the frame is
//! drawn with. Never reads a database and never decides anything.

pub mod theme;

#[cfg(test)]
mod tests;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph, Wrap};

use crate::app::{App, Focus, KEYS, TabState, keys_for};
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
    pane(
        frame,
        app,
        theme,
        Focus::Objects,
        objects,
        vec![placeholder("press c to connect", theme)],
    );
    pane(
        frame,
        app,
        theme,
        Focus::Scratch,
        scratch,
        vec![placeholder("your SQL goes here", theme)],
    );
    pane(
        frame,
        app,
        theme,
        Focus::Results,
        results,
        results_body(app, theme),
    );
    frame.render_widget(footer_line(app, theme, area.width), footer);
    if app.shell.help {
        render_help(frame, area, theme);
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

/// What the results pane has to say: the failure a connection ended in, or
/// the line that says nothing has run.
fn results_body(app: &App, theme: &Theme) -> Vec<Line<'static>> {
    match app.tab().map(|tab| &tab.state) {
        Some(TabState::Failed(message)) => vec![
            Line::from(Span::styled(message.clone(), theme.error)),
            Line::from(Span::styled("c to retry", theme.dim)),
        ],
        _ => vec![placeholder("nothing has run yet", theme)],
    }
}

fn placeholder(text: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(text.to_owned(), theme.dim))
}

/// One bordered pane around whatever it has to show.
fn pane(
    frame: &mut Frame,
    app: &App,
    theme: &Theme,
    which: Focus,
    area: Rect,
    body: Vec<Line<'static>>,
) {
    let focused = app.shell.focus == which;
    let (title_style, border_style) = if focused {
        (theme.accent, theme.accent)
    } else {
        (theme.dim, theme.border)
    };
    let block = titled(&format!(" {} ", which.title()), title_style, border_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    // Wrapped: a driver's complaint is as long as the driver made it.
    frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: false }), inner);
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
        (Some(error), _) => Span::styled(format!(" {error}"), theme.error),
        (None, "") => Span::styled(hints(app.shell.focus, budget), theme.dim),
        (None, status) => Span::raw(format!(" {status}")),
    };
    let gap = usize::from(width).saturating_sub(left.width() + right.width());
    Line::from(vec![left, Span::raw(" ".repeat(gap)), right])
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

/// Every key, from the one table the footer hints come from too.
fn render_help(frame: &mut Frame, area: Rect, theme: &Theme) {
    let lines: Vec<Line> = KEYS
        .iter()
        .map(|(key, place, does)| {
            Line::from(vec![
                Span::styled(format!(" {key:<10}"), theme.accent),
                Span::styled(format!("{place:<12}"), theme.dim),
                Span::raw((*does).to_owned()),
            ])
        })
        .collect();
    #[allow(clippy::cast_possible_truncation)]
    let overlay = centered(area, 56, lines.len() as u16 + 2);
    frame.render_widget(Clear, overlay);
    frame.render_widget(
        Paragraph::new(lines).block(titled(" Help ", theme.accent, theme.accent)),
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
