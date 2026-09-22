//! Pure state: tabs, focus, the scratch pad, the result grid and the object
//! tree. Nothing here touches the terminal, the clock or a database — an
//! event goes in, a state change and a list of [`Action`]s come out, which is
//! what makes the whole app testable without either.

pub mod finder;
pub mod objects;
pub mod pointer;
pub mod prompt;
pub mod results;
pub mod scratch;

#[cfg(test)]
pub(crate) mod tests;

use std::ops::Range;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Size;

use crate::config::{Config, Kind};
use crate::db::catalog::{CatalogAnswer, CatalogRequest, ObjectKind};
use crate::db::model::{DbError, QueryEvent};
use finder::Finder;
use objects::Objects;
use prompt::Prompt;
use results::{Hit, Inspector, Results};
use scratch::{Outcome, Scratch};

/// Every key this build handles: the key, where it works, what it does.
///
/// The help overlay, the footer hints and the key test all read this table
/// and nothing else, so a key that is not in it is a key nobody is told
/// about — and a key in it that [`App::handle`] ignores fails the test.
/// `where` is [`ANYWHERE`], [`NOT_SCRATCH`] for a key the pad types instead,
/// or [`SCRATCH`] for one that is the pad's own.
pub const KEYS: &[(&str, &str, &str)] = &[
    ("Tab", NOT_SCRATCH, "next pane"),
    ("Shift-Tab", ANYWHERE, "previous pane"),
    ("Ctrl-T", ANYWHERE, "next tab"),
    ("1-9", NOT_SCRATCH, "select tab"),
    ("c", NOT_SCRATCH, "connect"),
    ("C", NOT_SCRATCH, "disconnect"),
    ("Ctrl-R", SCRATCH, "run the statement"),
    ("F5", SCRATCH, "run all"),
    ("Ctrl-E", SCRATCH, "edit in $EDITOR"),
    ("Ctrl-Z", SCRATCH, "undo the last edits"),
    ("Ctrl-C", SCRATCH, "copy the selection"),
    ("Shift-Arrows", SCRATCH, "select"),
    ("Tab", SCRATCH, "two spaces"),
    ("Home", SCRATCH, "line start"),
    ("End", SCRATCH, "line end"),
    ("PageDown", SCRATCH, "page down"),
    ("PageUp", SCRATCH, "page up"),
    ("Ctrl-A", SCRATCH, "line start"),
    ("Ctrl-U", SCRATCH, "delete to line start"),
    ("Ctrl-K", SCRATCH, "delete to line end"),
    ("Ctrl-W", SCRATCH, "delete the word before"),
    ("Ctrl-Left", SCRATCH, "word left"),
    ("Ctrl-Right", SCRATCH, "word right"),
    ("?", ANYWHERE, "help"),
    ("Ctrl-P", ANYWHERE, "find an object"),
    ("Esc", ANYWHERE, "cancel or close help"),
    ("q", NOT_SCRATCH, "quit"),
    ("Ctrl-Q", ANYWHERE, "quit"),
    ("j", RESULTS, "row down"),
    ("k", RESULTS, "row up"),
    ("h", RESULTS, "column left"),
    ("l", RESULTS, "column right"),
    ("Arrows", RESULTS, "move the cell cursor"),
    ("PageDown", RESULTS, "page down"),
    ("PageUp", RESULTS, "page up"),
    ("Ctrl-D", RESULTS, "half a page down"),
    ("Ctrl-U", RESULTS, "half a page up"),
    ("g", RESULTS, "first row"),
    ("G", RESULTS, "last row"),
    ("0", RESULTS, "first column"),
    ("$", RESULTS, "last column"),
    ("[", RESULTS, "previous result set"),
    ("]", RESULTS, "next result set"),
    ("m", RESULTS, "10,000 more rows"),
    ("Enter", RESULTS, "inspect the cell"),
    ("y", RESULTS, "copy the cell"),
    ("Y", RESULTS, "copy the row"),
    ("e", RESULTS, "export the result set"),
    ("o", RESULTS, "sort by the column"),
    ("j", OBJECTS, "down"),
    ("k", OBJECTS, "up"),
    ("l", OBJECTS, "expand or open"),
    ("h", OBJECTS, "collapse or go up"),
    ("Space", OBJECTS, "expand or collapse"),
    ("Enter", OBJECTS, "select from it"),
    ("s", OBJECTS, "source"),
    ("i", OBJECTS, "columns"),
    ("r", OBJECTS, "reload"),
    ("/", OBJECTS, "find in every schema"),
    ("y", OBJECTS, "copy the name"),
    ("Arrows", OBJECTS, "move about the tree"),
    ("g", OBJECTS, "first row"),
    ("G", OBJECTS, "last row"),
    ("PageDown", OBJECTS, "page down"),
    ("PageUp", OBJECTS, "page up"),
];

pub const ANYWHERE: &str = "anywhere";

pub const NOT_SCRATCH: &str = "not Scratch";

pub const SCRATCH: &str = "Scratch";

pub const RESULTS: &str = "Results";

pub const OBJECTS: &str = "Objects";

/// The frames a connecting tab's mark cycles through, one every
/// [`SPIN_EVERY`].
pub const SPINNER: [&str; 4] = ["⠋", "⠙", "⠹", "⠸"];

/// How often the spinner moves on. Fast enough to read as motion, slow
/// enough that a connecting app costs ten frames a second and not more.
pub const SPIN_EVERY: Duration = Duration::from_millis(100);

/// How far PageUp and PageDown move the help overlay. The app never sees
/// the terminal, so this is a page of the smallest one it supports; the
/// overlay clamps the offset to the rows it can actually show.
const HELP_PAGE: usize = 10;

/// The rows of [`KEYS`] that work while `focus` has the focus.
pub fn keys_for(
    focus: Focus,
) -> impl Iterator<Item = &'static (&'static str, &'static str, &'static str)> {
    KEYS.iter().filter(move |(_, place, _)| {
        let typing = focus == Focus::Scratch;
        match *place {
            SCRATCH => typing,
            RESULTS => focus == Focus::Results,
            OBJECTS => focus == Focus::Objects,
            NOT_SCRATCH => !typing,
            _ => true,
        }
    })
}

/// A key the way [`KEYS`], the help overlay and `docs/DESIGN.md` spell one:
/// `Enter`, `F5`, `q`, and any of them after `Ctrl-`, `Alt-` or `Shift-`.
///
/// Replay scripts and the tests both read keys through this, so a key name
/// means the same thing wherever it is written.
pub(crate) fn key_named(name: &str) -> Option<KeyEvent> {
    let (modifiers, base) = match name.split_once('-') {
        Some(("Ctrl", rest)) => (KeyModifiers::CONTROL, rest),
        Some(("Alt", rest)) => (KeyModifiers::ALT, rest),
        Some(("Shift", rest)) => (KeyModifiers::SHIFT, rest),
        _ => (KeyModifiers::NONE, name),
    };
    let code = match base {
        "Enter" => KeyCode::Enter,
        "Esc" => KeyCode::Esc,
        // Shift-Tab is what a keyboard calls it and BackTab is what crossterm
        // sends; both spellings are the one key.
        "Tab" if modifiers == KeyModifiers::SHIFT => KeyCode::BackTab,
        "Tab" => KeyCode::Tab,
        "BackTab" => KeyCode::BackTab,
        "Up" => KeyCode::Up,
        "Down" => KeyCode::Down,
        "Left" => KeyCode::Left,
        "Right" => KeyCode::Right,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "Backspace" => KeyCode::Backspace,
        "Delete" => KeyCode::Delete,
        "Insert" => KeyCode::Insert,
        "Space" => KeyCode::Char(' '),
        other => function_key(other).or_else(|| character_key(other))?,
    };
    // BackTab is already the shifted key, and saying so twice is how a key the
    // app matches on stops matching.
    let modifiers = if code == KeyCode::BackTab {
        modifiers.difference(KeyModifiers::SHIFT)
    } else {
        modifiers
    };
    Some(KeyEvent::new(code, modifiers))
}

fn function_key(name: &str) -> Option<KeyCode> {
    let number: u8 = name.strip_prefix('F')?.parse().ok()?;
    (1..=12).contains(&number).then_some(KeyCode::F(number))
}

fn character_key(name: &str) -> Option<KeyCode> {
    let mut characters = name.chars();
    match (characters.next(), characters.next()) {
        (Some(character), None) => Some(KeyCode::Char(character)),
        _ => None,
    }
}

/// What the app asks the run loop to do. The app itself never does IO, so
/// everything with a side effect leaves through here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    Quit,
    /// Open (or re-open) this tab's connection.
    Connect(usize),
    /// Close it, cancelling whatever it is running.
    Disconnect(usize),
    /// Run the statement the scratch pad's cursor is in *(T5.2)*.
    RunStatement {
        tab: usize,
        sql: String,
    },
    /// Run every statement in the pad, in order *(T5.2)*.
    RunAll {
        tab: usize,
        statements: Vec<String>,
    },
    /// Hand this tab's pad to `$VISUAL` or `$EDITOR` and take back what it
    /// saves.
    OpenEditor {
        tab: usize,
    },
    /// Write this tab's pad to its file: the settle after the last edit, and
    /// once more on the way out.
    SaveScratch {
        tab: usize,
    },
    /// Stop whatever this tab is running.
    Cancel(usize),
    /// Run the last statement again with ten thousand more rows allowed.
    MoreRows {
        tab: usize,
    },
    /// Best effort, into the terminal's clipboard.
    Copy(String),
    /// Write the result set on screen where the prompt said: JSON for a
    /// `.json` name, CSV for anything else.
    Export {
        tab: usize,
        path: String,
    },
    /// Fill a branch of the object tree, or show what an object is made of.
    LoadObjects {
        tab: usize,
        request: CatalogRequest,
    },
    /// Nothing to do: `o` sorted this many rows while the event was being
    /// handled, which the run loop times for the trace because the app
    /// reads no clock.
    Sorted {
        rows: usize,
    },
}

/// What the run loop reports back about a connection. The app never opens
/// one, so this is the only way a tab moves off [`TabState::Connecting`] —
/// and it is what makes those transitions testable without a database.
#[derive(Clone, Debug, PartialEq)]
pub enum RuntimeEvent {
    Connecting {
        tab: usize,
    },
    Connected {
        tab: usize,
        connect_ms: u32,
    },
    Failed {
        tab: usize,
        message: String,
    },
    Disconnected {
        tab: usize,
    },
    /// A statement has been handed to the driver: which one of how many, and
    /// when — the app reads no clock of its own for the running timer.
    /// `keep_view` is `m` asking for more rows of the same statement.
    QueryStarted {
        tab: usize,
        at: Instant,
        statement: usize,
        of: usize,
        keep_view: bool,
    },
    /// One event from the query that tab is running.
    Query {
        tab: usize,
        event: QueryEvent,
    },
    /// What a catalog query this tab asked for came back with.
    Catalog {
        tab: usize,
        request: CatalogRequest,
        result: Result<CatalogAnswer, DbError>,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Focus {
    #[default]
    Objects,
    Scratch,
    Results,
}

impl Focus {
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Objects => Self::Scratch,
            Self::Scratch => Self::Results,
            Self::Results => Self::Objects,
        }
    }

    #[must_use]
    pub const fn previous(self) -> Self {
        match self {
            Self::Objects => Self::Results,
            Self::Scratch => Self::Objects,
            Self::Results => Self::Scratch,
        }
    }

    /// The pane's title, which is also how [`KEYS`] names it.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Objects => "Objects",
            Self::Scratch => "Scratch",
            Self::Results => "Results",
        }
    }
}

/// Where one tab's connection is. Only [`App::apply`] moves it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum TabState {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Failed(String),
}

impl TabState {
    /// The glyph the tab bar marks a tab with.
    #[must_use]
    pub const fn mark(&self) -> &'static str {
        match self {
            Self::Disconnected => "○",
            // The still frame; [`Shell::mark`] is the moving one.
            Self::Connecting => SPINNER[0],
            Self::Connected => "●",
            Self::Failed(_) => "✗",
        }
    }

    /// What the footer's right end says about it.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Failed(_) => "failed",
        }
    }
}

/// One tab: a connection from `config.toml` and where it is.
#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    pub name: String,
    pub kind: Kind,
    pub state: TabState,
    /// How long the connection that is up took to open.
    pub connect_ms: Option<u32>,
    /// The SQL this connection is being written against, loaded from and
    /// saved to `<state dir>/scratch/<name>.sql`.
    pub scratch: Scratch,
    /// What the last statement run on this tab returned.
    pub results: Results,
    /// What this connection holds, as far as it has been asked.
    pub objects: Objects,
}

/// The state every pane shares.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Shell {
    pub active_tab: usize,
    pub focus: Focus,
    /// A passing message for the footer; empty most of the time.
    pub status: String,
    /// A failure the footer shows until Esc, in the error colour.
    pub error: Option<String>,
    pub should_quit: bool,
    pub size: Size,
    /// Whether the help overlay is open.
    pub help: bool,
    /// The open cell inspector, if Enter opened one.
    pub inspector: Option<Inspector>,
    /// The open footer prompt, if `e` opened one. While it is there every
    /// key is a key it is being typed with.
    pub prompt: Option<Prompt>,
    /// The open finder, if Ctrl-P opened one. It takes every key but Ctrl-Q
    /// for the same reason.
    pub finder: Option<Finder>,
    /// The first row of [`keys_for`] the open overlay shows. `?` and Esc put
    /// it back to the top.
    pub help_scroll: usize,
    /// Which frame of [`SPINNER`] a connecting tab is showing.
    pub spinner: usize,
    /// What Ctrl-C last copied. The terminal's own clipboard is a best
    /// effort the run loop makes; this one is always there.
    pub clipboard: String,
    /// Where the pointer is and what a button held down is pressing.
    pub mouse: pointer::Mouse,
    /// Where the seams between the panes are.
    pub split: pointer::Split,
    /// When that frame went up. The clock comes from the caller, so this
    /// module still reads none of its own.
    spun_at: Option<Instant>,
}

impl Shell {
    /// Move the spinner on if it is `spinning` and a frame is due, and say
    /// whether the screen has to be painted again.
    ///
    /// An app with nothing connecting never returns `true`, which is what
    /// keeps an idle run at zero frames.
    pub fn tick(&mut self, now: Instant, spinning: bool) -> bool {
        if !spinning {
            self.spun_at = None;
            return false;
        }
        if self
            .spun_at
            .is_some_and(|at| now.duration_since(at) < SPIN_EVERY)
        {
            return false;
        }
        self.spun_at = Some(now);
        self.spinner = self.spinner.wrapping_add(1);
        true
    }

    /// The glyph for a tab in this state, this spinner frame included.
    #[must_use]
    pub fn mark(&self, state: &TabState) -> &'static str {
        match state {
            TabState::Connecting => SPINNER[self.spinner % SPINNER.len()],
            other => other.mark(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct App {
    pub shell: Shell,
    pub tabs: Vec<Tab>,
}

impl App {
    /// One tab per configured connection, all of them disconnected. No
    /// connections is a valid app: it is what a first run has, and the UI
    /// says where the config goes.
    #[must_use]
    pub fn new(config: &Config) -> Self {
        Self {
            shell: Shell::default(),
            tabs: config
                .connections
                .iter()
                .map(|connection| Tab {
                    name: connection.name.clone(),
                    kind: connection.kind,
                    state: TabState::default(),
                    connect_ms: None,
                    scratch: Scratch::default(),
                    results: Results::default(),
                    objects: Objects::new(connection.kind, &connection.user),
                })
                .collect(),
        }
    }

    /// The tab whose panes are on screen, if there is one.
    #[must_use]
    pub fn tab(&self) -> Option<&Tab> {
        self.tabs.get(self.shell.active_tab)
    }

    /// Whether anything is still in flight: a tab connecting, or a query
    /// running. This is what a replay's `wait busy` waits out, so it answers
    /// for the whole app and not just the tab on screen.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.connecting() || self.running() || self.loading()
    }

    /// Whether any tab is waiting on a catalog query.
    #[must_use]
    pub fn loading(&self) -> bool {
        self.tabs.iter().any(|tab| tab.objects.busy())
    }

    /// Whether any tab has a query in flight.
    #[must_use]
    pub fn running(&self) -> bool {
        self.tabs.iter().any(|tab| tab.results.running())
    }

    /// Whether any tab is connecting, which is the only thing that animates.
    #[must_use]
    pub fn connecting(&self) -> bool {
        self.tabs
            .iter()
            .any(|tab| tab.state == TabState::Connecting)
    }

    /// What the run loop found out about a connection.
    ///
    /// The footer says which tab it was, because the tab it happened to is
    /// not always the tab on screen.
    pub fn apply(&mut self, event: RuntimeEvent) {
        let (index, state, connect_ms) = match event {
            RuntimeEvent::QueryStarted {
                tab,
                at,
                statement,
                of,
                keep_view,
            } => {
                let Some(open) = self.tabs.get_mut(tab) else {
                    return;
                };
                open.results.start(at, statement, of, keep_view);
                open.scratch.flag(None);
                self.shell.status = if of > 1 {
                    format!("running statement {} of {of}", statement + 1)
                } else {
                    "running…".to_owned()
                };
                return;
            }
            RuntimeEvent::Query { tab, event } => return self.query_event(tab, event),
            RuntimeEvent::Catalog {
                tab,
                request,
                result,
            } => return self.catalog_event(tab, &request, &result),
            RuntimeEvent::Connecting { tab } => (tab, TabState::Connecting, None),
            RuntimeEvent::Connected { tab, connect_ms } => {
                (tab, TabState::Connected, Some(connect_ms))
            }
            RuntimeEvent::Failed { tab, message } => (tab, TabState::Failed(message), None),
            RuntimeEvent::Disconnected { tab } => (tab, TabState::Disconnected, None),
        };
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        // Whatever the tree held was that connection's; the run loop asks
        // for the schemas again as soon as this one is up.
        tab.objects.clear();
        tab.state = state;
        tab.connect_ms = connect_ms;
        self.shell.status = format!(
            "{} {} {}{}",
            tab.state.mark(),
            tab.name,
            tab.state.label(),
            connect_ms
                .map(|ms| format!(" in {ms} ms"))
                .unwrap_or_default(),
        );
        // An open finder loses that tab's objects with the connection.
        if let Some(finder) = self.shell.finder.as_mut() {
            finder.search(&self.tabs);
        }
    }

    /// One event of a running query. When it is the last one, the pad is
    /// told which statement failed — it highlights those lines until the
    /// next edit — and the footer says which of a run of several it was.
    fn query_event(&mut self, tab: usize, event: QueryEvent) {
        let last = matches!(event, QueryEvent::Done { .. } | QueryEvent::Error(_));
        let Some(open) = self.tabs.get_mut(tab) else {
            return;
        };
        open.results.apply(event);
        if !last {
            return;
        }
        let (ran, of) = open.results.progress();
        if open.results.failure().is_some() {
            let lines = open.results.statement_lines();
            open.scratch.flag(lines);
            self.shell.status = if of > 1 {
                format!("statement {ran} of {of} failed")
            } else {
                String::new()
            };
        } else {
            self.shell.status.clear();
        }
    }

    /// One event, turned into state changes and whatever has to happen
    /// outside the app.
    pub fn handle(&mut self, event: Event) -> Vec<Action> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.key(key),
            Event::Paste(text) if self.shell.focus == Focus::Scratch => {
                if let Some(tab) = self.tabs.get_mut(self.shell.active_tab) {
                    tab.scratch.paste(&text);
                }
                Vec::new()
            }
            Event::Resize(columns, rows) => {
                self.shell.size = Size::new(columns, rows);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// The clock, once a turn: what the pads that have stopped being typed
    /// into owe the disk.
    ///
    /// The app reads no clock of its own, so this is how a save 500 ms after
    /// the last edit happens without one — and [`App::settling`] is what
    /// keeps the loop turning long enough for it to come round.
    pub fn settle(&mut self, now: Instant) -> Vec<Action> {
        (0..self.tabs.len())
            .filter(|tab| self.tabs[*tab].scratch.settle(now))
            .map(|tab| Action::SaveScratch { tab })
            .collect()
    }

    /// Whether any pad is waiting to be written.
    #[must_use]
    pub fn settling(&self) -> bool {
        self.tabs.iter().any(|tab| tab.scratch.settling())
    }

    fn key(&mut self, key: KeyEvent) -> Vec<Action> {
        // A menu is only ever open on its own, and it is what the keys were
        // aimed at, so it takes every one — Esc before anything else Esc does.
        if self.shell.mouse.menu.is_some() {
            return self.menu_key(key);
        }
        // An open prompt is being typed into, so it takes every key before
        // anything else can claim one — `?` and `q` included.
        if self.shell.prompt.is_some() {
            return self.prompt_key(key);
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.shell.finder.is_some() && !(control && matches!(key.code, KeyCode::Char('q' | 'Q')))
        {
            return self.finder_key(key);
        }
        if self.shell.help && self.help_key(key) {
            return Vec::new();
        }
        if self.shell.inspector.is_some() && self.inspector_key(key) {
            return Vec::new();
        }
        // The scratch pad types every key the shell does not keep for
        // itself, which is why the shell's keys are matched first.
        let typing = self.shell.focus == Focus::Scratch;
        // So does the filter line, once `/` has opened it: `c` is a letter
        // of a table's name there and not a connect key.
        let filtering = self.shell.focus == Focus::Objects
            && self.tab().is_some_and(|tab| tab.objects.filtering());
        match key.code {
            KeyCode::Char('q' | 'Q') if control => return vec![Action::Quit],
            KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Esc | KeyCode::Enter
                if filtering && !control =>
            {
                return self.objects_key(key);
            }
            KeyCode::Char('t' | 'T') if control => {
                if !self.tabs.is_empty() {
                    return self.open_tab((self.shell.active_tab + 1) % self.tabs.len());
                }
            }
            KeyCode::Char('p' | 'P') if control => {
                self.shell.finder = Some(Finder::open(&self.tabs));
            }
            KeyCode::Tab if !typing => self.shell.focus = self.shell.focus.next(),
            KeyCode::BackTab => self.shell.focus = self.shell.focus.previous(),
            KeyCode::Char('?') => {
                self.shell.help = !self.shell.help;
                self.shell.help_scroll = 0;
            }
            KeyCode::Esc => {
                if self.shell.help {
                    self.close_help();
                } else if self.shell.inspector.is_some() {
                    self.shell.inspector = None;
                } else if self.tab().is_some_and(|tab| tab.results.running()) {
                    return vec![Action::Cancel(self.shell.active_tab)];
                } else if self.shell.focus == Focus::Objects
                    && self
                        .tab()
                        .is_some_and(|tab| !tab.objects.filter().is_empty())
                {
                    // Enter gives the keys back but keeps the filter, so Esc
                    // has to reach the tree to be the way out of one.
                    return self.objects_key(key);
                } else {
                    self.shell.error = None;
                }
            }
            _ if typing => return self.scratch_key(key),
            KeyCode::Char('q') => return vec![Action::Quit],
            // Not `control`: Ctrl-C is the copy key, not a connect key.
            KeyCode::Char('c') if !control && !self.tabs.is_empty() => {
                return vec![Action::Connect(self.shell.active_tab)];
            }
            KeyCode::Char('C') if !control && !self.tabs.is_empty() => {
                return vec![Action::Disconnect(self.shell.active_tab)];
            }
            KeyCode::Char(digit @ '1'..='9') => {
                let wanted = digit as usize - '1' as usize;
                if wanted < self.tabs.len() {
                    return self.open_tab(wanted);
                }
            }
            _ if self.shell.focus == Focus::Results => return self.results_key(key),
            _ if self.shell.focus == Focus::Objects => return self.objects_key(key),
            _ => {}
        }
        Vec::new()
    }

    /// Put this tab on screen, and connect it if it is not: a tab is opened
    /// to be used. One that failed waits for `c`, so its error stays up and
    /// a bad password is not tried again on every visit.
    pub(crate) fn open_tab(&mut self, index: usize) -> Vec<Action> {
        self.shell.active_tab = index;
        match self.tabs.get(index) {
            Some(tab) if tab.state == TabState::Disconnected => vec![Action::Connect(index)],
            _ => Vec::new(),
        }
    }

    /// The keys the open overlay keeps for itself — the pane under it never
    /// sees them — and whether this was one of them.
    fn help_key(&mut self, key: KeyEvent) -> bool {
        match scroll_by(key) {
            Some(by) => self.scroll_help(by),
            None => return false,
        }
        true
    }

    /// The help closed, and back at its top for the next time it opens.
    fn close_help(&mut self) {
        self.shell.help = false;
        self.shell.help_scroll = 0;
    }

    fn scroll_help(&mut self, by: isize) {
        // Past the last row is as far as it goes, so an offset the overlay
        // clamps away does not have to be scrolled back through.
        let last = keys_for(self.shell.focus).count().saturating_sub(1);
        let scroll = &mut self.shell.help_scroll;
        *scroll = scroll.saturating_add_signed(by).min(last);
    }

    /// The scroll keys of the open inspector, which the grid under it never
    /// sees — the same bargain the help overlay strikes.
    fn inspector_key(&mut self, key: KeyEvent) -> bool {
        match scroll_by(key) {
            Some(by) if self.shell.inspector.is_some() => self.scroll_inspector(by),
            _ => return false,
        }
        true
    }

    fn scroll_inspector(&mut self, by: isize) {
        let last = self.inspect_height().saturating_sub(1);
        if let Some(inspector) = self.shell.inspector.as_mut() {
            inspector.scroll = inspector.scroll.saturating_add_signed(by).min(last);
        }
    }

    /// How many lines the cell the inspector is open on comes to. The
    /// overlay is sized by this and the scroll keys clamp against it, so
    /// both agree on how far down the value goes.
    #[must_use]
    pub fn inspect_height(&self) -> usize {
        self.tab()
            .and_then(|tab| tab.results.cell())
            .map_or(0, results::inspect_height)
    }

    /// The `count` lines of it from `top`, which is what the overlay draws
    /// and all it ever formats.
    #[must_use]
    pub fn inspect_lines(&self, top: usize, count: usize) -> Vec<String> {
        self.tab()
            .and_then(|tab| tab.results.cell())
            .map(|cell| results::inspect_lines(cell, top, count))
            .unwrap_or_default()
    }

    /// A key the open finder took. Esc closes it, Enter goes where it
    /// points, and the rest is the finder's own.
    fn finder_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let Some(mut finder) = self.shell.finder.take() else {
            return Vec::new();
        };
        match finder.key(key, &self.tabs) {
            finder::Hit::Close => Vec::new(),
            finder::Hit::Moved => {
                self.shell.finder = Some(finder);
                Vec::new()
            }
            finder::Hit::Open(found) => self.go_to(&found),
        }
    }

    /// The finder chose an object: its tab on screen, the tree open on it,
    /// and what it is made of in the results pane — the source of anything
    /// that has some, the columns of a table or a view — with the focus
    /// there, because reading it is what the finder was opened for.
    fn go_to(&mut self, found: &finder::Match) -> Vec<Action> {
        let Some(open) = self.tabs.get_mut(found.tab) else {
            return Vec::new();
        };
        let object = &found.object;
        self.shell.active_tab = found.tab;
        self.shell.focus = Focus::Objects;
        if !open.objects.reveal(object) {
            self.shell.status = format!("{}.{} is not in the tree", object.schema, object.name);
            return Vec::new();
        }
        self.shell.status.clear();
        let request = match object.kind {
            ObjectKind::Table | ObjectKind::View => CatalogRequest::Columns {
                schema: object.schema.clone(),
                table: object.name.clone(),
                show: true,
            },
            ObjectKind::Sequence => return Vec::new(),
            _ => CatalogRequest::Source {
                schema: object.schema.clone(),
                name: object.name.clone(),
                kind: object.kind,
            },
        };
        self.shell.focus = Focus::Results;
        vec![Action::LoadObjects {
            tab: found.tab,
            request,
        }]
    }

    /// A key the open prompt is being typed with. Enter is what it was
    /// opened for and Esc is the way out of it; everything else is editing.
    fn prompt_key(&mut self, key: KeyEvent) -> Vec<Action> {
        match key.code {
            KeyCode::Enter => {
                let path = self.shell.prompt.take().map(|prompt| prompt.text);
                match path.filter(|path| !path.trim().is_empty()) {
                    Some(path) => vec![Action::Export {
                        tab: self.shell.active_tab,
                        path,
                    }],
                    None => Vec::new(),
                }
            }
            KeyCode::Esc => {
                self.shell.prompt = None;
                Vec::new()
            }
            _ => {
                if let Some(prompt) = self.shell.prompt.as_mut() {
                    prompt.handle(key);
                }
                Vec::new()
            }
        }
    }

    /// A key the result grid handles, and what it asks the run loop for.
    fn results_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let tab = self.shell.active_tab;
        let Some(open) = self.tabs.get_mut(tab) else {
            return Vec::new();
        };
        match open.results.key(key) {
            Hit::Ignored | Hit::Moved => Vec::new(),
            Hit::MoreRows if open.results.truncated() => vec![Action::MoreRows { tab }],
            Hit::MoreRows => {
                self.shell.status = "every row is already here".to_owned();
                Vec::new()
            }
            Hit::Inspect => {
                if open.results.cell().is_some() {
                    self.shell.inspector = Some(Inspector::default());
                }
                Vec::new()
            }
            Hit::CopyCell | Hit::CopyRow if open.results.source().is_some() => {
                let text = open.results.source_text().unwrap_or_default();
                self.copied(text, "the source")
            }
            Hit::CopyCell => match open.results.cell().map(|cell| cell.display().into_owned()) {
                Some(text) => self.copied(text, "1 cell"),
                None => Vec::new(),
            },
            Hit::CopyRow => match open.results.row_text() {
                Some(text) => self.copied(text, "1 row"),
                None => Vec::new(),
            },
            Hit::Export => {
                self.shell.prompt = Some(Prompt::new(prompt::export_path(&open.name)));
                Vec::new()
            }
            // The rows are still coming, and a batch landing on sorted rows
            // would be out of order.
            Hit::Sort if open.results.running() => {
                self.shell.status = "sort once every row is here".to_owned();
                Vec::new()
            }
            Hit::Sort => vec![Action::Sorted {
                rows: open.results.sort(),
            }],
        }
    }

    /// Into the app's own clipboard, and out to the terminal's if it takes
    /// the escape — `copied 1 cell` either way, because the app cannot know.
    fn copied(&mut self, text: String, what: &str) -> Vec<Action> {
        self.shell.status = format!("copied {what}");
        self.shell.clipboard = text.clone();
        vec![Action::Copy(text)]
    }

    /// What a catalog query came back with: the tree's half, and — for `i`
    /// and `s` — the results pane's.
    fn catalog_event(
        &mut self,
        tab: usize,
        request: &CatalogRequest,
        result: &Result<CatalogAnswer, DbError>,
    ) {
        let Some(open) = self.tabs.get_mut(tab) else {
            return;
        };
        open.objects.answer(request, result);
        match (request, result) {
            (
                CatalogRequest::Columns {
                    schema,
                    table,
                    show: true,
                },
                Ok(CatalogAnswer::Columns(columns)),
            ) => open
                .results
                .show_columns(format!("{schema}.{table} columns"), columns),
            (CatalogRequest::Source { schema, name, .. }, Ok(CatalogAnswer::Source(text))) => {
                open.results.show_source(format!("{schema}.{name}"), text);
            }
            _ => {}
        }
        if let Err(error) = result {
            self.shell.error = Some(error.to_string());
        }
        // An index landing while the finder is open is one more tab to
        // search; the query stays.
        if matches!(request, CatalogRequest::Index)
            && let Some(finder) = self.shell.finder.as_mut()
        {
            finder.search(&self.tabs);
        }
    }

    /// A load is on its way: the row it is for says so until it lands, and
    /// [`App::busy`] says the app is working until then.
    pub fn catalog_started(&mut self, tab: usize, request: &CatalogRequest) {
        if let Some(open) = self.tabs.get_mut(tab) {
            open.objects.started(request);
        }
    }

    /// A key the object tree handles, and what it asks the run loop for.
    fn objects_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let tab = self.shell.active_tab;
        let Some(open) = self.tabs.get_mut(tab) else {
            return Vec::new();
        };
        let kind = open.kind;
        match open.objects.key(key) {
            objects::Hit::Ignored | objects::Hit::Moved => Vec::new(),
            objects::Hit::Load(request) => vec![Action::LoadObjects { tab, request }],
            objects::Hit::Select(object) => {
                // On its own line: a select dropped into the middle of the
                // line somebody was writing is two broken statements.
                let sql = select_from(kind, &object.schema, &object.name);
                let (line, column) = open.scratch.cursor();
                let alone = column == 0
                    || open
                        .scratch
                        .lines()
                        .get(line)
                        .is_some_and(|text| text.trim().is_empty());
                open.scratch.paste(&if alone {
                    format!("{sql}\n")
                } else {
                    format!("\n{sql}\n")
                });
                self.shell.focus = Focus::Scratch;
                Vec::new()
            }
            objects::Hit::Copy(name) => {
                self.shell.clipboard = name.clone();
                self.shell.status = format!("copied {name}");
                vec![Action::Copy(name)]
            }
            objects::Hit::Say(message) => {
                self.shell.status = message;
                Vec::new()
            }
        }
    }

    /// A key the pad handles, and what the run loop owes it afterwards.
    fn scratch_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let tab = self.shell.active_tab;
        let Some(open) = self.tabs.get_mut(tab) else {
            return Vec::new();
        };
        let kind = open.kind;
        match open.scratch.handle(key) {
            Outcome::Unchanged | Outcome::Edited => Vec::new(),
            Outcome::RunStatement => match open.scratch.statement_at_cursor(kind) {
                Some((sql, lines)) => {
                    open.results.expect(vec![lines]);
                    vec![Action::RunStatement { tab, sql }]
                }
                None => {
                    self.shell.status = "no statement under the cursor".to_owned();
                    Vec::new()
                }
            },
            Outcome::RunAll => {
                let (statements, lines): (Vec<String>, Vec<Range<usize>>) =
                    open.scratch.statements(kind).into_iter().unzip();
                if statements.is_empty() {
                    self.shell.status = "the pad is empty".to_owned();
                    return Vec::new();
                }
                open.results.expect(lines);
                vec![Action::RunAll { tab, statements }]
            }
            Outcome::OpenEditor => vec![Action::OpenEditor { tab }],
            Outcome::Copy(text) => {
                self.shell.status = format!("copied {} characters", text.chars().count());
                self.shell.clipboard = text.clone();
                vec![Action::Copy(text)]
            }
        }
    }
}

/// How far an overlay's scroll keys move it, or `None` for a key that is not
/// one of them.
fn scroll_by(key: KeyEvent) -> Option<isize> {
    #[allow(clippy::cast_possible_wrap)]
    let page = HELP_PAGE as isize;
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => Some(1),
        KeyCode::Char('k') | KeyCode::Up => Some(-1),
        KeyCode::PageDown => Some(page),
        KeyCode::PageUp => Some(-page),
        _ => None,
    }
}

/// The statement Enter drops in the pad: a hundred rows, spelled the way the
/// backend spells a limit.
fn select_from(kind: Kind, schema: &str, name: &str) -> String {
    match kind {
        Kind::Mssql => format!("select top 100 * from {schema}.{name}"),
        Kind::Oracle => format!("select * from {schema}.{name} fetch first 100 rows only"),
    }
}
