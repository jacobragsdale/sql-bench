//! Replay mode: the real loop, the real app and a `ratatui::backend::TestBackend`
//! sized by `--size`, driven by a file of keys instead of a keyboard. No raw
//! mode, no alternate screen, no terminal touched at all — which is what makes
//! rule 3 in `CLAUDE.md` (everything is verifiable headlessly) affordable.
//!
//! A script is one command per line; blank lines and lines starting with `#`
//! are ignored. `docs/DESIGN.md` documents the grammar for the people writing
//! scripts; [`parse_line`] is the same list for the people reading code.
//!
//! The runner takes the loop a turn at a time through [`Driver`] rather than
//! handing [`run_loop`](super::run_loop) a cleverer [`InputSource`], because
//! `expect`, `wait` and `frame` all have to look at the screen and the app
//! *between* keys, and a source called from inside the loop can see neither.
//! Each command queues its events, turns the loop until they are handled and
//! once more so that what they changed is painted, and then reads the buffer.
//! A `wait` keeps turning the loop while it polls, so whatever it is waiting
//! for can happen.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

use crate::app::{App, key_named};
use crate::cli::{Cli, Size};
use crate::config::Config;
use crate::run::state::Store;
use crate::run::{Driver, InputSource, startup_tabs};
use crate::trace::Trace;
use crate::ui::theme::Theme;

/// The script ran to its end.
const OK: u8 = 0;
/// A `wait` gave up.
const TIMED_OUT: u8 = 3;
/// An `expect` or `expect-not` was wrong.
const FAILED: u8 = 4;

/// The default `--size`, which is the size every frame in `docs/` is drawn at.
const DEFAULT_SIZE: Size = Size {
    cols: 120,
    rows: 40,
};

/// How long `wait busy` waits: long enough for a connection to a cold
/// container, short enough that CI notices a hang.
const BUSY_TIMEOUT: Duration = Duration::from_secs(60);

/// How long `wait text` waits.
const TEXT_TIMEOUT: Duration = Duration::from_secs(30);

/// How often a `wait` looks again. The loop is turned once per look, so this
/// is also how often the app is let on while a replay waits — and each look
/// reads the whole frame, so looking ten times more often costs ten times the
/// CPU and buys nothing a person would notice.
const POLL: Duration = Duration::from_millis(20);

/// What a replay run was asked for.
#[derive(Clone, Debug)]
pub struct Options {
    pub size: Size,
    /// Where `frame` and a timed-out `wait` write. Created if missing.
    pub frames: PathBuf,
    /// Also write `<name>.styles.txt`, so colour can be asserted headlessly.
    pub styles: bool,
    /// The same theme the real run would use; a test pins it.
    pub theme: Theme,
    /// Only a test shortens these.
    pub busy_timeout: Duration,
    pub text_timeout: Duration,
    /// Tabs `--connect` asked for, connected after the first frame.
    pub connect: Vec<usize>,
    /// Where the scratch pads are loaded from and saved to; a test points it
    /// at a directory of its own.
    pub store: Store,
    /// The row cap `--max-rows` asked for.
    pub max_rows: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            size: DEFAULT_SIZE,
            frames: PathBuf::from("frames"),
            styles: false,
            theme: Theme::from_env(),
            busy_timeout: BUSY_TIMEOUT,
            text_timeout: TEXT_TIMEOUT,
            connect: Vec::new(),
            store: Store::from_env(),
            max_rows: crate::db::model::QueryOptions::default()
                .max_rows
                .unwrap_or(10_000),
        }
    }
}

impl Options {
    /// What the command line asked for, and the defaults for the rest.
    /// `connect` is not among them: resolving a name needs the config, which
    /// only [`replay`] has.
    #[must_use]
    pub fn from_cli(args: &Cli) -> Self {
        let defaults = Self::default();
        Self {
            size: args.size.unwrap_or(defaults.size),
            frames: args.frames_dir.clone().unwrap_or(defaults.frames),
            styles: args.frame_styles,
            max_rows: args.max_rows,
            ..defaults
        }
    }
}

/// Run the script `--replay` names against the app this config makes.
///
/// The exit code is the script's verdict: 0 ran to the end, 3 a `wait` gave
/// up, 4 an `expect` was wrong. Anything else is an error, which the caller
/// reports as 1.
pub fn replay(config: &Config, args: &Cli) -> Result<ExitCode> {
    let path = args
        .replay
        .as_deref()
        .context("replay was asked for without a script")?;
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("reading the replay script {}", path.display()))?;
    let options = Options {
        connect: startup_tabs(config, args)?,
        ..Options::from_cli(args)
    };
    run_script(&source, App::new(config), config, &options).map(ExitCode::from)
}

/// The same run from a string, which is how a test asks for one.
fn run_script(source: &str, app: App, config: &Config, options: &Options) -> Result<u8> {
    let script = parse(source)?;
    let mut replay = Replay::new(app, config, options)?;
    // The first frame, so that the first `expect` has something to look at.
    replay.turn()?;
    for (number, command) in &script {
        if let Some(code) = replay.step(command, *number)? {
            return Ok(code);
        }
    }
    Ok(OK)
}

/// One line of a script.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Command {
    Key(KeyEvent),
    /// One [`KeyEvent`] per character, which is what typing is.
    Type(String),
    Paste(String),
    Wait(Wait),
    /// Write `<frames-dir>/<name>.txt` now.
    Frame(String),
    Resize(Size),
    /// `expect` when `present`, `expect-not` when not.
    Expect {
        text: String,
        present: bool,
    },
    Mouse(Gesture, Where),
    /// Press at the first, move to the second, release there.
    Drag([u16; 2], [u16; 2]),
}

/// What a mouse command does where it points.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Gesture {
    Click,
    DoubleClick,
    RightClick,
    Hover,
    /// One notch of the wheel: `ScrollUp`, `ScrollDown`, `ScrollLeft` or
    /// `ScrollRight`.
    Scroll(MouseEventKind),
}

impl Gesture {
    /// What a terminal sends for it at `(x, y)`.
    fn events(self, x: u16, y: u16) -> Vec<Event> {
        let at = |kind| {
            Event::Mouse(MouseEvent {
                kind,
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            })
        };
        let click = |button| {
            [
                at(MouseEventKind::Down(button)),
                at(MouseEventKind::Up(button)),
            ]
        };
        match self {
            Self::Click => click(MouseButton::Left).to_vec(),
            Self::DoubleClick => [click(MouseButton::Left), click(MouseButton::Left)].concat(),
            Self::RightClick => click(MouseButton::Right).to_vec(),
            Self::Hover => vec![at(MouseEventKind::Moved)],
            Self::Scroll(kind) => vec![at(kind)],
        }
    }
}

/// Where a mouse command points: a cell, or the text drawn there.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Where {
    /// Column and row, from 0, the way a frame file counts them.
    At(u16, u16),
    /// The first cell of the first place the text is on the frame.
    On(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Wait {
    /// Until [`App::busy`] is false.
    Busy,
    Millis(u64),
    /// Until the substring is somewhere on the frame.
    Text(String),
}

/// Every command in the file, with the line it was written on — which is what
/// a failure reports, so a long script says where it went wrong.
fn parse(source: &str) -> Result<Vec<(usize, Command)>> {
    let mut script = Vec::new();
    for (index, line) in source.lines().enumerate() {
        let number = index + 1;
        let command = parse_line(line).with_context(|| format!("line {number}"))?;
        if let Some(command) = command {
            script.push((number, command));
        }
    }
    Ok(script)
}

/// One line, or `None` for a blank line or a comment.
///
/// `type` and `paste` take the rest of the line exactly as written, spaces and
/// all, because that is the text they send. Every other argument is trimmed,
/// because a trailing space in a script is an invisible mistake.
fn parse_line(line: &str) -> Result<Option<Command>> {
    let line = line.trim_start().trim_end_matches(['\r', '\n']);
    if line.trim().is_empty() || line.starts_with('#') {
        return Ok(None);
    }
    let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
    let argument = rest.trim();
    let needed = |what: &str| -> Result<&str> {
        if argument.is_empty() {
            bail!("{verb} needs {what}");
        }
        Ok(argument)
    };
    Ok(Some(match verb {
        "key" => {
            let name = needed("a key name")?;
            Command::Key(key_named(name).ok_or_else(|| anyhow!("unknown key {name:?}"))?)
        }
        "type" => {
            needed("something to type")?;
            Command::Type(rest.to_owned())
        }
        "paste" => {
            needed("something to paste")?;
            Command::Paste(rest.to_owned())
        }
        "wait" => Command::Wait(parse_wait(needed(
            "busy, text <substring> or milliseconds",
        )?)?),
        "frame" => Command::Frame(frame_name(needed("a name")?)?),
        "resize" => Command::Resize(
            needed("a size")?
                .parse::<Size>()
                .map_err(|why| anyhow!("resize {argument}: {why}"))?,
        ),
        "expect" => Command::Expect {
            text: needed("a substring")?.to_owned(),
            present: true,
        },
        "expect-not" => Command::Expect {
            text: needed("a substring")?.to_owned(),
            present: false,
        },
        "click" => Command::Mouse(Gesture::Click, parse_where(verb, argument)?),
        "double-click" => Command::Mouse(Gesture::DoubleClick, parse_where(verb, argument)?),
        "right-click" => Command::Mouse(Gesture::RightClick, parse_where(verb, argument)?),
        "hover" => Command::Mouse(Gesture::Hover, parse_where(verb, argument)?),
        "scroll" => {
            let (way, place) = argument.split_once(' ').unwrap_or((argument, ""));
            let kind = match way {
                "up" => MouseEventKind::ScrollUp,
                "down" => MouseEventKind::ScrollDown,
                "left" => MouseEventKind::ScrollLeft,
                "right" => MouseEventKind::ScrollRight,
                _ => bail!("scroll {argument:?}: expected up, down, left or right, then a place"),
            };
            Command::Mouse(Gesture::Scroll(kind), parse_where(verb, place.trim())?)
        }
        "drag" => match cells(argument).as_deref() {
            Some(&[x, y, to_x, to_y]) => Command::Drag([x, y], [to_x, to_y]),
            _ => bail!("drag {argument:?}: expected X Y X Y, from and to"),
        },
        other => bail!("unknown command {other:?}"),
    }))
}

/// `X Y`, or `on <substring>`.
fn parse_where(verb: &str, argument: &str) -> Result<Where> {
    if let Some(text) = argument.strip_prefix("on ") {
        return Ok(Where::On(text.trim().to_owned()));
    }
    match cells(argument).as_deref() {
        Some(&[x, y]) => Ok(Where::At(x, y)),
        _ => bail!("{verb} {argument:?}: expected X Y or on <substring>"),
    }
}

/// Whitespace-separated cell numbers, or `None` if any of them is not one.
fn cells(argument: &str) -> Option<Vec<u16>> {
    argument
        .split_whitespace()
        .map(|number| number.parse().ok())
        .collect()
}

fn parse_wait(argument: &str) -> Result<Wait> {
    if argument == "busy" {
        return Ok(Wait::Busy);
    }
    if argument == "text" {
        bail!("wait text needs a substring");
    }
    if let Some(text) = argument.strip_prefix("text ") {
        return Ok(Wait::Text(text.trim().to_owned()));
    }
    argument
        .parse::<u64>()
        .map(Wait::Millis)
        .map_err(|_| anyhow!("wait {argument:?}: expected busy, text <substring>, or milliseconds"))
}

/// A frame name is a file name, so it may not climb out of the frames
/// directory: a script is input like any other.
fn frame_name(name: &str) -> Result<String> {
    if name.starts_with('.') || name.contains(['/', '\\']) {
        bail!("{name:?} is not a frame name");
    }
    Ok(name.to_owned())
}

/// The events the command being run queued, and nothing else.
///
/// Coming up empty counts towards the loop's exhaustion, which is harmless
/// here: the replay takes the turns itself and says when the run is over.
#[derive(Debug, Default)]
struct Queue {
    pending: VecDeque<Event>,
}

impl InputSource for Queue {
    fn next(&mut self, _timeout: Duration) -> Result<Option<Event>> {
        Ok(self.pending.pop_front())
    }
}

/// One replay in progress.
struct Replay {
    terminal: Terminal<TestBackend>,
    app: App,
    driver: Driver,
    input: Queue,
    trace: Trace,
    frames: PathBuf,
    styles: bool,
    busy_timeout: Duration,
    text_timeout: Duration,
}

impl Replay {
    fn new(mut app: App, config: &Config, options: &Options) -> Result<Self> {
        let mut driver = Driver::new(options.theme, config);
        driver.set_max_rows(options.max_rows);
        driver.keep_scratch_in(options.store.clone());
        // A replay owns no terminal, so Ctrl-E says so rather than handing
        // over a screen it cannot take back.
        driver.without_terminal();
        driver.restore_scratch(&mut app);
        driver.connect_at_startup(options.connect.clone());
        Ok(Self {
            terminal: Terminal::new(TestBackend::new(options.size.cols, options.size.rows))
                .context("opening a test terminal")?,
            app,
            driver,
            input: Queue::default(),
            trace: Trace::from_env(),
            frames: options.frames.clone(),
            styles: options.styles,
            busy_timeout: options.busy_timeout,
            text_timeout: options.text_timeout,
        })
    }

    /// One command. `Some(code)` means the replay is over, and why.
    fn step(&mut self, command: &Command, line: usize) -> Result<Option<u8>> {
        match command {
            Command::Key(key) => self.send([Event::Key(*key)])?,
            Command::Type(text) => self.send(text.chars().map(|character| {
                Event::Key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE))
            }))?,
            Command::Paste(text) => self.send([Event::Paste(text.clone())])?,
            Command::Resize(size) => {
                self.terminal.backend_mut().resize(size.cols, size.rows);
                self.send([Event::Resize(size.cols, size.rows)])?;
            }
            Command::Frame(name) => self.write_frame(name)?,
            Command::Expect { text, present } => {
                let screen = self.screen_text();
                if screen.contains(text.as_str()) != *present {
                    let what = if *present { "expect" } else { "expect-not" };
                    eprintln!("sql-bench: line {line}: {what} {text:?} failed, on this frame:");
                    eprintln!("{screen}");
                    return Ok(Some(FAILED));
                }
            }
            Command::Wait(wait) => return self.wait(wait, line),
            Command::Mouse(gesture, place) => {
                let (x, y) = match place {
                    Where::At(x, y) => (*x, *y),
                    Where::On(text) => match find(self.screen(), text) {
                        Some(cell) => cell,
                        None => {
                            eprintln!("sql-bench: line {line}: {text:?} is not on this frame:");
                            eprintln!("{}", self.screen_text());
                            return Ok(Some(FAILED));
                        }
                    },
                };
                self.send(gesture.events(x, y))?;
            }
            Command::Drag([x, y], [to_x, to_y]) => {
                let at = |kind, column, row| {
                    Event::Mouse(MouseEvent {
                        kind,
                        column,
                        row,
                        modifiers: KeyModifiers::NONE,
                    })
                };
                let left = MouseButton::Left;
                self.send([
                    at(MouseEventKind::Down(left), *x, *y),
                    at(MouseEventKind::Drag(left), *to_x, *to_y),
                    at(MouseEventKind::Up(left), *to_x, *to_y),
                ])?;
            }
        }
        Ok(None)
    }

    /// Hand the loop some events and let it paint what they changed.
    fn send(&mut self, events: impl IntoIterator<Item = Event>) -> Result<()> {
        self.input.pending.extend(events);
        // A click the loop held back for the next frame is still to come.
        while (!self.input.pending.is_empty() || self.driver.held.is_some()) && self.turn()? {}
        // One more, because a turn draws what the turn before it changed.
        self.turn()?;
        Ok(())
    }

    fn turn(&mut self) -> Result<bool> {
        let turned = self.driver.turn(
            &mut self.terminal,
            &mut self.app,
            &mut self.input,
            &self.trace,
        );
        cover_wide_glyphs(self.terminal.backend_mut());
        turned
    }

    fn wait(&mut self, wait: &Wait, line: usize) -> Result<Option<u8>> {
        let (limit, what) = match wait {
            Wait::Millis(milliseconds) => {
                std::thread::sleep(Duration::from_millis(*milliseconds));
                self.turn()?;
                return Ok(None);
            }
            Wait::Busy => (self.busy_timeout, "wait busy".to_owned()),
            Wait::Text(text) => (self.text_timeout, format!("wait text {text:?}")),
        };
        let deadline = Instant::now() + limit;
        loop {
            let done = match wait {
                Wait::Busy => !self.app.busy(),
                Wait::Text(text) => self.screen_text().contains(text.as_str()),
                Wait::Millis(_) => true,
            };
            if done {
                return Ok(None);
            }
            if Instant::now() >= deadline {
                let frame = match self.write_frame("timeout") {
                    Ok(()) => self.frame_path("timeout").display().to_string(),
                    Err(error) => format!("not written ({error:#})"),
                };
                eprintln!(
                    "sql-bench: line {line}: {what} gave up after {limit:?}; the frame is {frame}"
                );
                return Ok(Some(TIMED_OUT));
            }
            // Turning the loop is what lets the app get on with whatever is
            // being waited for; the sleep is what keeps it from spinning.
            self.turn()?;
            std::thread::sleep(POLL);
        }
    }

    fn screen(&self) -> &Buffer {
        self.terminal.backend().buffer()
    }

    fn screen_text(&self) -> String {
        screen_text(self.screen())
    }

    fn frame_path(&self, name: &str) -> PathBuf {
        self.frames.join(format!("{name}.txt"))
    }

    fn write_frame(&self, name: &str) -> Result<()> {
        std::fs::create_dir_all(&self.frames)
            .with_context(|| format!("creating {}", self.frames.display()))?;
        let area = self.screen().area;
        let text = format!(
            "# {name} {}x{}\n{}\n",
            area.width,
            area.height,
            self.screen_text()
        );
        write(&self.frame_path(name), &text)?;
        if self.styles {
            write(
                &self.frames.join(format!("{name}.styles.txt")),
                &styles_text(name, self.screen()),
            )?;
        }
        Ok(())
    }
}

fn write(path: &Path, text: &str) -> Result<()> {
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

/// Does what a terminal does and `TestBackend` does not: a glyph two cells
/// wide covers the cell after it. ratatui never sends that cell, counting on
/// the terminal, so without this the cell keeps whatever an older frame left
/// there, and a row the screen shows as `李 雷` reads `李н雷а` to an `expect`.
fn cover_wide_glyphs(backend: &mut TestBackend) {
    use ratatui::backend::Backend as _;
    use unicode_width::UnicodeWidthStr as _;
    let buffer = backend.buffer();
    let area = buffer.area;
    let mut covered = Vec::new();
    for y in 0..area.height {
        let mut x = 0;
        while x < area.width {
            let width = u16::try_from(buffer[(x, y)].symbol().width()).unwrap_or(1);
            for under in x.saturating_add(1)..x.saturating_add(width).min(area.width) {
                if buffer[(under, y)].symbol() != " " {
                    let mut blank = buffer[(under, y)].clone();
                    blank.set_symbol(" ");
                    covered.push((under, y, blank));
                }
            }
            x = x.saturating_add(width.max(1));
        }
    }
    if !covered.is_empty() {
        let Ok(()) = backend.draw(covered.iter().map(|(x, y, cell)| (*x, *y, cell)));
    }
}

/// The frame as text: one line per row, the padding every row is right-filled
/// with taken off, every box-drawing character kept.
fn screen_text(buffer: &Buffer) -> String {
    let area = buffer.area;
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The cell `text` starts at, the first time it is on the frame.
///
/// The search goes cell by cell rather than through the frame's text, because
/// a glyph two cells wide is followed by a blank cell a person reading the
/// screen never sees — and would not type.
fn find(buffer: &Buffer, text: &str) -> Option<(u16, u16)> {
    use unicode_width::UnicodeWidthStr as _;
    let area = buffer.area;
    (0..area.height).find_map(|y| {
        let mut row = String::new();
        // Where in `row` each drawn cell's text starts.
        let mut starts = Vec::new();
        let mut x = 0;
        while x < area.width {
            let symbol = buffer[(x, y)].symbol();
            starts.push((row.len(), x));
            row.push_str(symbol);
            x = x.saturating_add(u16::try_from(symbol.width()).unwrap_or(1).max(1));
        }
        let at = row.find(text)?;
        starts
            .iter()
            .rev()
            .find(|(start, _)| *start <= at)
            .map(|(_, x)| (*x, y))
    })
}

/// The colours of a frame, as the runs of cells that share one style:
/// `<row> <from>..<to> fg=<colour> bg=<colour> mod=<modifiers>`.
fn styles_text(name: &str, buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut text = format!("# {name} {}x{} styles\n", area.width, area.height);
    let style = |x: u16, y: u16| {
        let cell = &buffer[(x, y)];
        (cell.fg, cell.bg, cell.modifier)
    };
    for y in 0..area.height {
        let mut start = 0;
        for x in 1..=area.width {
            if x < area.width && style(x, y) == style(start, y) {
                continue;
            }
            let (fg, bg, modifier) = style(start, y);
            let _ = writeln!(text, "{y} {start}..{x} fg={fg} bg={bg} mod={modifier:?}");
            start = x;
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{two_connections, two_tabs};
    use crate::app::{Focus, TabState};

    fn parsed(line: &str) -> Command {
        parse_line(line)
            .unwrap_or_else(|error| panic!("{line:?}: {error:#}"))
            .unwrap_or_else(|| panic!("{line:?} parsed as nothing"))
    }

    fn key(name: &str) -> KeyEvent {
        key_named(name).unwrap_or_else(|| panic!("{name:?} is not a key"))
    }

    /// The options a test runs with: colour pinned on, so the styles file is
    /// the same whether or not this machine sets `NO_COLOR`.
    fn options(directory: &Path) -> Options {
        Options {
            frames: directory.to_path_buf(),
            theme: Theme::new(false),
            // A test keeps its scratch pads to itself, whatever
            // `$SQL_BENCH_STATE_DIR` says on the machine running it.
            store: Store::new(Some(directory.to_path_buf())),
            ..Options::default()
        }
    }

    fn run(source: &str, app: App, options: &Options) -> u8 {
        run_script(source, app, &two_connections(), options).expect("the replay")
    }

    #[test]
    fn blank_lines_and_comments_are_not_commands() {
        for line in ["", "   ", "# a comment", "  # indented"] {
            assert_eq!(parse_line(line).expect("parsed"), None, "{line:?}");
        }
    }

    #[test]
    fn every_command_parses() {
        assert_eq!(parsed("key Enter"), Command::Key(key("Enter")));
        assert_eq!(
            parsed("type select 1"),
            Command::Type("select 1".to_owned())
        );
        assert_eq!(
            parsed("paste one  two"),
            Command::Paste("one  two".to_owned())
        );
        assert_eq!(parsed("wait busy"), Command::Wait(Wait::Busy));
        assert_eq!(parsed("wait 250"), Command::Wait(Wait::Millis(250)));
        assert_eq!(
            parsed("wait text 10 rows"),
            Command::Wait(Wait::Text("10 rows".to_owned()))
        );
        assert_eq!(parsed("frame help"), Command::Frame("help".to_owned()));
        assert_eq!(
            parsed("resize 80x24"),
            Command::Resize(Size { cols: 80, rows: 24 })
        );
        assert_eq!(
            parsed("expect  Objects "),
            Command::Expect {
                text: "Objects".to_owned(),
                present: true,
            }
        );
        assert_eq!(
            parsed("expect-not ╭ Help"),
            Command::Expect {
                text: "╭ Help".to_owned(),
                present: false,
            }
        );
        assert_eq!(
            parsed("click 3 0"),
            Command::Mouse(Gesture::Click, Where::At(3, 0))
        );
        assert_eq!(
            parsed("click on  2 local-oracle "),
            Command::Mouse(Gesture::Click, Where::On("2 local-oracle".to_owned()))
        );
        assert_eq!(
            parsed("double-click on Tables"),
            Command::Mouse(Gesture::DoubleClick, Where::On("Tables".to_owned()))
        );
        assert_eq!(
            parsed("right-click 10 5"),
            Command::Mouse(Gesture::RightClick, Where::At(10, 5))
        );
        assert_eq!(
            parsed("hover  0  0"),
            Command::Mouse(Gesture::Hover, Where::At(0, 0))
        );
        for (way, kind) in [
            ("up", MouseEventKind::ScrollUp),
            ("down", MouseEventKind::ScrollDown),
            ("left", MouseEventKind::ScrollLeft),
            ("right", MouseEventKind::ScrollRight),
        ] {
            assert_eq!(
                parsed(&format!("scroll {way} 40 20")),
                Command::Mouse(Gesture::Scroll(kind), Where::At(40, 20))
            );
        }
        assert_eq!(
            parsed("scroll down on Help"),
            Command::Mouse(
                Gesture::Scroll(MouseEventKind::ScrollDown),
                Where::On("Help".to_owned())
            )
        );
        assert_eq!(parsed("drag 1 2 30 4"), Command::Drag([1, 2], [30, 4]));
    }

    #[test]
    fn a_gesture_is_what_a_terminal_sends_for_it() {
        let kinds = |gesture: Gesture| -> Vec<MouseEventKind> {
            gesture
                .events(7, 3)
                .into_iter()
                .map(|event| match event {
                    Event::Mouse(mouse) => {
                        assert_eq!((mouse.column, mouse.row), (7, 3));
                        mouse.kind
                    }
                    other => panic!("{other:?} is not the mouse"),
                })
                .collect()
        };
        let (down, up) = (MouseEventKind::Down, MouseEventKind::Up);
        let (left, right) = (MouseButton::Left, MouseButton::Right);
        assert_eq!(kinds(Gesture::Click), [down(left), up(left)]);
        assert_eq!(
            kinds(Gesture::DoubleClick),
            [down(left), up(left), down(left), up(left)]
        );
        assert_eq!(kinds(Gesture::RightClick), [down(right), up(right)]);
        assert_eq!(kinds(Gesture::Hover), [MouseEventKind::Moved]);
        assert_eq!(
            kinds(Gesture::Scroll(MouseEventKind::ScrollLeft)),
            [MouseEventKind::ScrollLeft]
        );
    }

    #[test]
    fn a_wide_glyph_covers_what_an_older_frame_left_in_the_cell_after_it() {
        use ratatui::backend::Backend as _;
        use ratatui::buffer::Cell;
        let mut backend = TestBackend::new(4, 1);
        let old = [Cell::new("a"), Cell::new("j")];
        let Ok(()) = backend.draw([(0, 0, &old[0]), (1, 0, &old[1])].into_iter());
        // What ratatui sends for `山` over `aj`: the glyph, and not the cell
        // it covers.
        let wide = Cell::new("山");
        let Ok(()) = backend.draw([(0, 0, &wide)].into_iter());
        assert_eq!(screen_text(backend.buffer()), "山j", "the stale cell");
        cover_wide_glyphs(&mut backend);
        assert_eq!(screen_text(backend.buffer()), "山");
    }

    #[test]
    fn text_is_found_by_the_cell_it_starts_in_past_a_wide_glyph() {
        let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, 20, 3));
        buffer.set_string(2, 1, "表 orders", ratatui::style::Style::default());
        assert_eq!(find(&buffer, "orders"), Some((5, 1)));
        assert_eq!(
            find(&buffer, "表 orders"),
            Some((2, 1)),
            "no blank typed after it"
        );
        assert_eq!(find(&buffer, "customers"), None);
    }

    #[test]
    fn typing_keeps_its_spaces_and_every_other_argument_is_trimmed() {
        assert_eq!(
            parsed("type  select  1 "),
            Command::Type(" select  1 ".to_owned())
        );
        assert_eq!(parsed("frame  help "), Command::Frame("help".to_owned()));
        assert_eq!(parsed("  key q"), Command::Key(key("q")));
    }

    #[test]
    fn every_key_name_is_the_key_crossterm_sends() {
        let plain = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert_eq!(key("Enter"), plain(KeyCode::Enter));
        assert_eq!(key("Esc"), plain(KeyCode::Esc));
        assert_eq!(key("Tab"), plain(KeyCode::Tab));
        assert_eq!(key("BackTab"), plain(KeyCode::BackTab));
        assert_eq!(
            key("Shift-Tab"),
            plain(KeyCode::BackTab),
            "one key, two spellings"
        );
        assert_eq!(key("Up"), plain(KeyCode::Up));
        assert_eq!(key("Down"), plain(KeyCode::Down));
        assert_eq!(key("Left"), plain(KeyCode::Left));
        assert_eq!(key("Right"), plain(KeyCode::Right));
        assert_eq!(key("PageUp"), plain(KeyCode::PageUp));
        assert_eq!(key("PageDown"), plain(KeyCode::PageDown));
        assert_eq!(key("Home"), plain(KeyCode::Home));
        assert_eq!(key("End"), plain(KeyCode::End));
        assert_eq!(key("Backspace"), plain(KeyCode::Backspace));
        assert_eq!(key("Delete"), plain(KeyCode::Delete));
        assert_eq!(key("Insert"), plain(KeyCode::Insert));
        assert_eq!(key("Space"), plain(KeyCode::Char(' ')));
        for number in 1..=12u8 {
            assert_eq!(key(&format!("F{number}")), plain(KeyCode::F(number)));
        }
        assert_eq!(key("q"), plain(KeyCode::Char('q')));
        assert_eq!(key("?"), plain(KeyCode::Char('?')));
        assert_eq!(key("1"), plain(KeyCode::Char('1')));
        assert_eq!(key("F"), plain(KeyCode::Char('F')), "F alone is a letter");
        assert_eq!(
            key("Ctrl-r"),
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)
        );
        assert_eq!(
            key("Ctrl-Q"),
            KeyEvent::new(KeyCode::Char('Q'), KeyModifiers::CONTROL)
        );
        assert_eq!(
            key("Alt-x"),
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT)
        );
        assert_eq!(
            key("Ctrl-F5"),
            KeyEvent::new(KeyCode::F(5), KeyModifiers::CONTROL)
        );
    }

    #[test]
    fn bad_input_names_the_line_and_says_what_is_wrong() {
        for (source, wanted) in [
            ("key q\nkye q\n", "line 2"),
            ("key\n", "key needs a key name"),
            ("key Meta\n", "unknown key \"Meta\""),
            ("key F13\n", "unknown key \"F13\""),
            ("key ctrl-q\n", "unknown key \"ctrl-q\""),
            ("wait soon\n", "expected busy, text <substring>"),
            ("wait text\n", "wait text needs a substring"),
            ("resize 80\n", "expected COLSxROWS"),
            ("expect\n", "expect needs a substring"),
            ("expect-not\n", "expect-not needs a substring"),
            ("frame\n", "frame needs a name"),
            ("frame ../escape\n", "is not a frame name"),
            ("frame a/b\n", "is not a frame name"),
            ("type\n", "type needs something to type"),
            ("paste\n", "paste needs something to paste"),
            ("click\n", "click \"\": expected X Y or on <substring>"),
            ("click 3\n", "click \"3\": expected X Y or on <substring>"),
            ("click 3 4 5\n", "expected X Y or on <substring>"),
            ("click -1 0\n", "expected X Y or on <substring>"),
            ("click on\n", "expected X Y or on <substring>"),
            ("double-click x y\n", "double-click \"x y\": expected X Y"),
            ("right-click\n", "right-click \"\": expected X Y"),
            ("hover 70000 0\n", "hover \"70000 0\": expected X Y"),
            ("scroll\n", "expected up, down, left or right"),
            ("scroll sideways 1 1\n", "expected up, down, left or right"),
            ("scroll up\n", "scroll \"\": expected X Y or on <substring>"),
            ("drag 1 2 3\n", "drag \"1 2 3\": expected X Y X Y"),
            ("drag on Tables\n", "expected X Y X Y"),
            ("dance\n", "unknown command \"dance\""),
        ] {
            let error = format!("{:#}", parse(source).expect_err(source));
            assert!(error.contains(wanted), "{source:?} said {error:?}");
            assert!(error.contains("line "), "{source:?} said {error:?}");
        }
    }

    #[test]
    fn a_script_drives_the_real_loop_and_writes_the_frames_it_names() {
        let directory = tempfile::tempdir().expect("a directory");
        let code = run(
            "# the smoke path\n\
             expect 1 local-mssql\n\
             key Tab\n\
             expect ╭ Scratch\n\
             key ?\n\
             expect Ctrl-T        next tab\n\
             frame help\n\
             key Esc\n\
             expect-not ╭ Help\n\
             key q\n",
            two_tabs(),
            &options(directory.path()),
        );
        assert_eq!(code, OK);
        let written = std::fs::read_to_string(directory.path().join("help.txt")).expect("a frame");
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(lines[0], "# help 120x40");
        assert_eq!(
            lines[1],
            format!(" 1 local-mssql ○  2 local-oracle ○{:79}? Help", "")
        );
        assert_eq!(lines.len(), 41, "a header and one line per row");
        assert!(written.contains("╭ Help "), "{written}");
        assert!(
            lines.iter().all(|line| !line.ends_with(' ')),
            "a row was left padded:\n{written}"
        );
    }

    #[test]
    fn a_replay_stops_where_the_app_did() {
        let directory = tempfile::tempdir().expect("a directory");
        let mut replay = Replay::new(two_tabs(), &two_connections(), &options(directory.path()))
            .expect("a replay");
        replay.turn().expect("the first frame");
        assert_eq!(replay.step(&parsed("key Tab"), 1).expect("a key"), None);
        assert_eq!(replay.app.shell.focus, Focus::Scratch);
        assert_eq!(replay.step(&parsed("key Ctrl-Q"), 2).expect("a key"), None);
        assert!(
            replay.app.shell.should_quit,
            "q in the scratch pad is typed"
        );
    }

    #[test]
    fn a_click_on_a_tab_shows_that_tab() {
        let directory = tempfile::tempdir().expect("a directory");
        let mut app = two_tabs();
        app.tabs[1].state = TabState::Failed("listener refused".to_owned());
        let code = run(
            "expect ○ disconnected\n\
             click on 2 local-oracle\n\
             expect ✗ failed\n\
             expect listener refused\n\
             click 1 0\n\
             expect ○ disconnected\n\
             key q\n",
            app,
            &options(directory.path()),
        );
        assert_eq!(code, OK);
        assert_eq!(
            run(
                "click on nothing says this\n",
                two_tabs(),
                &options(directory.path())
            ),
            FAILED,
            "text that is not there is a failed expectation"
        );
    }

    #[test]
    fn a_drag_moves_a_seam() {
        let directory = tempfile::tempdir().expect("a directory");
        let code = run(
            "drag 36 10 50 10\n\
             frame dragged\n\
             key q\n",
            two_tabs(),
            &options(directory.path()),
        );
        assert_eq!(code, OK);
        let written =
            std::fs::read_to_string(directory.path().join("dragged.txt")).expect("a frame");
        let top = written.lines().nth(2).expect("the panes' top row");
        assert_eq!(top.chars().position(|c| c == '╭'), Some(0), "{written}");
        assert_eq!(
            top.chars().skip(1).position(|c| c == '╭'),
            Some(49),
            "Scratch starts at column 50: {written}"
        );
    }

    #[test]
    fn a_config_with_no_connections_replays_too() {
        let directory = tempfile::tempdir().expect("a directory");
        let code = run(
            "expect No connections yet.\nexpect q quits.\nkey q\n",
            App::new(&Config::default()),
            &options(directory.path()),
        );
        assert_eq!(code, OK);
    }

    #[test]
    fn typing_pasting_and_resizing_all_reach_the_app() {
        let directory = tempfile::tempdir().expect("a directory");
        let code = run(
            "key Tab\n\
             type select 1\n\
             paste and this too\n\
             wait 1\n\
             resize 60x15\n\
             expect ╭ Objects\n\
             frame small\n\
             key Ctrl-Q\n",
            App::new(&two_connections()),
            &options(directory.path()),
        );
        assert_eq!(code, OK);
        let written = std::fs::read_to_string(directory.path().join("small.txt")).expect("a frame");
        assert!(written.starts_with("# small 60x15\n"), "{written}");
    }

    #[test]
    fn a_pad_is_written_on_the_way_out_and_is_there_again_next_time() {
        let directory = tempfile::tempdir().expect("a directory");
        let options = options(directory.path());
        assert_eq!(
            run(
                "key Tab\n\
                 type select 1 from bench.events\n\
                 key Ctrl-Q\n",
                App::new(&two_connections()),
                &options,
            ),
            OK
        );
        assert_eq!(
            std::fs::read_to_string(directory.path().join("scratch/local-mssql.sql"))
                .expect("the pad"),
            "select 1 from bench.events\n"
        );
        assert_eq!(
            run(
                "expect select 1 from bench.events\nkey Ctrl-Q\n",
                App::new(&two_connections()),
                &options,
            ),
            OK,
            "the pad is on the screen before a key is pressed"
        );
    }

    #[test]
    fn the_pads_keys_do_what_they_say_and_ctrl_c_never_quits() {
        let directory = tempfile::tempdir().expect("a directory");
        let code = run(
            "key Tab\n\
             type select 1\n\
             key Ctrl-C\n\
             expect select 1\n\
             key Shift-Left\n\
             key Ctrl-C\n\
             expect copied 1 characters\n\
             key Ctrl-R\n\
             expect connecting… then running\n\
             key Ctrl-E\n\
             expect editor unavailable in replay\n\
             key Ctrl-Q\n",
            App::new(&two_connections()),
            &options(directory.path()),
        );
        assert_eq!(code, OK);
    }

    #[test]
    fn an_expectation_that_fails_is_exit_4() {
        let directory = tempfile::tempdir().expect("a directory");
        assert_eq!(
            run(
                "expect nothing says this\n",
                two_tabs(),
                &options(directory.path())
            ),
            FAILED
        );
        assert_eq!(
            run(
                "expect-not 1 local-mssql\n",
                two_tabs(),
                &options(directory.path())
            ),
            FAILED
        );
    }

    #[test]
    fn a_wait_that_never_ends_is_exit_3_and_a_frame_called_timeout() {
        let directory = tempfile::tempdir().expect("a directory");
        let short = Options {
            busy_timeout: Duration::from_millis(30),
            text_timeout: Duration::from_millis(30),
            ..options(directory.path())
        };
        let mut app = two_tabs();
        app.tabs[0].state = TabState::Connecting;
        assert!(app.busy(), "nothing else makes this app busy yet");
        assert_eq!(run("wait busy\n", app, &short), TIMED_OUT);
        let written =
            std::fs::read_to_string(directory.path().join("timeout.txt")).expect("the frame");
        assert!(written.starts_with("# timeout 120x40\n"), "{written}");

        assert_eq!(
            run("wait text NEVER_THERE\n", two_tabs(), &short),
            TIMED_OUT
        );
    }

    #[test]
    fn a_wait_whose_condition_already_holds_returns_at_once() {
        let directory = tempfile::tempdir().expect("a directory");
        let started = Instant::now();
        assert_eq!(
            run(
                "wait busy\nwait text local-mssql\nkey q\n",
                two_tabs(),
                &options(directory.path())
            ),
            OK
        );
        assert!(started.elapsed() < Duration::from_secs(5), "it waited");
    }

    #[test]
    fn frame_styles_writes_the_colours_beside_the_frame() {
        let directory = tempfile::tempdir().expect("a directory");
        let code = run(
            "frame first\nkey q\n",
            two_tabs(),
            &Options {
                styles: true,
                ..options(directory.path())
            },
        );
        assert_eq!(code, OK);
        let written =
            std::fs::read_to_string(directory.path().join("first.styles.txt")).expect("the styles");
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(lines[0], "# first 120x40 styles");
        assert_eq!(lines[1], "0 0..1 fg=Reset bg=Reset mod=NONE");
        // The tab showing, in the accent colour, up to the gap before the next.
        assert!(
            lines[1].starts_with("0 0..") && lines[2].ends_with("fg=Cyan bg=Reset mod=BOLD"),
            "{}",
            lines[..4].join("\n")
        );
    }
}
