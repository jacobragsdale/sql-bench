//! Pure state: tabs, focus, the scratch pad, the result grid and the object
//! tree. Nothing here touches the terminal, the clock or a database — an
//! event goes in, a state change and a list of [`Action`]s come out, which is
//! what makes the whole app testable without either.

pub mod results;
pub mod scratch;

#[cfg(test)]
pub(crate) mod tests;

use std::ops::Range;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Size;

use crate::config::{Config, Kind};
use crate::db::model::QueryEvent;
use results::{Hit, Results};
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
    ("Ctrl-A", SCRATCH, "line start"),
    ("Ctrl-U", SCRATCH, "delete to line start"),
    ("Ctrl-K", SCRATCH, "delete to line end"),
    ("Ctrl-W", SCRATCH, "delete the word before"),
    ("Ctrl-Left", SCRATCH, "word left"),
    ("Ctrl-Right", SCRATCH, "word right"),
    ("?", ANYWHERE, "help"),
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
];

pub const ANYWHERE: &str = "anywhere";

pub const NOT_SCRATCH: &str = "not Scratch";

pub const SCRATCH: &str = "Scratch";

pub const RESULTS: &str = "Results";

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
            NOT_SCRATCH => !typing,
            _ => true,
        }
    })
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
    /// The first row of [`keys_for`] the open overlay shows. `?` and Esc put
    /// it back to the top.
    pub help_scroll: usize,
    /// Which frame of [`SPINNER`] a connecting tab is showing.
    pub spinner: usize,
    /// What Ctrl-C last copied. The terminal's own clipboard is a best
    /// effort the run loop makes; this one is always there.
    pub clipboard: String,
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
        self.connecting() || self.running()
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
        if self.shell.help && self.help_key(key) {
            return Vec::new();
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        // The scratch pad types every key the shell does not keep for
        // itself, which is why the shell's keys are matched first.
        let typing = self.shell.focus == Focus::Scratch;
        match key.code {
            KeyCode::Char('q' | 'Q') if control => return vec![Action::Quit],
            KeyCode::Char('t' | 'T') if control => {
                if !self.tabs.is_empty() {
                    self.shell.active_tab = (self.shell.active_tab + 1) % self.tabs.len();
                }
            }
            KeyCode::Tab if !typing => self.shell.focus = self.shell.focus.next(),
            KeyCode::BackTab => self.shell.focus = self.shell.focus.previous(),
            KeyCode::Char('?') => {
                self.shell.help = !self.shell.help;
                self.shell.help_scroll = 0;
            }
            KeyCode::Esc => {
                if self.shell.help {
                    self.shell.help = false;
                    self.shell.help_scroll = 0;
                } else if self.tab().is_some_and(|tab| tab.results.running()) {
                    return vec![Action::Cancel(self.shell.active_tab)];
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
                    self.shell.active_tab = wanted;
                }
            }
            _ if self.shell.focus == Focus::Results => return self.results_key(key),
            _ => {}
        }
        Vec::new()
    }

    /// The keys the open overlay keeps for itself — the pane under it never
    /// sees them — and whether this was one of them.
    fn help_key(&mut self, key: KeyEvent) -> bool {
        // Past the last row is as far as it goes, so an offset the overlay
        // clamps away does not have to be scrolled back through.
        let last = keys_for(self.shell.focus).count().saturating_sub(1);
        let scroll = &mut self.shell.help_scroll;
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => *scroll += 1,
            KeyCode::Char('k') | KeyCode::Up => *scroll = scroll.saturating_sub(1),
            KeyCode::PageDown => *scroll += HELP_PAGE,
            KeyCode::PageUp => *scroll = scroll.saturating_sub(HELP_PAGE),
            _ => return false,
        }
        *scroll = (*scroll).min(last);
        true
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
                self.shell.status = "inspector arrives in T5.3".to_owned();
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
