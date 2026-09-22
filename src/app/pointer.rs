//! The mouse: what each frame says can be clicked, and the gestures that turn
//! presses, releases, motion and the wheel into the same state changes the
//! keys make.
//!
//! The renderer returns a [`Hits`] and the run loop hands the last one back
//! with every mouse event, so the app still never sees a terminal — and a
//! click resolves against exactly what was painted, overlays included.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use super::{Action, App, Focus};

/// How close together two clicks on one spot have to be to count as a
/// double-click: the common desktop default.
pub const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// How far one notch of the wheel scrolls.
const WHEEL: isize = 3;

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
    /// The text of the export prompt, starting at the region's left edge.
    PromptText,
    /// The body of the open help or inspector, whichever is on top.
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
        matches!(self, Self::Tab(_) | Self::Button { .. })
    }
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
            // A right-click does what a left click does; the menu it will
            // open after that has not been built yet.
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
            // A drag off anything but the pad's text does nothing; seams and
            // scrollbar thumbs will get arms of their own above this one.
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
                    let double = self.shell.mouse.clicked(press.spot, now);
                    // The column into the region, which only the prompt's
                    // text reads: a spot is a target and a row, no more.
                    let column = hits.at(position).map_or(0, |(rect, _)| position.x - rect.x);
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
            }
            Target::Source { .. } => self.shell.focus = Focus::Results,
            Target::Tab(index) if index < self.tabs.len() => self.shell.active_tab = index,
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
            // The way Esc goes: the prompt, then the help, then the
            // inspector, so a click beside one closes only the one on top.
            Target::Outside => {
                if self.shell.prompt.is_some() {
                    self.shell.prompt = None;
                } else if self.shell.help {
                    self.close_help();
                } else {
                    self.shell.inspector = None;
                }
            }
            Target::Tab(_) | Target::Overlay => {}
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
    fn pad_point(&mut self, (rect, target): (Rect, Target), position: Position, extend: bool) {
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
            self.pad_point(region, position, true);
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
}
