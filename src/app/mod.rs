//! Pure state: tabs, focus, the scratch pad, the result grid and the object
//! tree. Nothing here touches the terminal, the clock or a database — an
//! event goes in, a state change and a list of [`Action`]s come out, which is
//! what makes the whole app testable without either.

#[cfg(test)]
pub(crate) mod tests;

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
    ("?", ANYWHERE, "help"),
    ("Esc", ANYWHERE, "close help or error"),
    ("q", NOT_SCRATCH, "quit"),
    ("Ctrl-Q", ANYWHERE, "quit"),
];

pub const ANYWHERE: &str = "anywhere";

pub const NOT_SCRATCH: &str = "not Scratch";

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

/// Where one tab's connection is. Nothing moves it off `Disconnected` yet:
/// connecting is E4's.
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
            Self::Connecting => "◌",
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
            Self::Failed(_) => "error",
        }
    }
}

/// One tab: a connection from `config.toml` and where it is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tab {
    pub name: String,
    pub kind: Kind,
    pub state: TabState,
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
                })
                .collect(),
        }
    }

    /// The tab whose panes are on screen, if there is one.
    #[must_use]
    pub fn tab(&self) -> Option<&Tab> {
        self.tabs.get(self.shell.active_tab)
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
