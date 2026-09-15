//! Pure state: tabs, focus, the scratch pad, the result grid and the object
//! tree. Nothing here touches the terminal, the clock or a database — an
//! event goes in, a state change and a list of [`Action`]s come out, which is
//! what makes the whole app testable without either.

#[cfg(test)]
pub(crate) mod tests;

use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Size;

use crate::config::{Config, Kind};

/// Every key this build handles: the key, where it works, what it does.
///
/// The help overlay, the footer hints and the key test all read this table
/// and nothing else, so a key that is not in it is a key nobody is told
/// about — and a key in it that [`App::handle`] ignores fails the test.
/// `where` is [`ANYWHERE`] or [`NOT_SCRATCH`], because those are the only two
/// answers a key that does not type text can give.
pub const KEYS: &[(&str, &str, &str)] = &[
    ("Tab", ANYWHERE, "next pane"),
    ("Shift-Tab", ANYWHERE, "previous pane"),
    ("Ctrl-T", ANYWHERE, "next tab"),
    ("1-9", NOT_SCRATCH, "select tab"),
    ("c", NOT_SCRATCH, "connect"),
    ("C", NOT_SCRATCH, "disconnect"),
    ("?", ANYWHERE, "help"),
    ("Esc", ANYWHERE, "close help or error"),
    ("q", NOT_SCRATCH, "quit"),
    ("Ctrl-Q", ANYWHERE, "quit"),
];

pub const ANYWHERE: &str = "anywhere";

pub const NOT_SCRATCH: &str = "not Scratch";

/// The frames a connecting tab's mark cycles through, one every
/// [`SPIN_EVERY`].
pub const SPINNER: [&str; 4] = ["⠋", "⠙", "⠹", "⠸"];

/// How often the spinner moves on. Fast enough to read as motion, slow
/// enough that a connecting app costs ten frames a second and not more.
pub const SPIN_EVERY: Duration = Duration::from_millis(100);

/// The rows of [`KEYS`] that work while `focus` has the focus.
pub fn keys_for(
    focus: Focus,
) -> impl Iterator<Item = &'static (&'static str, &'static str, &'static str)> {
    KEYS.iter()
        .filter(move |(_, place, _)| *place == ANYWHERE || focus != Focus::Scratch)
}

/// What the app asks the run loop to do. The app itself never does IO, so
/// everything with a side effect leaves through here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Quit,
    /// Open (or re-open) this tab's connection.
    Connect(usize),
    /// Close it, cancelling whatever it is running.
    Disconnect(usize),
}

/// What the run loop reports back about a connection. The app never opens
/// one, so this is the only way a tab moves off [`TabState::Connecting`] —
/// and it is what makes those transitions testable without a database.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeEvent {
    Connecting { tab: usize },
    Connected { tab: usize, connect_ms: u32 },
    Failed { tab: usize, message: String },
    Disconnected { tab: usize },
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tab {
    pub name: String,
    pub kind: Kind,
    pub state: TabState,
    /// How long the connection that is up took to open.
    pub connect_ms: Option<u32>,
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
    /// Which frame of [`SPINNER`] a connecting tab is showing.
    pub spinner: usize,
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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
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
                })
                .collect(),
        }
    }

    /// The tab whose panes are on screen, if there is one.
    #[must_use]
    pub fn tab(&self) -> Option<&Tab> {
        self.tabs.get(self.shell.active_tab)
    }

    /// Whether anything is still in flight: a tab connecting, and from E5 a
    /// query running too. This is what a replay's `wait busy` waits out, so
    /// it answers for the whole app and not just the tab on screen.
    #[must_use]
    pub fn busy(&self) -> bool {
        self.connecting()
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

    /// One event, turned into state changes and whatever has to happen
    /// outside the app.
    pub fn handle(&mut self, event: Event) -> Vec<Action> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.key(key),
            Event::Resize(columns, rows) => {
                self.shell.size = Size::new(columns, rows);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn key(&mut self, key: KeyEvent) -> Vec<Action> {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        // ponytail: the scratch pad has no editor yet (T4.1), so all it does
        // here is swallow the keys that would type into it.
        let typing = self.shell.focus == Focus::Scratch;
        match key.code {
            KeyCode::Char('q' | 'Q') if control => return vec![Action::Quit],
            KeyCode::Char('t' | 'T') if control => {
                if !self.tabs.is_empty() {
                    self.shell.active_tab = (self.shell.active_tab + 1) % self.tabs.len();
                }
            }
            KeyCode::Tab => self.shell.focus = self.shell.focus.next(),
            KeyCode::BackTab => self.shell.focus = self.shell.focus.previous(),
            KeyCode::Char('?') => self.shell.help = !self.shell.help,
            KeyCode::Esc => {
                if self.shell.help {
                    self.shell.help = false;
                } else {
                    self.shell.error = None;
                }
            }
            KeyCode::Char('q') if !typing => return vec![Action::Quit],
            // Not `control`: Ctrl-C is the terminal's, not a connect key.
            KeyCode::Char('c') if !typing && !control && !self.tabs.is_empty() => {
                return vec![Action::Connect(self.shell.active_tab)];
            }
            KeyCode::Char('C') if !typing && !control && !self.tabs.is_empty() => {
                return vec![Action::Disconnect(self.shell.active_tab)];
            }
            KeyCode::Char(digit @ '1'..='9') if !typing => {
                let wanted = digit as usize - '1' as usize;
                if wanted < self.tabs.len() {
                    self.shell.active_tab = wanted;
                }
            }
            _ => {}
        }
        Vec::new()
    }
}
