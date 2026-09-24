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
use unicode_width::UnicodeWidthStr;

use crate::app::finder::Finder;
use crate::app::pointer::{Hits, Menu, Seam, Target, menu, thumb};
use crate::app::prompt::Prompt;
use crate::app::results::{INSPECT_WIDTH, Inspector, inspect_title};
use crate::app::scratch::{Scratch, char_width};
use crate::app::{App, Focus, TabState, key_named, keys_for};
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

/// The frame, and where on it each thing that can be clicked was drawn.
pub fn render(frame: &mut Frame, app: &App, theme: &Theme) -> Hits {
    let mut hits = Hits::default();
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        let message = format!("sql-bench needs {MIN_WIDTH}x{MIN_HEIGHT}");
        frame.render_widget(
            Paragraph::new(Span::styled(message, theme.dim)).alignment(Alignment::Center),
            middle_row(area),
        );
        return hits;
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
        return hits;
    }

    let [bar, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let [objects, scratch, results] = app.shell.split.areas(body);
    let right = scratch.union(results);

    tab_bar(frame, app, theme, bar, &mut hits);
    // Under the buttons each pane draws over itself.
    hits.push(objects, Target::Pane(Focus::Objects));
    hits.push(scratch, Target::Pane(Focus::Scratch));
    hits.push(results, Target::Pane(Focus::Results));
    // The seams go over the panes' borders and under whatever the panes
    // draw on them after.
    hits.push(
        Rect { width: 1, ..right },
        Target::Seam {
            seam: Seam::Objects,
            area: body,
        },
    );
    hits.push(
        Rect {
            y: scratch.bottom().saturating_sub(1),
            height: 1,
            ..scratch
        },
        Target::Seam {
            seam: Seam::Scratch,
            area: right,
        },
    );
    objects::render(frame, app, theme, objects, &mut hits);
    scratch_pane(frame, app, theme, scratch, &mut hits);
    results::render(frame, app, theme, results, &mut hits);
    if app.shell.prompt.is_none() {
        footer_line(frame, app, theme, footer, &mut hits);
    }
    if let Some(inspector) = &app.shell.inspector {
        render_inspector(frame, area, app, inspector, theme, &mut hits);
    }
    // The help goes over the inspector, because Esc closes them in that
    // order too.
    if app.shell.help {
        render_help(frame, area, app, theme, &mut hits);
    }
    // The prompt takes every key, so it takes every click too: one beside it
    // gives up the way Esc does, and only its own text and buttons are over
    // that.
    if app.shell.prompt.is_some() {
        hits.push(area, Target::Outside);
        footer_line(frame, app, theme, footer, &mut hits);
    }
    if let Some(open) = app.shell.mouse.menu {
        render_menu(frame, area, open, theme, &mut hits);
    }
    // And the finder over everything: it takes every key while it is open.
    if let Some(finder) = &app.shell.finder {
        render_finder(frame, area, finder, app, theme, &mut hits);
    }
    hover(frame, app, theme, &hits);
    hits
}

/// What is under the pointer, restyled last so it sits over everything —
/// and read from the same hits a click is, so what lights up is exactly what
/// a click there would act on.
///
/// A seam is lit in the accent colour on the hover's ground, which shows
/// even over a focused pane's border, and it stays lit while it is held
/// however far the pointer has outrun it.
fn hover(frame: &mut Frame, app: &App, theme: &Theme, hits: &Hits) {
    let held = app.shell.mouse.seam().and_then(|held| {
        hits.regions()
            .find(|(_, target)| matches!(target, Target::Seam { seam, .. } if *seam == held))
    });
    let Some((rect, target)) = held.or_else(|| app.shell.mouse.pointer.and_then(|at| hits.at(at)))
    else {
        return;
    };
    match target {
        Target::Seam { .. } => frame
            .buffer_mut()
            .set_style(rect, theme.hover.patch(theme.accent)),
        _ if target.hovers() => frame.buffer_mut().set_style(rect, theme.hover),
        _ => {}
    }
}

/// `1 local-mssql ●  2 local-oracle ○`, the tab showing in the accent colour,
/// and `? Help` at the right end while the tabs leave room for it.
fn tab_bar(frame: &mut Frame, app: &App, theme: &Theme, bar: Rect, hits: &mut Hits) {
    let line = Line::from(tabs(app, theme, bar, hits));
    let end = bar
        .x
        .saturating_add(u16::try_from(line.width()).unwrap_or(u16::MAX));
    frame.render_widget(line, bar);
    let help = " ? Help ";
    let x = bar.right().saturating_sub(cells(help));
    if x > end {
        frame.buffer_mut().set_string(x, bar.y, help, theme.dim);
        hits.push(
            Rect::new(x, bar.y, cells(help), 1),
            // F1, because the pad types a `?`.
            button(app.shell.focus, "F1"),
        );
    }
}

/// The tab labels, drawn. Each is a click target, cut to the bar where it
/// runs off the end.
fn tabs(app: &App, theme: &Theme, bar: Rect, hits: &mut Hits) -> Vec<Span<'static>> {
    let mut spans = Vec::with_capacity(app.tabs.len() * 2);
    let mut x = bar.x;
    for (index, tab) in app.tabs.iter().enumerate() {
        let gap = Span::raw(if index == 0 { " " } else { "  " });
        let label = Span::styled(
            format!("{} {} {}", index + 1, tab.name, app.shell.mark(&tab.state)),
            if index == app.shell.active_tab {
                theme.accent
            } else {
                theme.dim
            },
        );
        x = x.saturating_add(width(&gap));
        let region = Rect::new(x, bar.y, width(&label), 1).intersection(bar);
        hits.push(region, Target::Tab(index));
        x = x.saturating_add(width(&label));
        spans.push(gap);
        spans.push(label);
    }
    spans
}

fn width(span: &Span) -> u16 {
    u16::try_from(span.width()).unwrap_or(u16::MAX)
}

/// How many cells `text` takes on screen.
fn cells(text: &str) -> u16 {
    u16::try_from(UnicodeWidthStr::width(text)).unwrap_or(u16::MAX)
}

/// A button pressing the key [`KEYS`](crate::app::KEYS) spells `name`.
fn button(pane: Focus, name: &str) -> Target {
    Target::Button {
        pane,
        key: key_named(name).unwrap_or_else(|| panic!("{name}: not a key")),
    }
}

/// Buttons right-aligned over the top border of `area`, each ` label ` with
/// a border cell between it and the next, pressing its key in `pane`.
///
/// The title keeps its room: a button that would reach it is dropped, and so
/// is every one left of it, so the least of them goes first in `chips`.
pub(super) fn buttons(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    chips: &[(&str, &str)],
    pane: Focus,
    style: Style,
    hits: &mut Hits,
) {
    // The title, and one border cell after it.
    let floor = area.x.saturating_add(cells(title)).saturating_add(2);
    // Where the next button ends: one border cell short of the corner.
    let mut end = area.right().saturating_sub(2);
    for (label, name) in chips.iter().rev() {
        let text = format!(" {label} ");
        let Some(x) = end.checked_sub(cells(&text)).filter(|x| *x >= floor) else {
            break;
        };
        frame.buffer_mut().set_string(x, area.y, &text, style);
        hits.push(Rect::new(x, area.y, cells(&text), 1), button(pane, name));
        end = x.saturating_sub(1);
    }
}

/// A scrollbar over the right border of `pane`, beside `rows`, drawn from
/// row `top` of `content`, when there is more than `rows` holds. The track
/// keeps the border's glyph, which is why it is only there for a click, and
/// the thumb is `┃`: a shape, so it reads with `NO_COLOR` too. The track
/// either side of the thumb is PageUp or PageDown for `focus`.
///
/// It stays between the border's corners, so the title and its buttons are
/// never under it.
pub(super) fn scrollbar(
    frame: &mut Frame,
    (pane, rows): (Rect, Rect),
    (focus, top, content): (Focus, usize, usize),
    style: Style,
    hits: &mut Hits,
) {
    let viewport = usize::from(rows.height);
    let x = pane.right().saturating_sub(1);
    let track = Rect::new(x, rows.y, 1, rows.height).intersection(Rect::new(
        x,
        pane.y.saturating_add(1),
        1,
        pane.height.saturating_sub(2),
    ));
    if content <= viewport || track.is_empty() {
        return;
    }
    let cells = thumb(top, content, viewport, track.height);
    for y in cells.clone() {
        frame.buffer_mut().set_string(x, track.y + y, "┃", style);
    }
    hits.push(
        Rect::new(x, track.y, 1, cells.start),
        button(focus, "PageUp"),
    );
    hits.push(
        Rect::new(x, track.y + cells.start, 1, cells.end - cells.start),
        Target::Thumb {
            pane: focus,
            content,
            viewport,
            track,
        },
    );
    hits.push(
        Rect::new(x, track.y + cells.end, 1, track.height - cells.end),
        button(focus, "PageDown"),
    );
}

/// A placeholder that is a button, `[ Connect ]`, on the first row of
/// `area`.
pub(super) fn placeholder_button(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    (pane, name): (Focus, &str),
    theme: &Theme,
    hits: &mut Hits,
) {
    let text = format!("[ {label} ]");
    let row = Rect {
        width: cells(&text).min(area.width),
        height: 1.min(area.height),
        ..area
    };
    frame.render_widget(Span::styled(text, theme.accent), row);
    hits.push(row, button(pane, name));
}

/// The scratch pad: a line number gutter, the text with the cursor cell and
/// the selection painted on it, and `[modified]` while the file is behind.
///
/// It draws its own block rather than going through [`pane`], because it is
/// the one pane whose body has to know how wide the inside is.
fn scratch_pane(frame: &mut Frame, app: &App, theme: &Theme, area: Rect, hits: &mut Hits) {
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
    let Some(tab) = app.tab() else {
        return;
    };
    let mut chips = vec![
        ("✎ Editor", "Ctrl-E"),
        ("▶▶ All", "F5"),
        ("▶ Run", "Ctrl-R"),
    ];
    if tab.results.running() {
        chips.push(("■ Stop", "Esc"));
    }
    buttons(
        frame,
        area,
        title,
        &chips,
        Focus::Scratch,
        title_style,
        hits,
    );
    let scratch = &tab.scratch;
    if scratch.is_empty() && !focused {
        frame.render_widget(
            Paragraph::new(placeholder("your SQL goes here", theme)),
            inner,
        );
        return;
    }
    let (lines, top) = scratch_lines(scratch, theme, inner, focused, hits);
    frame.render_widget(Paragraph::new(lines), inner);
    scrollbar(
        frame,
        (area, inner),
        (Focus::Scratch, top, scratch.lines().len()),
        border_style,
        hits,
    );
}

/// The rows of the pad that are on screen, gutter and all, and the line they
/// start from. The window they were drawn from goes in the hits for a click
/// to land in.
fn scratch_lines(
    scratch: &Scratch,
    theme: &Theme,
    area: Rect,
    focused: bool,
    hits: &mut Hits,
) -> (Vec<Line<'static>>, usize) {
    let height = usize::from(area.height);
    let lines = scratch.lines();
    let digits = lines.len().to_string().len();
    // The gutter is the widest number and the space after it.
    let width = usize::from(area.width).saturating_sub(digits + 1).max(1);
    let (top, left) = scratch.window(height, width);
    hits.push(
        area,
        Target::Pad {
            top,
            left,
            gutter: digits + 1,
        },
    );
    let (cursor_line, cursor_column) = scratch.cursor();
    let selection = scratch.selection();
    let flagged = scratch.flagged();
    let lines = lines
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
                last = last.max(characters.len() + left + width);
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
            // `left` and `width` are terminal columns and `column` is a
            // character, so a wide one moves `cell` on by two.
            let mut cell = 0;
            for column in 0..last {
                let character = characters.get(column).copied().unwrap_or(' ');
                let start = cell;
                cell += char_width(character);
                if cell <= left {
                    continue;
                }
                if cell > left + width {
                    break;
                }
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
                // Half a wide character is left of the window: its other
                // half is a blank.
                run.push(if start < left { ' ' } else { character });
            }
            if !run.is_empty() {
                spans.push(Span::styled(run, run_style));
            }
            Line::from(spans)
        })
        .collect();
    (lines, top)
}

fn placeholder(text: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(text.to_owned(), theme.dim))
}

/// Key hints on the left — or the error, or the status — and where the tab's
/// connection is on the right. Each part of it that is one key is a button
/// for that key.
fn footer_line(frame: &mut Frame, app: &App, theme: &Theme, area: Rect, hits: &mut Hits) {
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
    let budget = usize::from(area.width).saturating_sub(right.width() + 1);
    let focus = app.shell.focus;
    let mut parts = match (
        &app.shell.prompt,
        &app.shell.error,
        app.shell.status.as_str(),
    ) {
        (Some(prompt), ..) => prompt_spans(prompt, focus, theme, budget),
        (None, Some(error), _) if error_closes(app) => vec![
            (
                Span::styled(cut(error, budget.saturating_sub(3)), theme.error),
                None,
            ),
            (Span::styled(" × ", theme.error), Some(button(focus, "Esc"))),
        ],
        (None, Some(error), _) => vec![(Span::styled(cut(error, budget), theme.error), None)],
        (None, None, "") => hints(focus, budget, theme),
        (None, None, status) => vec![(Span::raw(cut(status, budget)), None)],
    };
    let used: usize = parts.iter().map(|(span, _)| span.width()).sum();
    let gap = usize::from(area.width).saturating_sub(used + right.width());
    // The tree's `c` and `C`, because the pad would type them.
    let toggle = match state {
        TabState::Connected | TabState::Connecting => "C",
        _ => "c",
    };
    parts.push((Span::raw(" ".repeat(gap)), None));
    parts.push((
        right,
        app.shell
            .prompt
            .is_none()
            .then(|| button(Focus::Objects, toggle)),
    ));
    let mut x = area.x;
    for (span, target) in &parts {
        if let Some(target) = target {
            hits.push(
                Rect::new(x, area.y, width(span), 1).intersection(area),
                *target,
            );
        }
        x = x.saturating_add(width(span));
    }
    if let Some(prompt) = &app.shell.prompt {
        // The text and the cell past its end, where a click puts the cursor
        // back at the end.
        let text = Rect::new(
            area.x + cells(PROMPT),
            area.y,
            cells(&prompt.text).saturating_add(1),
            1,
        );
        hits.push(text.intersection(area), Target::PromptText);
    }
    let spans: Vec<Span> = parts.into_iter().map(|(span, _)| span).collect();
    frame.render_widget(Line::from(spans), area);
}

/// Whether Esc would close the error, which is only when nothing else is
/// ahead of it: a running query is cancelled first, and a filter in the tree
/// or the grid cleared.
fn error_closes(app: &App) -> bool {
    app.tab().is_none_or(|tab| {
        !tab.results.running()
            && (app.shell.focus != Focus::Objects || tab.objects.filter().is_empty())
            && (app.shell.focus != Focus::Results || tab.results.filter().is_empty())
    })
}

/// ` text`, cut to `budget` columns with an ellipsis. The footer's right end
/// says where the connection is, and a message long enough to push it off
/// the screen has taken the footer over rather than used it.
/// A driver's message of several lines is one line here, each break a space.
fn cut(text: &str, budget: usize) -> String {
    let line = text.replace("\r\n", " ").replace(['\n', '\r'], " ");
    format!(
        " {}",
        crate::export::cut_to(&line, budget.saturating_sub(1).max(1))
    )
}

const PROMPT: &str = " Export to: ";

/// `label` and the line being typed, the cursor painted the way the scratch
/// pad's is — a `TestBackend` has no terminal cursor, so a prompt whose
/// cursor were the real one could not be tested at all.
fn typed(prompt: &Prompt, label: &str, theme: &Theme) -> Vec<Span<'static>> {
    let characters: Vec<char> = prompt.text.chars().collect();
    vec![
        Span::styled(label.to_owned(), theme.accent),
        Span::raw(characters.iter().take(prompt.cursor).collect::<String>()),
        Span::styled(
            characters
                .get(prompt.cursor)
                .copied()
                .unwrap_or(' ')
                .to_string(),
            theme.cursor,
        ),
        Span::raw(
            characters
                .iter()
                .skip(prompt.cursor + 1)
                .collect::<String>(),
        ),
    ]
}

/// ` Export to: ` and the path being typed, then Enter and Esc as buttons,
/// while there is room for them.
fn prompt_spans(
    prompt: &Prompt,
    focus: Focus,
    theme: &Theme,
    budget: usize,
) -> Vec<(Span<'static>, Option<Target>)> {
    let mut parts: Vec<(Span<'static>, Option<Target>)> = typed(prompt, PROMPT, theme)
        .into_iter()
        .map(|span| (span, None))
        .collect();
    let mut room = budget.saturating_sub(parts.iter().map(|(span, _)| span.width()).sum());
    // Dropped from the left, the way a pane's are: Cancel is the one to keep.
    let mut chips = Vec::new();
    for (label, name) in [("Cancel", "Esc"), ("Export", "Enter")] {
        let chip = format!(" {label} ");
        if chip.len() + 1 > room {
            break;
        }
        room -= chip.len() + 1;
        chips.push((Span::styled(chip, theme.accent), Some(button(focus, name))));
        chips.push((Span::raw(" "), None));
    }
    parts.extend(chips.into_iter().rev());
    parts
}

/// As many of this pane's keys as fit, in the order [`KEYS`] lists them,
/// each a button for its key when it is one key. Esc is not: while the hints
/// are showing nothing is running and no help is open, so it would have
/// nothing to do of what it says it does.
///
/// [`KEYS`]: crate::app::KEYS
fn hints(focus: Focus, budget: usize, theme: &Theme) -> Vec<(Span<'static>, Option<Target>)> {
    let mut parts = Vec::new();
    let mut used = 0;
    for (key, _, does) in keys_for(focus) {
        let hint = format!("{key} {does}");
        let separator = if used == 0 { " " } else { "  " };
        let wide = separator.len() + hint.chars().count();
        if used + wide > budget {
            break;
        }
        used += wide;
        let target = key_named(key)
            .filter(|_| *key != "Esc")
            .map(|key| Target::Button { pane: focus, key });
        parts.push((Span::styled(separator, theme.dim), None));
        parts.push((Span::styled(hint, theme.dim), target));
    }
    parts
}

/// The keys that work in the focused pane, from the one table the footer
/// hints come from too. It never grows past the screen: what does not fit
/// scrolls, and the title says which rows are showing.
fn render_help(frame: &mut Frame, area: Rect, app: &App, theme: &Theme, hits: &mut Hits) {
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
    hits.push(area, Target::Outside);
    hits.push(overlay, Target::Overlay);
    // Each row that is one key is a button for it, the width of the inside.
    for (y, (key, _, _)) in (overlay.y + 1..).zip(&rows[top..top + showing]) {
        if let Some(key) = key_named(key) {
            let row = Rect::new(overlay.x + 1, y, overlay.width.saturating_sub(2), 1);
            hits.push(row, Target::Button { pane: focus, key });
        }
    }
    buttons(
        frame,
        overlay,
        &title,
        &[("×", "Esc")],
        focus,
        theme.accent,
        hits,
    );
}

/// The whole of one cell over the grid: the column, its type and how much of
/// it there is in the title, and the value under it, scrolled by j and k.
///
/// The cell is read here rather than copied when Enter opened the overlay, so
/// what is on screen is what the grid holds and nothing is kept twice.
fn render_inspector(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    inspector: &Inspector,
    theme: &Theme,
    hits: &mut Hits,
) {
    let Some(results) = app.tab().map(|tab| &tab.results) else {
        return;
    };
    let (Some(column), Some(cell)) = (results.column(), results.cell()) else {
        return;
    };
    let total = app.inspect_height();
    let height = u16::try_from(total)
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .min(area.height.saturating_sub(2));
    let overlay = centered(area, INSPECT_WIDTH as u16 + 4, height);
    let showing = usize::from(height.saturating_sub(2));
    let top = inspector.scroll.min(total.saturating_sub(showing));
    let body: Vec<Line> = app
        .inspect_lines(top, showing)
        .into_iter()
        .map(Line::raw)
        .collect();
    let title = format!(" {} ", inspect_title(column, cell));
    frame.render_widget(Clear, overlay);
    frame.render_widget(
        Paragraph::new(body).block(titled(&title, theme.accent, theme.accent)),
        overlay,
    );
    hits.push(area, Target::Outside);
    hits.push(overlay, Target::Overlay);
    buttons(
        frame,
        overlay,
        &title,
        &[("×", "Esc")],
        app.shell.focus,
        theme.accent,
        hits,
    );
}

/// The context menu a right-click opened: each entry what it does and, on
/// the right, the key it presses. It opens right and down from the pointer,
/// or left and up where that would run off the frame, and the entry Enter
/// would pick is painted like the pad's cursor.
fn render_menu(frame: &mut Frame, area: Rect, open: Menu, theme: &Theme, hits: &mut Hits) {
    let entries = menu(open.pane);
    let does = entries
        .iter()
        .map(|(_, does)| cells(does))
        .max()
        .unwrap_or(0);
    let keys = entries.iter().map(|(key, _)| cells(key)).max().unwrap_or(0);
    // Two borders, a cell of padding inside each, and three between.
    let width = (does + keys + 7).min(area.width);
    let height = u16::try_from(entries.len() + 2)
        .unwrap_or(u16::MAX)
        .min(area.height);
    let rect = Rect::new(
        open_from(open.at.x, width, area.x, area.right()),
        open_from(open.at.y, height, area.y, area.bottom()),
        width,
        height,
    );
    let (does, keys) = (usize::from(does), usize::from(keys));
    let lines: Vec<Line> = entries
        .iter()
        .map(|(key, what)| Line::raw(format!("{what:<does$}   {key:>keys$}")))
        .collect();
    let title = format!(" {} ", open.pane.title());
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(titled(&title, theme.accent, theme.accent)),
        rect,
    );
    hits.push(area, Target::Outside);
    hits.push(rect, Target::Overlay);
    for (item, y) in (rect.y + 1..rect.bottom().saturating_sub(1)).enumerate() {
        let row = Rect::new(rect.x + 1, y, rect.width.saturating_sub(2), 1);
        if item == open.item {
            frame.buffer_mut().set_style(row, theme.cursor);
        }
        hits.push(row, Target::MenuItem(item));
    }
}

/// Where something `size` long starts on one axis, opening from `at`
/// towards `end`, or back from `at` when that would run past it, and never
/// off either end — `size` is no more than `end - start`.
fn open_from(at: u16, size: u16, start: u16, end: u16) -> u16 {
    if at.saturating_add(size) <= end {
        at.max(start)
    } else {
        at.saturating_add(1)
            .saturating_sub(size)
            .clamp(start, end - size)
    }
}

/// How many match rows the finder shows at most: enough to scan, few enough
/// that the query line stays near the middle of any supported screen.
const FINDER_ROWS: u16 = 20;

/// The finder over the layout: the query on the first line, the matches
/// under it best first, the chosen one in the cursor colour. Each row is the
/// qualified name, what kind of thing it is and which tab holds it.
///
/// ponytail: a click beside it closes it and one on it does nothing; rows
/// that open what they name on a click would need a target of their own.
fn render_finder(
    frame: &mut Frame,
    area: Rect,
    finder: &Finder,
    app: &App,
    theme: &Theme,
    hits: &mut Hits,
) {
    let width = area.width.saturating_sub(4).min(96);
    let height = (FINDER_ROWS + 3).min(area.height.saturating_sub(2));
    let overlay = centered(area, width, height);
    let showing = usize::from(height.saturating_sub(3));
    let matches = finder.matches();
    let top = if finder.cursor >= showing {
        finder.cursor + 1 - showing
    } else {
        0
    };
    let inner = usize::from(width.saturating_sub(4));
    let mut lines = vec![Line::from(typed(&finder.query, "> ", theme))];
    if matches.is_empty() {
        let message = if finder.indexed() == 0 {
            "nothing indexed yet: c connects a tab"
        } else if finder.query.text.trim().is_empty() {
            "type a name, or schema.name"
        } else {
            "no objects match"
        };
        lines.push(placeholder(message, theme));
    }
    // The name column is as wide as the widest name showing, so the kinds
    // and the tabs line up down the list.
    let name_width = matches
        .iter()
        .skip(top)
        .take(showing)
        .map(|found| found.object.schema.chars().count() + 1 + found.object.name.chars().count())
        .max()
        .unwrap_or(0)
        .min(inner.saturating_sub(22));
    for (at, found) in matches.iter().enumerate().skip(top).take(showing) {
        let qualified = format!("{}.{}", found.object.schema, found.object.name);
        let name = crate::app::results::cut(&qualified, name_width);
        let tab = app.tabs.get(found.tab).map_or("", |tab| tab.name.as_str());
        let text = format!(
            "{name:<name_width$}  {:<9}  {tab}",
            found.object.kind.as_str()
        );
        lines.push(Line::from(Span::styled(
            crate::app::results::cut(&text, inner).into_owned(),
            if at == finder.cursor {
                theme.cursor
            } else {
                Style::default()
            },
        )));
    }
    frame.render_widget(Clear, overlay);
    frame.render_widget(
        Paragraph::new(lines).block(titled(&finder.title(), theme.accent, theme.accent)),
        overlay,
    );
    hits.push(area, Target::Outside);
    hits.push(overlay, Target::Overlay);
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
