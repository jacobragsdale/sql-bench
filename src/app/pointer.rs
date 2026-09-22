//! The mouse: what each frame says can be clicked, and the gestures that turn
//! presses, releases, motion and the wheel into the same state changes the
//! keys make.
//!
//! The renderer returns a [`Hits`] and the run loop hands the last one back
//! with every mouse event, so the app still never sees a terminal — and a
//! click resolves against exactly what was painted, overlays included.

use std::ops::Range;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Constraint, Layout, Position, Rect};

use super::{Action, App, Focus, key_named, keys_for};

/// How close together two clicks on one spot have to be to count as a
/// double-click: the common desktop default.
pub const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// How far one notch of the wheel scrolls.
const WHEEL: isize = 3;

/// What a right-click offers in each pane: rows of [`KEYS`](super::KEYS),
/// named the way it names them, so a menu can do nothing a key cannot.
pub const MENU: [(Focus, &[&str]); 3] = [
    (Focus::Objects, &["Enter", "s", "i", "y", "r", "/", "Space"]),
    (
        Focus::Results,
        &["Enter", "y", "Y", "o", "e", "m", "[", "]"],
    ),
    (
        Focus::Scratch,
        &["Ctrl-R", "F5", "Ctrl-C", "Ctrl-Z", "Ctrl-E"],
    ),
];

/// `pane`'s menu, each entry its key and what the key does there. Read
/// through [`keys_for`], because [`KEYS`](super::KEYS) has an Enter and a
/// `y` for more than one pane.
#[must_use]
pub fn menu(pane: Focus) -> Vec<(&'static str, &'static str)> {
    MENU.iter()
        .filter(|(of, _)| *of == pane)
        .flat_map(|(_, names)| names.iter())
        .filter_map(|name| {
            keys_for(pane)
                .find(|(key, ..)| key == name)
                .map(|(key, _, does)| (*key, *does))
        })
        .collect()
}

/// What was painted in a region of the frame. Indexes and never references,
/// so a frame's targets outlive the borrow of the app it was drawn from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Target {
    /// The tab bar's label for this tab.
    Tab(usize),
    /// Anywhere in a pane nothing more particular was drawn over.
    Pane(Focus),
    /// A chip, a placeholder, a footer hint or a help row: clicking it
    /// focuses `pane` and presses `key`, so a button can do nothing a key
    /// cannot.
    Button { pane: Focus, key: KeyEvent },
    /// The rows of the object tree, the first of them `top` in the rows
    /// showing.
    Tree { top: usize },
    /// One column of the grid's rows, drawn from row `top` with column
    /// `left` at the pane's left edge. A click sets the window from these
    /// rather than from the stored hint, so the view stays where it was.
    Cells {
        column: usize,
        top: usize,
        left: usize,
    },
    /// A column's header, drawn with column `left` at the pane's left edge.
    /// A click selects the column and presses `o`.
    Header { column: usize, left: usize },
    /// An object's source, drawn from line `top`.
    Source { top: usize },
    /// The scratch pad's text, gutter included, drawn from line `top` and
    /// character `left`; the text starts `gutter` cells in.
    Pad {
        top: usize,
        left: usize,
        gutter: usize,
    },
    /// The thumb of `pane`'s scrollbar, over `content` rows of which
    /// `viewport` show, on a `track` of the pane's right border. Dragging it
    /// scrolls; the track either side of it is a PageUp or PageDown button.
    Thumb {
        pane: Focus,
        content: usize,
        viewport: usize,
        track: Rect,
    },
    /// A border between panes, dividing `area`: dragging it moves the
    /// split, and a double-click puts it back.
    Seam { seam: Seam, area: Rect },
    /// Entry `n` of the open context menu.
    MenuItem(usize),
    /// The text of the export prompt, starting at the region's left edge.
    PromptText,
    /// The body of the open help, inspector or menu, whichever is on top.
    Overlay,
    /// The whole frame, pushed under an overlay so that a click beside it
    /// closes it rather than reaching the pane underneath.
    Outside,
}

impl Target {
    /// Whether the pointer resting on it lights it up. A pane or an overlay
    /// body is too big to be a thing the eye should be drawn to.
    #[must_use]
    pub const fn hovers(self) -> bool {
        matches!(
            self,
            Self::Tab(_)
                | Self::Button { .. }
                | Self::Thumb { .. }
                | Self::Seam { .. }
                | Self::MenuItem(_)
        )
    }
}

/// The two borders a drag can move, named for the share of [`Split`] each
/// one sets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Seam {
    /// The left border of Scratch and Results, over the height of the body.
    /// Objects' own right border is its scrollbar's.
    Objects,
    /// Scratch's bottom border, across the right-hand column.
    Scratch,
}

// ponytail: for the session only; save it in the state dir if anyone asks.
/// How much of the screen Objects has, and of the right-hand column Scratch
/// has, in percent, so a resize keeps the proportions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Split {
    pub objects: u16,
    pub scratch: u16,
}

impl Default for Split {
    fn default() -> Self {
        Self {
            objects: 30,
            scratch: 40,
        }
    }
}

/// The narrowest Objects and the right-hand column get, and the shortest
/// Scratch and Results: a title and a row.
const NARROWEST: u16 = 20;
const SHORTEST: u16 = 3;

impl Split {
    /// Objects, Scratch and Results in `body`. The least each gets holds at
    /// every size rather than only when the seam was dragged, so a split
    /// made on a big screen still leaves every pane a pane after it shrinks.
    #[must_use]
    pub fn areas(self, body: Rect) -> [Rect; 3] {
        let [objects, right] = Layout::horizontal([
            Constraint::Length(share(self.objects, body.width, NARROWEST)),
            Constraint::Fill(1),
        ])
        .areas(body);
        let [scratch, results] = Layout::vertical([
            Constraint::Length(share(self.scratch, right.height, SHORTEST)),
            Constraint::Fill(1),
        ])
        .areas(right);
        [objects, scratch, results]
    }
}

/// `percent` of `total` cells, rounded, leaving `least` either side.
fn share(percent: u16, total: u16, least: u16) -> u16 {
    let cells = (u32::from(total) * u32::from(percent) + 50) / 100;
    u16::try_from(cells)
        .unwrap_or(total)
        .min(total.saturating_sub(least))
        .max(least)
}

/// [`share`] turned round: the percent that draws `cells` of `total`, once
/// they leave `least` either side. Clamped first, so dragging on past the
/// limit is the same percent and draws nothing new.
// ponytail: whole percents, so past 100 cells a seam moves two at a time;
// per-mille if anyone minds.
fn percent(cells: u16, total: u16, least: u16) -> u16 {
    let cells = u32::from(cells.min(total.saturating_sub(least)).max(least));
    u16::try_from((cells * 100 + u32::from(total) / 2) / u32::from(total.max(1))).unwrap_or(100)
}

/// The cells of a `track` long scrollbar the thumb covers, `offset` rows
/// into `content` rows of which `viewport` show. The painter and the drag
/// both use it, so what is drawn is what is hit.
///
/// The thumb is as long as the share of the content that shows, at least a
/// cell. It leaves the top only past offset 0 and reaches the bottom at the
/// last offset, so a track showing above or below it always has a page to go.
#[must_use]
pub fn thumb(offset: usize, content: usize, viewport: usize, track: u16) -> Range<u16> {
    let length = (usize::from(track) * viewport)
        .checked_div(content)
        .unwrap_or(0)
        .max(1)
        .min(usize::from(track));
    let travel = usize::from(track).saturating_sub(length);
    let last = content.saturating_sub(viewport);
    let start = (offset.min(last) * travel + last / 2)
        .checked_div(last)
        .unwrap_or(0);
    let start = u16::try_from(start).unwrap_or(0);
    start..start.saturating_add(u16::try_from(length).unwrap_or(track))
}

/// The offset whose thumb starts `start` cells down the track: [`thumb`]
/// turned round, clamped to the ends.
#[must_use]
pub fn offset(start: u16, content: usize, viewport: usize, track: u16) -> usize {
    let travel = usize::from(track).saturating_sub(thumb(0, content, viewport, track).len());
    let last = content.saturating_sub(viewport);
    (usize::from(start).min(travel) * last + travel / 2)
        .checked_div(travel)
        .unwrap_or(0)
}

/// Where each clickable thing on one frame was drawn, in paint order.
///
/// There are no layers: what was painted last is on top, so [`Hits::at`]
/// looks from the end.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Hits(Vec<(Rect, Target)>);

impl Hits {
    pub fn push(&mut self, rect: Rect, target: Target) {
        if !rect.is_empty() {
            self.0.push((rect, target));
        }
    }

    /// Every region, in the order it was painted.
    pub fn regions(&self) -> impl Iterator<Item = (Rect, Target)> + '_ {
        self.0.iter().copied()
    }

    /// The topmost region under `position`, and the target painted there.
    #[must_use]
    pub fn at(&self, position: Position) -> Option<(Rect, Target)> {
        self.0
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(position))
            .copied()
    }

    /// The target under `position` and the row of it the pointer is on:
    /// what a click there would act on. Two positions with the same spot
    /// look the same on screen, which is how a flood of motion costs no
    /// frames.
    #[must_use]
    pub fn spot(&self, position: Position) -> Option<Spot> {
        self.at(position).map(|(rect, target)| Spot {
            target,
            row: position.y - rect.y,
        })
    }
}

/// A target and a row of it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Spot {
    pub target: Target,
    pub row: u16,
}

/// All the mouse state there is, in one field so a test can reset it in one
/// line.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Mouse {
    /// Where the pointer was last seen. The renderer lights up what is under
    /// it; nothing else is stored about hovering.
    pub pointer: Option<Position>,
    press: Option<Press>,
    last: Option<Click>,
    /// The context menu a right-click opened, while it is open.
    pub menu: Option<Menu>,
}

/// An open context menu: whose actions it offers, the cell it was opened
/// from, and the entry Enter would pick.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Menu {
    pub pane: Focus,
    pub at: Position,
    pub item: usize,
}

/// A button that went down and has not come up yet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Press {
    spot: Spot,
    button: MouseButton,
    /// The pointer has been somewhere else since, so the release is not a
    /// click: pressing on the wrong thing and sliding off is how a desktop
    /// takes a press back.
    left: bool,
}

/// The last click, for telling whether the next one is a double.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Click {
    spot: Spot,
    at: Instant,
}

impl Mouse {
    /// The seam the left button is holding, which stays lit however far the
    /// pointer outruns it.
    #[must_use]
    pub fn seam(&self) -> Option<Seam> {
        match self.press? {
            Press {
                spot:
                    Spot {
                        target: Target::Seam { seam, .. },
                        ..
                    },
                button: MouseButton::Left,
                ..
            } => Some(seam),
            _ => None,
        }
    }

    /// Note a click on `spot` at `now`, and say whether it was the second of
    /// a double-click. A double uses up both clicks, so three in a row are a
    /// double and a single, never two doubles.
    fn clicked(&mut self, spot: Spot, now: Instant) -> bool {
        let double = self.last.is_some_and(|last| {
            last.spot == spot && now.saturating_duration_since(last.at) < DOUBLE_CLICK
        });
        self.last = (!double).then_some(Click { spot, at: now });
        double
    }
}

impl App {
    /// One mouse event against the frame it was pointed at.
    ///
    /// `now` comes from the caller, the way [`App::settle`]'s does, so the
    /// app still reads no clock and a double-click can be tested with two
    /// made-up instants.
    pub fn pointer(&mut self, mouse: MouseEvent, now: Instant, hits: &Hits) -> Vec<Action> {
        let position = Position::new(mouse.column, mouse.row);
        self.shell.mouse.pointer = Some(position);
        let spot = hits.spot(position);
        match mouse.kind {
            MouseEventKind::Down(button @ (MouseButton::Left | MouseButton::Right)) => {
                self.shell.mouse.press = spot.map(|spot| Press {
                    spot,
                    button,
                    left: false,
                });
                // The cursor goes down with the button, so a drag has an
                // anchor to select from.
                if let Some(region @ (_, Target::Pad { .. })) = hits.at(position) {
                    self.pad_point(
                        region,
                        position,
                        mouse.modifiers.contains(KeyModifiers::SHIFT),
                        button == MouseButton::Right,
                    );
                }
                Vec::new()
            }
            MouseEventKind::Drag(MouseButton::Left)
                if self.shell.mouse.press.is_some_and(|press| {
                    press.button == MouseButton::Left
                        && matches!(press.spot.target, Target::Pad { .. })
                }) =>
            {
                self.pad_drag(position, hits)
            }
            MouseEventKind::Drag(MouseButton::Left)
                if self.shell.mouse.press.is_some_and(|press| {
                    press.button == MouseButton::Left
                        && matches!(press.spot.target, Target::Thumb { .. })
                }) =>
            {
                self.thumb_drag(position, hits)
            }
            MouseEventKind::Drag(MouseButton::Left) if self.shell.mouse.seam().is_some() => {
                self.seam_drag(position)
            }
            // A drag off anything but the pad's text, a thumb or a seam does
            // nothing.
            MouseEventKind::Drag(_) | MouseEventKind::Moved => {
                if let Some(press) = self.shell.mouse.press.as_mut() {
                    press.left |= spot != Some(press.spot);
                }
                Vec::new()
            }
            MouseEventKind::Up(button) => match self.shell.mouse.press.take() {
                Some(press)
                    if press.button == button && !press.left && spot == Some(press.spot) =>
                {
                    // The column into the region, which only the prompt's
                    // text reads: a spot is a target and a row, no more.
                    let column = hits.at(position).map_or(0, |(rect, _)| position.x - rect.x);
                    if button == MouseButton::Right {
                        return self.right_click(press.spot, column, position);
                    }
                    let double = self.shell.mouse.clicked(press.spot, now);
                    self.click(press.spot, column, double)
                }
                _ => Vec::new(),
            },
            MouseEventKind::ScrollDown
            | MouseEventKind::ScrollUp
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight => self.wheel(mouse, hits.at(position)),
            _ => Vec::new(),
        }
    }

    /// A click, acted on the target that was pressed: `column` cells into
    /// its region, on the row of it the spot says.
    fn click(&mut self, spot: Spot, column: u16, double: bool) -> Vec<Action> {
        let target = spot.target;
        if let Target::Pane(Focus::Objects)
        | Target::Tree { .. }
        | Target::Button {
            pane: Focus::Objects,
            ..
        } = target
        {
            self.end_filter_typing();
        }
        let row = usize::from(spot.row);
        match target {
            Target::Tree { top } => return self.click_tree(top, row, column, double),
            Target::Cells { column, top, left } => {
                self.shell.focus = Focus::Results;
                let Some(tab) = self.tabs.get_mut(self.shell.active_tab) else {
                    return Vec::new();
                };
                if tab.results.click(top + row, column, (top, left)) && double {
                    return self.results_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                }
            }
            Target::Header { column, left } => {
                self.shell.focus = Focus::Results;
                if let Some(tab) = self.tabs.get_mut(self.shell.active_tab) {
                    tab.results.click_header(column, left);
                }
                return self.results_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));
            }
            Target::Source { .. } => self.shell.focus = Focus::Results,
            Target::Tab(index) if index < self.tabs.len() => return self.open_tab(index),
            Target::Pane(focus) => self.shell.focus = focus,
            Target::Button { pane, key } => {
                // The help keeps j, k, the arrows and the Page keys for
                // itself, so a row of it is pressed with the help gone. Esc
                // and `?` are the help's own way out, and closing it first
                // would make them act on what is under it instead.
                if self.shell.help && !matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) {
                    self.close_help();
                }
                self.shell.focus = pane;
                return self.key(key);
            }
            // The cursor went where the button went down, so all a click
            // adds is the focus, and a double-click the word.
            Target::Pad { .. } => {
                self.shell.focus = Focus::Scratch;
                if double && let Some(tab) = self.tabs.get_mut(self.shell.active_tab) {
                    tab.scratch.select_word();
                }
            }
            Target::PromptText => {
                if let Some(prompt) = self.shell.prompt.as_mut() {
                    prompt.place(usize::from(column));
                }
            }
            Target::MenuItem(item) => return self.pick(item),
            // The way Esc goes: the menu, the prompt, then the help, then
            // the inspector, so a click beside one closes only the one on top.
            Target::Outside => {
                if self.shell.mouse.menu.is_some() {
                    self.shell.mouse.menu = None;
                } else if self.shell.prompt.is_some() {
                    self.shell.prompt = None;
                } else if self.shell.help {
                    self.close_help();
                } else {
                    self.shell.inspector = None;
                }
            }
            // A seam belongs to neither pane either side of it, so a click
            // focuses neither; a double-click is the way back to the start.
            Target::Seam { .. } if double => self.shell.split = Split::default(),
            // A thumb or a seam is for dragging, and pressed and let go is
            // nothing.
            Target::Tab(_) | Target::Overlay | Target::Thumb { .. } | Target::Seam { .. } => {}
        }
        Vec::new()
    }

    /// A right-click on a row, a cell, a header or the pad selects what is
    /// under it — only selects: the glyph, the sort and the double-click are
    /// all entries of the menu — then opens that pane's menu at the pointer.
    /// Anywhere else in a pane opens the menu and moves nothing, and on
    /// anything that is not a pane it is a left click.
    fn right_click(&mut self, spot: Spot, column: u16, at: Position) -> Vec<Action> {
        if let Target::Pane(Focus::Objects) | Target::Tree { .. } = spot.target {
            self.end_filter_typing();
        }
        let row = usize::from(spot.row);
        let tab = self.tabs.get_mut(self.shell.active_tab);
        let pane = match (spot.target, tab) {
            (Target::Tree { top }, Some(tab)) => {
                tab.objects.click(top, row);
                Focus::Objects
            }
            (Target::Cells { column, top, left }, Some(tab)) => {
                tab.results.click(top + row, column, (top, left));
                Focus::Results
            }
            (Target::Header { column, left }, Some(tab)) => {
                tab.results.click_header(column, left);
                Focus::Results
            }
            // The cursor went where the button went down, or stayed in the
            // selection it went down in.
            (Target::Pad { .. }, _) => Focus::Scratch,
            (Target::Source { .. }, _) => Focus::Results,
            (Target::Pane(pane), _) => pane,
            _ => return self.click(spot, column, false),
        };
        self.shell.focus = pane;
        self.shell.mouse.menu = Some(Menu { pane, at, item: 0 });
        Vec::new()
    }

    /// Entry `item` of the open menu: the menu closed, its pane focused, and
    /// its key pressed, so picking it is exactly the key.
    fn pick(&mut self, item: usize) -> Vec<Action> {
        let Some(open) = self.shell.mouse.menu.take() else {
            return Vec::new();
        };
        let Some(key) = menu(open.pane)
            .get(item)
            .and_then(|(name, _)| key_named(name))
        else {
            return Vec::new();
        };
        self.shell.focus = open.pane;
        self.key(key)
    }

    /// A key while the menu is open, which takes every one: the arrows and
    /// j and k move the highlight, Enter picks it, and any other key closes
    /// the menu and goes no further — Esc as it should, and a letter because
    /// it was aimed at a menu that was in the way.
    pub(super) fn menu_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let Some(open) = self.shell.mouse.menu.as_mut() else {
            return Vec::new();
        };
        let last = menu(open.pane).len().saturating_sub(1);
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => open.item = (open.item + 1).min(last),
            KeyCode::Up | KeyCode::Char('k') => open.item = open.item.saturating_sub(1),
            KeyCode::Enter => {
                let item = open.item;
                return self.pick(item);
            }
            _ => self.shell.mouse.menu = None,
        }
        Vec::new()
    }

    /// A row of the tree: the cursor goes there, its `▸` or `▾` opens or
    /// closes it the way Space does, and a second click is Enter. The keys
    /// are pressed rather than their work copied, so what a load, a select
    /// or a source asks the run loop for is decided in one place.
    fn click_tree(&mut self, top: usize, row: usize, column: u16, double: bool) -> Vec<Action> {
        self.shell.focus = Focus::Objects;
        let Some(tab) = self.tabs.get_mut(self.shell.active_tab) else {
            return Vec::new();
        };
        if !tab.objects.click(top, row) {
            return Vec::new();
        }
        let code = if tab.objects.on_glyph(usize::from(column)) {
            KeyCode::Char(' ')
        } else if double {
            KeyCode::Enter
        } else {
            return Vec::new();
        };
        self.objects_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    /// A click in Objects is done typing the filter, the way Enter is:
    /// otherwise Reload, or whatever the click would press, is one more
    /// letter of it.
    fn end_filter_typing(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.shell.active_tab)
            && tab.objects.filtering()
        {
            tab.objects
                .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        }
    }

    /// The pad drawn in `region`: the cursor to the line and character at
    /// `position`, from the window the frame drew so the view stays put.
    /// Above or below the pad is the line just past its edge, so a drag out
    /// of it scrolls a line each time it moves; left of the text is column 0.
    /// With `keep`, a point inside the selection leaves it be, so a
    /// right-click on it can still copy it.
    fn pad_point(
        &mut self,
        (rect, target): (Rect, Target),
        position: Position,
        extend: bool,
        keep: bool,
    ) {
        let (Target::Pad { top, left, gutter }, Some(tab)) =
            (target, self.tabs.get_mut(self.shell.active_tab))
        else {
            return;
        };
        let line = if position.y < rect.y {
            top.saturating_sub(1)
        } else if position.y >= rect.bottom() {
            top + usize::from(rect.height)
        } else {
            top + usize::from(position.y - rect.y)
        };
        let x = usize::from(position.x.saturating_sub(rect.x));
        let column = if x < gutter { 0 } else { left + x - gutter };
        tab.scratch.show_from(top, left);
        if keep
            && tab
                .scratch
                .selection()
                .is_some_and(|(from, to)| (from..to).contains(&(line, column)))
        {
            return;
        }
        tab.scratch.place((line, column), extend);
    }

    /// A press on the pad dragged: the selection from where it went down to
    /// wherever the pointer is now, measured against the pad as the last
    /// frame drew it, which may have scrolled since the press.
    fn pad_drag(&mut self, position: Position, hits: &Hits) -> Vec<Action> {
        if let Some(press) = self.shell.mouse.press.as_mut() {
            // A drag is never a click, even one that ends where it began.
            press.left = true;
        }
        self.shell.focus = Focus::Scratch;
        if let Some(region) = hits
            .regions()
            .find(|(_, target)| matches!(target, Target::Pad { .. }))
        {
            self.pad_point(region, position, true, false);
        }
        Vec::new()
    }

    /// A thumb dragged: the pane scrolled so the thumb is under the pointer
    /// where it was grabbed, measured on the track as the press found it,
    /// and the cursor pulled along to stay in view the way the wheel pulls
    /// it. The focus stays where it was, as it does for the wheel.
    fn thumb_drag(&mut self, position: Position, hits: &Hits) -> Vec<Action> {
        let Some(press) = self.shell.mouse.press.as_mut() else {
            return Vec::new();
        };
        press.left = true;
        let Target::Thumb {
            pane,
            content,
            viewport,
            track,
        } = press.spot.target
        else {
            return Vec::new();
        };
        let start = position
            .y
            .saturating_sub(press.spot.row)
            .saturating_sub(track.y);
        let top = offset(start, content, viewport, track.height);
        // The pad scrolls sideways too, and that stays as it was drawn.
        let left = hits.regions().find_map(|(_, target)| match target {
            Target::Pad { left, .. } => Some(left),
            _ => None,
        });
        let Some(tab) = self.tabs.get_mut(self.shell.active_tab) else {
            return Vec::new();
        };
        match pane {
            Focus::Objects => tab.objects.wheel(top, 0, viewport),
            Focus::Scratch => tab.scratch.wheel((top, left.unwrap_or(0)), viewport, 0),
            Focus::Results if tab.results.source().is_some() => {
                tab.results.wheel_source(top, 0, viewport);
            }
            Focus::Results => tab.results.wheel(top, 0, viewport),
        }
        Vec::new()
    }

    /// A seam dragged: the border to the pointer, measured in the area the
    /// press found it dividing, as near as a whole percent draws it.
    fn seam_drag(&mut self, position: Position) -> Vec<Action> {
        let Some(press) = self.shell.mouse.press.as_mut() else {
            return Vec::new();
        };
        press.left = true;
        let Target::Seam { seam, area } = press.spot.target else {
            return Vec::new();
        };
        let split = &mut self.shell.split;
        match seam {
            // The seam is the first column right of Objects.
            Seam::Objects => {
                split.objects = percent(position.x.saturating_sub(area.x), area.width, NARROWEST);
            }
            // The seam is Scratch's last row.
            Seam::Scratch => {
                split.scratch = percent(
                    (position.y + 1).saturating_sub(area.y),
                    area.height,
                    SHORTEST,
                );
            }
        }
        Vec::new()
    }

    /// What the last frame showed of the pad is where it goes on showing
    /// from, so a key that moves the cursor around inside the view leaves
    /// the view where it is. The run loop calls this after every draw,
    /// because only the renderer knows how tall the pad is.
    pub fn drawn(&mut self, hits: &Hits) {
        if let Some((_, Target::Pad { top, left, .. })) = hits
            .regions()
            .find(|(_, target)| matches!(target, Target::Pad { .. }))
            && let Some(tab) = self.tabs.get_mut(self.shell.active_tab)
        {
            tab.scratch.show_from(top, left);
        }
    }

    /// The wheel scrolls what is under the pointer and never moves the focus.
    /// A sideways wheel, or Shift with the wheel, scrolls the grid's columns
    /// a column a notch.
    fn wheel(&mut self, mouse: MouseEvent, under: Option<(Rect, Target)>) -> Vec<Action> {
        let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);
        let (by, sideways) = match mouse.kind {
            MouseEventKind::ScrollDown => (1, shift),
            MouseEventKind::ScrollUp => (-1, shift),
            MouseEventKind::ScrollRight => (1, true),
            _ => (-1, true),
        };
        let Some((rect, target)) = under else {
            return Vec::new();
        };
        let height = usize::from(rect.height);
        let help = self.shell.help;
        let inspecting = self.shell.inspector.is_some();
        let Some(tab) = self.tabs.get_mut(self.shell.active_tab) else {
            return Vec::new();
        };
        match (target, sideways) {
            (Target::Tree { top }, false) => tab.objects.wheel(top, by * WHEEL, height),
            (Target::Cells { top, .. }, false) => tab.results.wheel(top, by * WHEEL, height),
            (Target::Cells { left, .. } | Target::Header { left, .. }, true) => {
                tab.results.wheel_columns(left, by);
            }
            (Target::Source { top }, false) => tab.results.wheel_source(top, by * WHEEL, height),
            (Target::Pad { top, left, .. }, false) => {
                tab.scratch.wheel((top, left), height, by * WHEEL);
            }
            // An open overlay's Outside is under everything of its own and
            // over everything else, so any other target is the overlay: its
            // body, a row of it or its close button. The help goes over the
            // inspector, so while it is open it is the one under the pointer.
            (Target::Outside, _) | (_, true) => {}
            _ if help => self.scroll_help(by * WHEEL),
            _ if inspecting => self.scroll_inspector(by * WHEEL),
            _ => {}
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spot(target: Target, row: u16) -> Spot {
        Spot { target, row }
    }

    #[test]
    fn the_region_painted_last_is_the_one_under_the_pointer() {
        let mut hits = Hits::default();
        hits.push(Rect::new(0, 0, 10, 10), Target::Pane(Focus::Objects));
        hits.push(Rect::new(0, 0, 20, 20), Target::Outside);
        hits.push(Rect::new(5, 5, 4, 2), Target::Overlay);
        hits.push(Rect::new(0, 0, 0, 5), Target::Tab(0));
        assert_eq!(
            hits.at(Position::new(6, 6)),
            Some((Rect::new(5, 5, 4, 2), Target::Overlay))
        );
        assert_eq!(
            hits.spot(Position::new(1, 1)),
            Some(spot(Target::Outside, 1))
        );
        assert_eq!(hits.spot(Position::new(20, 0)), None, "past the right edge");
        assert_eq!(hits.0.len(), 3, "an empty region is never pushed");
    }

    #[test]
    fn every_menu_entry_is_a_key_of_its_pane() {
        for pane in [Focus::Objects, Focus::Results, Focus::Scratch] {
            let (_, names) = MENU
                .iter()
                .find(|(of, _)| *of == pane)
                .unwrap_or_else(|| panic!("no menu for {pane:?}"));
            for name in *names {
                assert!(
                    keys_for(pane).any(|(key, ..)| key == name) && key_named(name).is_some(),
                    "{name} is not a key of {pane:?}"
                );
            }
            assert_eq!(menu(pane).len(), names.len());
        }
        assert_eq!(
            menu(Focus::Objects)[0],
            ("Enter", "select from it"),
            "not Results' Enter"
        );
    }

    #[test]
    fn a_second_click_on_the_same_spot_inside_the_limit_is_a_double() {
        let at = Instant::now();
        let tab = spot(Target::Tab(1), 0);
        let mut mouse = Mouse::default();
        assert!(!mouse.clicked(tab, at));
        assert!(mouse.clicked(tab, at + Duration::from_millis(399)));
        assert!(
            !mouse.clicked(tab, at + Duration::from_millis(500)),
            "a double uses both up"
        );

        let mut mouse = Mouse::default();
        mouse.clicked(tab, at);
        assert!(!mouse.clicked(tab, at + DOUBLE_CLICK), "too slow");
        assert!(
            !mouse.clicked(spot(Target::Tab(1), 1), at + DOUBLE_CLICK),
            "another row"
        );
        assert!(
            !mouse.clicked(spot(Target::Tab(0), 1), at + DOUBLE_CLICK),
            "another target"
        );
    }

    #[test]
    fn the_thumb_is_at_the_top_the_middle_and_the_bottom_of_its_track() {
        // A hundred rows, ten showing, on a ten-cell track: a one-cell thumb
        // with nine cells to travel over ninety offsets.
        assert_eq!(thumb(0, 100, 10, 10), 0..1);
        assert_eq!(thumb(45, 100, 10, 10), 5..6);
        assert_eq!(thumb(90, 100, 10, 10), 9..10);
        assert_eq!(thumb(500, 100, 10, 10), 9..10, "past the end is the end");
        // The pad at 120x40: forty lines, thirteen showing.
        assert_eq!(thumb(0, 40, 13, 13), 0..4);
        assert_eq!(thumb(13, 40, 13, 13), 4..8);
        assert_eq!(thumb(27, 40, 13, 13), 9..13);
        assert_eq!(thumb(0, 5, 10, 10), 0..10, "all of it fits");
        assert_eq!(thumb(0, 100, 10, 0), 0..0, "no track");
    }

    #[test]
    fn a_seam_dragged_to_a_cell_is_drawn_there_up_to_a_hundred_cells() {
        for (total, least) in [(40, 20), (60, 20), (100, 20), (13, 3), (58, 3)] {
            for cells in least..=total - least {
                assert_eq!(
                    share(percent(cells, total, least), total, least),
                    cells,
                    "{cells} of {total}"
                );
            }
            assert_eq!(share(percent(0, total, least), total, least), least);
            assert_eq!(share(100, total, least), total - least);
        }
    }

    #[test]
    fn a_thumb_dragged_to_a_cell_scrolls_to_the_offset_drawn_there() {
        assert_eq!(offset(0, 100, 10, 10), 0);
        assert_eq!(offset(9, 100, 10, 10), 90);
        assert_eq!(offset(30, 100, 10, 10), 90, "dragged past the end");
        assert_eq!(offset(0, 5, 10, 10), 0, "nowhere to go");
        for (content, viewport, track) in [(100, 10, 10), (40, 13, 13), (11, 10, 10), (500, 19, 19)]
        {
            let travel = track - thumb(0, content, viewport, track).len() as u16;
            for start in 0..=travel {
                assert_eq!(
                    thumb(
                        offset(start, content, viewport, track),
                        content,
                        viewport,
                        track
                    )
                    .start,
                    start,
                    "{content} rows, {viewport} showing, {track} cells"
                );
            }
        }
    }
}
