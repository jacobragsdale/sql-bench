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
                Vec::new()
            }
            // Nothing is dragged anywhere yet: what a drag does is decided
            // here, by what was pressed, once pad text, seams and scrollbar
            // thumbs are targets of their own.
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
                    self.click(press.spot.target, column, double)
                }
                _ => Vec::new(),
            },
            MouseEventKind::ScrollDown => self.wheel(spot, WHEEL),
            MouseEventKind::ScrollUp => self.wheel(spot, -WHEEL),
            _ => Vec::new(),
        }
    }

    /// A click, acted on the target that was pressed.
    ///
    /// Nothing drawn yet means anything different on a second click, so
    /// `_double` is only passed on; tree rows and grid cells will read it.
    fn click(&mut self, target: Target, column: u16, _double: bool) -> Vec<Action> {
        if let Target::Pane(Focus::Objects)
        | Target::Button {
            pane: Focus::Objects,
            ..
        } = target
        {
            self.end_filter_typing();
        }
        match target {
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

    /// The wheel scrolls what is under the pointer and never moves the focus.
    fn wheel(&mut self, spot: Option<Spot>, by: isize) -> Vec<Action> {
        // An open overlay's Outside is under everything of its own and over
        // everything else, so any other target is the overlay: its body, a
        // row of it or its close button. The help goes over the inspector,
        // so while it is open it is the one under the pointer.
        if spot.is_some_and(|spot| spot.target != Target::Outside) {
            if self.shell.help {
                self.scroll_help(by);
            } else if self.shell.inspector.is_some() {
                self.scroll_inspector(by);
            }
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
