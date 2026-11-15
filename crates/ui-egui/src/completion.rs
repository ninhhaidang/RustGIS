//! A list of suggestions floating by a text box's cursor, as in a code editor, with the help of
//! the highlighted item beside it: ↑/↓ move the highlight, Enter or Tab accept it, Escape closes
//! the list, or pressing an item accepts it; and the help floating by the cursor while it's in
//! something the help is about (`Tip`). What to offer and what the help says are the caller's
//! (the Script pane's `scriptcomplete`, an expression's `exprcomplete`); where the list goes, how
//! it scrolls, its keys and its state between frames are here, so every box that completes looks
//! and behaves the same.

use egui::text::CCursor;
use egui::widgets::text_edit::TextEditOutput;
use egui::{Id, Key, Modifiers, Ui};

/// The list's narrowest width.
const MIN_WIDTH: f32 = 220.0;
/// The help's widest line, so a long signature or parameter wraps rather than widen the list
/// without end.
pub(crate) const HELP_WIDTH: f32 = 360.0;
/// The narrowest the help gets beside the list; with less room it goes under the list.
const HELP_MIN_WIDTH: f32 = 200.0;
/// The gap between the list and the help.
pub(crate) const GAP: f32 = 12.0;
/// The popup frame's margins, both sides together.
pub(crate) const FRAME: f32 = 24.0;

/// What a list offers: the items, and where the word they replace ends (a char index of the
/// box's text), which the list goes under.
pub(crate) trait Offered: Clone + PartialEq {
    fn items(&self) -> &[String];
    fn end(&self) -> usize;
}

/// Where a popup of `size` (last frame's) goes by `cursor`: under it if it fits, else above if it
/// fits, else on the side with more room, a line and a half clear of the cursor's line (the line
/// next to it stays in view whole, with room around it); kept on screen sideways. Its corner,
/// and the height its content may take (it scrolls beyond: a short window, a long help).
pub(crate) fn place(cursor: egui::Rect, size: egui::Vec2, screen: egui::Rect, min: f32) -> (egui::Pos2, f32) {
    let cursor = cursor.expand2(egui::vec2(0.0, cursor.height() * 1.5));
    let (below, above) = (screen.bottom() - cursor.bottom(), cursor.top() - screen.top());
    let under = size.y <= below || (size.y > above && below >= above);
    let room = (if under { below } else { above } - FRAME).max(min);
    let y = if under { cursor.bottom() } else { cursor.top() - size.y.min(above) };
    let x = cursor.left().min(screen.right() - size.x).max(screen.left());
    (egui::pos2(x, y), room)
}

/// A popup at `corner`, its content scrolling past `room` high: where it was drawn, and its whole
/// size, the part scrolled out of view included, for `place` to choose a side next frame by what
/// it holds, not by what last fit (it would stay on a side too short for it).
pub(crate) fn popup(ui: &Ui, id: Id, corner: egui::Pos2, room: f32, content: impl FnOnce(&mut Ui)) -> (egui::Rect, egui::Vec2) {
    let area = egui::Area::new(id).order(egui::Order::Foreground).fixed_pos(corner).show(ui.ctx(), |ui| {
        egui::Frame::popup(ui.style())
            .show(ui, |ui| {
                // The area starts from its last size, so after a short list the scroll area would get
                // only that room back, scrolling with room to spare: ask for the full height whenever
                // it scrolls, as Command Search does (#255). Short content still shrinks it to fit.
                let out =
                    egui::ScrollArea::vertical().max_height(room).min_scrolled_height(room).auto_shrink([true, true]).show(ui, content);
                (out.content_size.y - out.inner_rect.height()).max(0.0)
            })
            .inner
    });
    let rect = area.response.rect;
    (rect, rect.size() + egui::vec2(0.0, area.inner))
}

/// What was worked out at a cursor, and for which text (its hash) and `with` (whatever else it
/// depends on, as its caller says): worked out again only when one changes.
#[derive(Clone)]
pub(crate) struct Worked<W, T> {
    pub at: Option<usize>,
    pub text: u64,
    pub with: W,
    pub value: T,
}

impl<W: PartialEq, T: Clone> Worked<W, T> {
    /// The value, if worked out for this cursor, text and `with`.
    pub fn get(this: &Option<Self>, at: Option<usize>, text: u64, with: &W) -> Option<T> {
        this.as_ref().filter(|w| w.at == at && w.text == text && w.with == *with).map(|w| w.value.clone())
    }
}

/// Help floating by a box's cursor while it's in something the help is about (a command's
/// braces, a function's call), so it stays in view while that is typed: on the cursor's line,
/// which it doesn't cover, and anchored sideways at where that starts, so it doesn't slide with
/// each key typed. Escape closes it until the cursor leaves what it was about (its key, `K`).
#[derive(Clone)]
pub(crate) struct Tip<K> {
    /// What it shows the help about: what Escape acts on.
    shown: Option<K>,
    /// What Escape closed: not shown again until the cursor leaves it.
    dismissed: Option<K>,
    /// Its whole size last frame, scroll aside (`popup`): where it goes.
    size: Option<egui::Vec2>,
}

impl<K> Default for Tip<K> {
    fn default() -> Self {
        Self { shown: None, dismissed: None, size: None }
    }
}

impl<K: Clone + PartialEq> Tip<K> {
    /// Whether it shows: Escape is then its own.
    pub fn is_shown(&self) -> bool {
        self.shown.is_some()
    }

    /// Before the box (its id `id`) sees the keys, with the help shown and the box focused:
    /// Escape closes it (and the box keeps the focus). Whether the keys were its to read.
    pub fn keys(&mut self, ui: &Ui, id: Id) -> bool {
        if self.shown.is_none() || !ui.memory(|m| m.has_focus(id)) {
            return false;
        }
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.dismissed = self.shown.take();
            self.size = None;
        }
        true
    }

    /// After the box (`out`): the help `found` for the cursor at char `at` (None: no cursor, or
    /// none to show it by, such as a list open over it), as what it's about and the char where
    /// that starts, drawn by `draw`
    /// (given the width it takes), `rows` monospace lines high at first guess. Not with the
    /// cursor out of the box's view (`visible`) or what Escape closed.
    pub fn show(
        &mut self,
        ui: &Ui,
        id: Id,
        out: &TextEditOutput,
        visible: egui::Rect,
        at: Option<usize>,
        found: Option<(K, usize)>,
        rows: usize,
        draw: impl FnOnce(&mut Ui, f32),
    ) {
        // Out of what Escape closed (in nothing, or straight into something else), it may show
        // again next time. Only with a cursor to show it by: with none (the window left, text
        // selected, a list open over it) where the cursor is isn't known, and it stays closed.
        let here = found.as_ref().map(|(k, _)| k);
        if at.is_some() && here != self.dismissed.as_ref() {
            self.dismissed = None;
        }
        let galley_rect = |i: usize| out.galley.pos_from_cursor(CCursor::new(i)).translate(out.galley_pos.to_vec2());
        let shown = found.filter(|(k, _)| self.dismissed.as_ref() != Some(k)).and_then(|(k, start)| {
            let cursor = galley_rect(at?);
            let left = galley_rect(start).left();
            let anchor = egui::Rect::from_x_y_ranges(left..=left, cursor.y_range());
            visible.intersects(cursor).then_some((anchor, k))
        });
        let Some((anchor, k)) = shown else {
            self.shown = None;
            self.size = None;
            return;
        };
        let row = ui.text_style_height(&egui::TextStyle::Monospace) + ui.spacing().item_spacing.y + 4.0;
        let screen = ui.ctx().content_rect();
        let width = (screen.width() - FRAME).clamp(0.0, HELP_WIDTH);
        let size = self.size.unwrap_or(egui::vec2(width + FRAME, row * rows as f32));
        let (corner, room) = place(anchor, size, screen, row * 3.0);
        let (_, size) = popup(ui, id, corner, room, |ui| draw(ui, width));
        self.shown = Some(k);
        self.size = Some(size);
    }
}

/// The list's state between frames.
#[derive(Clone)]
pub(crate) struct List<O> {
    /// The list shown, and its highlighted item: what the keys act on.
    open: Option<(O, usize)>,
    /// The offer Escape closed: not shown again until what's typed changes.
    dismissed: Option<O>,
    /// Where the list was drawn last frame: a press on it takes the focus from the box (a click
    /// elsewhere), so the list stays while the pointer is on it, for the press to reach it. And
    /// its whole size, scroll aside (`popup`): where it goes.
    drawn: Option<(egui::Rect, egui::Vec2)>,
    /// Whether the help follows the item under the pointer (it moved last) or the highlighted one
    /// (an arrow key moved it last): a pointer resting on the list mustn't hold the help while
    /// the arrows move the highlight.
    pointer_leads: bool,
    /// The highlighted item the list last scrolled to: it scrolls to a new one only (so a wheel
    /// can still move it).
    scrolled_to: Option<usize>,
}

impl<O> Default for List<O> {
    fn default() -> Self {
        Self { open: None, dismissed: None, drawn: None, pointer_leads: false, scrolled_to: None }
    }
}

impl<O: Offered> List<O> {
    /// Whether the list shows: Escape is then its own.
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Where the list was drawn last frame.
    #[cfg(test)]
    pub fn drawn(&self) -> Option<egui::Rect> {
        self.drawn.map(|(r, _)| r)
    }

    /// Drops the list shown without dismissing its offer (its text changed under it).
    pub fn forget(&mut self) {
        self.open = None;
    }

    /// Lets the offer Escape closed show again (asked for again, Ctrl+Space).
    pub fn reopen(&mut self) {
        self.dismissed = None;
    }

    /// Closes the list as Escape does: not shown again until what's typed changes. Whether one
    /// was open.
    pub fn close(&mut self) -> bool {
        let Some((offer, _)) = self.open.take() else { return false };
        self.dismissed = Some(offer);
        true
    }

    /// Before the box (its id `id`) sees the keys, with the list open and the box focused: ↑/↓
    /// move the highlight, Enter or Tab accept it (so Enter doesn't break the line, nor Tab indent
    /// or move the focus), Escape closes the list (and the box keeps the focus). Returns the item
    /// to accept, if any. Without the focus the keys are another widget's: none is taken.
    pub fn keys(&mut self, ui: &Ui, id: Id) -> Option<(O, String)> {
        let (offer, selected) = self.open.clone()?;
        if !ui.memory(|m| m.has_focus(id)) {
            return None;
        }
        let n = offer.items().len();
        // Nothing to move to nor accept (`show` opens no empty list).
        if n == 0 {
            self.open = None;
            return None;
        }
        let (up, down, accept, close) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter) || i.consume_key(Modifiers::NONE, Key::Tab),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if close {
            self.open = None;
            self.dismissed = Some(offer);
            None
        } else if accept {
            self.open = None;
            offer.items().get(selected).cloned().map(|item| (offer, item))
        } else {
            if up || down {
                let selected = if up { (selected + n - 1) % n } else { (selected + 1) % n };
                self.open = Some((offer, selected));
                self.pointer_leads = false;
            }
            None
        }
    }

    /// After the box (`out`, its id `id`): the offer `work` gives for its cursor (a char index;
    /// None with text selected), a new offer keeping the highlight on the same item when it is
    /// still there, drawn just under the cursor, each item after its `icon`, the highlighted
    /// item's `help` beside it (or under it, in a narrow window). `visible` is the part of the box
    /// that shows (its scroll's viewport: the text edit's own clip rect is its whole text).
    /// Returns an item clicked, to accept. The list closes when the box loses focus or nothing
    /// matches.
    pub fn show(
        &mut self,
        ui: &Ui,
        id: Id,
        out: &TextEditOutput,
        visible: egui::Rect,
        work: impl FnOnce(Option<usize>) -> Option<O>,
        icon: impl Fn(&O, &str) -> Option<&'static str>,
        help: impl FnOnce(&mut Ui, &O, &str, f32),
    ) -> Option<(O, String)> {
        // The window left (another app took the focus, the box keeps its own): the list closes, as a
        // code editor's does, and comes back only once what's typed or the cursor changes.
        if !ui.input(|i| i.focused) {
            self.close();
            self.drawn = None;
            return None;
        }
        // A press on the list: only then does it outlive the box's focus.
        let on_list =
            self.drawn.is_some_and(|(r, _)| ui.input(|i| i.pointer.any_down() && i.pointer.hover_pos().is_some_and(|p| r.contains(p))));
        let offer = if out.response.response.has_focus() {
            // A cursor, not a selection: with text selected, Enter and Tab act on it as usual.
            work(out.cursor_range.filter(|r| r.is_empty()).map(|r| r.primary.index.0))
        } else if on_list {
            // Pressed on the list: the box let go of the focus (and maybe of its cursor), so the list
            // shown stays as it is until the press reaches it.
            self.open.as_ref().map(|(offer, _)| offer.clone())
        } else {
            None
        };
        // An offer with nothing in it shows no list.
        let offer = offer.filter(|o| !o.items().is_empty());
        // Closed with Escape: not again for the same word.
        if offer.is_some() && self.dismissed == offer {
            self.open = None;
            self.drawn = None;
            return None;
        }
        self.dismissed = None;
        // A cursor scrolled out of the box's view shows no list, and keeps none open: Enter mustn't
        // accept a suggestion no one sees. It comes back with the cursor, as what `work` worked out
        // is kept.
        let cursor = offer.as_ref().map(|o| out.galley.pos_from_cursor(CCursor::new(o.end())).translate(out.galley_pos.to_vec2()));
        let (Some(offer), Some(cursor)) = (offer, cursor.filter(|c| visible.intersects(*c))) else {
            self.open = None;
            self.drawn = None;
            return None;
        };
        if ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO) {
            self.pointer_leads = true;
        }
        let pointer_leads = self.pointer_leads;
        // A new list starts from its top: its scroll is the old list's otherwise.
        if self.open.as_ref().is_none_or(|(o, _)| o.items() != offer.items()) {
            self.scrolled_to = None;
        }
        let kept = self.open.as_ref().and_then(|(o, s)| o.items().get(*s));
        let selected = kept.and_then(|item| offer.items().iter().position(|i| i == item)).unwrap_or(0);
        // Just under the cursor, or above it when the list would run off the bottom; kept on screen
        // sideways. Its size is last frame's (a long item, another font), estimated only on the
        // first frame it shows.
        let row = ui.text_style_height(&egui::TextStyle::Monospace) + ui.spacing().item_spacing.y + 4.0;
        let size = self.drawn.map_or(egui::vec2(MIN_WIDTH + 16.0, row * offer.items().len() as f32 + 12.0), |(_, s)| s);
        let screen = ui.ctx().content_rect();
        let (corner, room) = place(cursor, size, screen, row * 3.0);
        // The help beside the list in the room the screen leaves, or under it when that's too narrow
        // to read (a narrow window), so neither is cut off at the screen's edge.
        let beside = (screen.width() - MIN_WIDTH - GAP - FRAME).min(HELP_WIDTH);
        let (beside, help_width) =
            if beside >= HELP_MIN_WIDTH { (true, beside) } else { (false, (screen.width() - FRAME).clamp(0.0, HELP_WIDTH)) };
        let layout = if beside { egui::Layout::left_to_right(egui::Align::Min) } else { egui::Layout::top_down(egui::Align::Min) };
        let mut clicked = None;
        let scrolled_to = &mut self.scrolled_to;
        let (area, size) = popup(ui, id, corner, room, |ui| {
            ui.with_layout(layout, |ui| {
                // The help follows the item under the pointer, else the highlighted one.
                let mut hovered = None;
                let t = crate::theme::Tokens::get(ui.ctx());
                ui.vertical(|ui| {
                    ui.set_min_width(MIN_WIDTH);
                    for (i, item) in offer.items().iter().enumerate() {
                        let label = |ui: &mut Ui| ui.selectable_label(i == selected, egui::RichText::new(item).monospace());
                        let r = match icon(&offer, item) {
                            Some(icon) => {
                                ui.horizontal(|ui| {
                                    let (r, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                                    crate::icons::paint(ui.painter(), r, icon, t.icon, t.accent);
                                    label(ui)
                                })
                                .inner
                            }
                            None => label(ui),
                        };
                        // Taken as the button goes down, as in a code editor: the press already
                        // takes the focus from the box, and a click (down, then up a frame or more
                        // later) could find the list gone by then. A click still counts
                        // (assistive technology).
                        if r.clicked() || (r.hovered() && ui.input(|i| i.pointer.primary_pressed())) {
                            clicked = Some((offer.clone(), item.clone()));
                        }
                        if r.hovered() {
                            hovered = Some(i);
                        }
                        if i == selected && *scrolled_to != Some(selected) {
                            r.scroll_to_me(None);
                            *scrolled_to = Some(selected);
                        }
                    }
                });
                let shown = hovered.filter(|_| pointer_leads).unwrap_or(selected);
                if let Some(item) = offer.items().get(shown) {
                    help(ui, &offer, item, help_width);
                }
            });
        });
        if clicked.is_some() {
            self.open = None;
            self.drawn = None;
        } else {
            self.open = Some((offer, selected));
            self.drawn = Some((area, size));
        }
        clicked
    }
}

#[cfg(test)]
mod tests {
    /// Words on offer, as a box would offer them.
    #[derive(Clone, Debug, PartialEq)]
    struct Words(Vec<String>);

    impl super::Offered for Words {
        fn items(&self) -> &[String] {
            &self.0
        }

        fn end(&self) -> usize {
            0
        }
    }

    /// A list holding no items takes no arrow, Enter nor Tab (there's no highlight to move or
    /// accept): the key is left for the box, and the list closes.
    #[test]
    fn an_empty_list_takes_no_keys() {
        for key in [egui::Key::ArrowDown, egui::Key::ArrowUp, egui::Key::Enter, egui::Key::Tab] {
            let pressed = move |ui: &egui::Ui| {
                ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { key: k, pressed: true, .. } if *k == key)))
            };
            // Opened afresh each frame, so it is open when the key comes.
            let mut h = egui_kittest::Harness::new_ui_state(
                move |ui, left: &mut Option<bool>| {
                    let id = egui::Id::unique("box");
                    ui.memory_mut(|m| m.request_focus(id));
                    let mut list = super::List { open: Some((Words(Vec::new()), 0)), ..Default::default() };
                    let came = pressed(ui);
                    assert_eq!(list.keys(ui, id), None);
                    assert!(!list.is_open());
                    if came {
                        *left = Some(pressed(ui));
                    }
                },
                None,
            );
            h.key_press(key);
            h.run();
            assert_eq!(*h.state(), Some(true), "{key:?} left for the box");
        }
    }

    /// Without the box's focus the keys are another widget's: the list takes none.
    #[test]
    fn a_list_takes_no_keys_without_the_focus() {
        let list = super::List { open: Some((Words(vec!["one".to_string()]), 0)), ..Default::default() };
        let enter = |ui: &egui::Ui| ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { key: egui::Key::Enter, .. })));
        let mut h = egui_kittest::Harness::new_ui_state(
            move |ui, (list, left): &mut (super::List<Words>, bool)| {
                let came = enter(ui);
                assert_eq!(list.keys(ui, egui::Id::unique("box")), None);
                if came {
                    *left = enter(ui);
                }
            },
            (list, false),
        );
        h.key_press(egui::Key::Enter);
        h.run();
        assert!(h.state().1, "Enter left for whatever has the focus");
        assert!(h.state().0.is_open(), "the list stays as it was");
    }

    /// An offer with items opens the list under a focused box; one with none opens nothing.
    #[test]
    fn an_empty_offer_opens_no_list() {
        for (items, open) in [(vec!["one".to_string()], true), (Vec::new(), false)] {
            let offered = items.clone();
            let mut h = egui_kittest::Harness::new_ui_state(
                move |ui, (text, list): &mut (String, super::List<Words>)| {
                    let label = ui.label("Expression");
                    let out = egui::TextEdit::singleline(text).show(ui);
                    let _ = out.response.response.clone().labelled_by(label.id);
                    out.response.response.request_focus();
                    let visible = ui.clip_rect();
                    let offer = Words(offered.clone());
                    list.show(ui, egui::Id::unique("list"), &out, visible, |_| Some(offer), |_, _| None, |_, _, _, _| {});
                },
                (String::new(), super::List::default()),
            );
            h.run_steps(3);
            assert_eq!(h.state().1.is_open(), open, "{items:?}");
        }
    }

    /// A popup leaves a line and a half clear of the cursor's line: under it when it fits, else
    /// above it.
    #[test]
    fn a_popup_leaves_a_line_clear_of_the_cursor() {
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let cursor = egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(2.0, 20.0));
        let (corner, _) = super::place(cursor, egui::vec2(200.0, 100.0), screen, 60.0);
        assert_eq!(corner, egui::pos2(100.0, 150.0), "under, clear of the line below");
        let cursor = cursor.translate(egui::vec2(0.0, 400.0));
        let (corner, _) = super::place(cursor, egui::vec2(200.0, 100.0), screen, 60.0);
        assert_eq!(corner.y + 100.0, 470.0, "above, clear of the line over it");
    }
}
