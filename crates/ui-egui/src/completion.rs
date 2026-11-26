//! A list of suggestions floating by a text box's cursor, as in a code editor, with the help of
//! the highlighted item beside it: ↑/↓ move the highlight, Enter or Tab accept it, Escape closes
//! the list, or pressing an item accepts it; and the help floating by the cursor while it's in
//! something the help is about (`Tip`). What to offer and what the help says are the caller's
//! (the Script pane's `scriptcomplete`, an expression's `exprcomplete`); where the list goes, how
//! it scrolls, its keys and its state between frames are here, so every box that completes looks
//! and behaves the same. So is what tells a bracket or quote typed from a paste or an undo, for
//! the boxes whose brackets and quotes close themselves (`pairs`); which ones close, and where,
//! is each language's.

use egui::text::CCursor;
use egui::widgets::text_edit::TextEditOutput;
use egui::{Id, Key, Modifiers, Ui};
use std::collections::HashMap;
use std::sync::Arc;

/// The list's narrowest width.
const MIN_WIDTH: f32 = 220.0;
/// The most rows a list shows at once: it scrolls beyond, its help staying beside it.
pub(crate) const MAX_ROWS: usize = 10;
/// The fewest rows' height the help's box takes, beside a short list (`rows`).
const HELP_MIN_ROWS: usize = 6;
/// The help's scroll area's salt: the id of the state `rows` drops is worked out from it.
const HELP_SCROLL: &str = "help";
/// The side of a row's icon.
const ICON: f32 = 16.0;
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
    /// Whether its items have a help to show beside them (`rows`): a list of mere choices has
    /// none, and keeps no room for one.
    fn has_help(&self) -> bool {
        true
    }
}

/// Which side of the cursor's line (or a button) a popup goes on when it fits there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    /// Under it: a list or help reads on from where the cursor is.
    Below,
    /// Over it: what's under a one-line box (its error, its help) stays in view.
    Above,
}

/// Where a popup of `size` (last frame's) goes by `cursor`: on side `side` if it fits, else on
/// the other if it fits, else on the side with more room, a line and a half clear of the
/// cursor's line (the line next to it stays in view whole, with room around it); kept on screen
/// sideways. Its corner, and the height its content may take (it scrolls beyond: a short
/// window, a long help).
pub(crate) fn place(cursor: egui::Rect, size: egui::Vec2, screen: egui::Rect, min: f32, side: Side) -> (egui::Pos2, f32) {
    around(cursor.expand2(egui::vec2(0.0, cursor.height() * 1.5)), size, screen, min, side)
}

/// Where a popup of `size` goes by `anchor`: on side `side` if it fits, else on the other if it
/// fits, else on the side with more room; kept on screen sideways. Its corner, and the height
/// its content may take.
fn around(anchor: egui::Rect, size: egui::Vec2, screen: egui::Rect, min: f32, side: Side) -> (egui::Pos2, f32) {
    let (below, above) = (screen.bottom() - anchor.bottom(), anchor.top() - screen.top());
    let more_below = below >= above;
    let under = match side {
        Side::Below => size.y <= below || (size.y > above && more_below),
        Side::Above => size.y > above && (size.y <= below || more_below),
    };
    let room = (if under { below } else { above } - FRAME).max(min);
    let y = if under { anchor.bottom() } else { anchor.top() - size.y.min(above) };
    let x = anchor.left().min(screen.right() - size.x).max(screen.left());
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
    // A box inside a menu sits on a foreground layer too, and a click in it brings the menu to
    // the top, over the popup. As the menu's sublayer, the popup stays drawn right above it
    // (egui clears sublayers each pass, so this is set on every one); and where it's drawn is
    // kept for the menu, which would take a click on it past its own edge as one outside it.
    let parent = ui.layer_id();
    if parent.order == egui::Order::Foreground {
        ui.ctx().set_sublayer(parent, area.response.layer_id);
        let pass = ui.ctx().cumulative_pass_nr();
        ui.ctx().data_mut(|d| {
            let drawn = d.get_temp_mut_or_default::<Vec<(u64, egui::Rect)>>(over_menu_id());
            drawn.retain(|(at, _)| at + 1 >= pass);
            drawn.push((pass, rect));
        });
    }
    (rect, rect.size() + egui::vec2(0.0, area.inner))
}

fn over_menu_id() -> Id {
    Id::unique("completion_over_menu")
}

/// Whether the click under way (held, or let go this pass) was pressed on a popup drawn over a
/// menu (a menu's box's list, or its signature): that click is the popup's, not one outside the
/// menu, though the list, taking its item on the press, is gone where it's let go. The popup
/// drawn the last pass counts, as the menu checks its clicks before its box draws it again.
pub(crate) fn pressed_over_menu(ctx: &egui::Context) -> bool {
    let id = over_menu_id().with("pressed");
    // Where the button went down this pass, read from the pass's events: a press let go in the
    // same pass (a quick tap, a slow frame) has no `press_origin` left by then.
    let (pressed, held) = ctx.input(|i| {
        let pressed = i.events.iter().find_map(|e| match e {
            egui::Event::PointerButton { pos, pressed: true, .. } => Some(*pos),
            _ => None,
        });
        (pressed, i.pointer.any_down() || i.pointer.any_released())
    });
    if let Some(pos) = pressed {
        let pass = ctx.cumulative_pass_nr();
        let on = ctx
            .data(|d| d.get_temp::<Vec<(u64, egui::Rect)>>(over_menu_id()))
            .is_some_and(|drawn| drawn.iter().any(|(at, r)| at + 1 >= pass && r.contains(pos)));
        ctx.data_mut(|d| d.insert_temp(id, on));
        on
    } else if held {
        ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false)
    } else {
        ctx.data_mut(|d| d.remove::<bool>(id));
        false
    }
}

/// How a list `list` wide and its help sit in a screen `width` wide: the help beside the list in
/// the room the screen leaves, or under it when that's too narrow to read (a narrow window, a list
/// of long names), so neither is cut off at the screen's edge; and the help's width.
fn help_layout(width: f32, list: f32) -> (egui::Layout, f32) {
    let beside = (width - list - GAP - FRAME).min(HELP_WIDTH);
    if beside >= HELP_MIN_WIDTH {
        (egui::Layout::left_to_right(egui::Align::Min), beside)
    } else {
        (egui::Layout::top_down(egui::Align::Min), (width - FRAME).clamp(0.0, HELP_WIDTH))
    }
}

/// One row of a list: `item`, highlighted when `selected`, after its `icon`.
fn item_row(ui: &mut Ui, item: &str, selected: bool, icon: Option<&str>) -> egui::Response {
    let label = |ui: &mut Ui| ui.selectable_label(selected, egui::RichText::new(item).monospace());
    if let Some(icon) = icon {
        let t = crate::theme::Tokens::get(ui.ctx());
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(ICON, ICON), egui::Sense::hover());
            crate::icons::paint(ui.painter(), r, icon, t.icon, t.accent);
            label(ui)
        })
        .inner
    } else {
        label(ui)
    }
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
/// braces, a function's call), so it stays in view while that is typed: by the cursor's line,
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

    /// Instead of `show`, with a cursor in `here` (what the help would be about there) but other
    /// help shown over it (a function's signature over its command's): not shown, and what Escape
    /// closed stays closed only while the cursor is still in it.
    pub fn hide(&mut self, here: Option<&K>) {
        if here != self.dismissed.as_ref() {
            self.dismissed = None;
        }
        self.shown = None;
        self.size = None;
    }

    /// After the box (`out`): the help `found` for the cursor at char `at` (None: no cursor, or
    /// none to show it by, such as a list open over it), as what it's about and the char where
    /// that starts, drawn by `draw`
    /// (given the width it takes), `rows` monospace lines high at first guess, on side `side` of
    /// the cursor's line when it fits there. Not with the cursor out of the box's view
    /// (`visible`) or what Escape closed.
    pub fn show(
        &mut self,
        ui: &Ui,
        id: Id,
        out: &TextEditOutput,
        visible: egui::Rect,
        at: Option<usize>,
        found: Option<(K, usize)>,
        rows: usize,
        side: Side,
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
        let (corner, room) = place(anchor, size, screen, row * 3.0, side);
        let (_, drawn) = popup(ui, id, corner, room, |ui| draw(ui, width));
        // Placed by a guess or by other help's size: over the line, its bottom edge would sit
        // off the line until something repaints, so place it again by what it holds now.
        if drawn != size {
            ui.ctx().request_repaint();
        }
        self.shown = Some(k);
        self.size = Some(drawn);
    }
}

/// Where Ctrl+Space asked a box for its list (the text's hash and the cursor): the list shows
/// there with nothing typed, until the text or the cursor moves on.
#[derive(Clone, Default)]
pub(crate) struct Asked(Option<(u64, usize)>);

impl Asked {
    /// Before the box (its id `id`, its text `text`) sees the keys, with it focused: Ctrl+Space
    /// asks for the list at its cursor. Whether it did (the list Escape closed may show again,
    /// `List::reopen`).
    pub fn keys(&mut self, ui: &Ui, id: Id, text: &str) -> bool {
        if !ui.memory(|m| m.has_focus(id)) || !ui.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::Space)) {
            return false;
        }
        let at = crate::widgets::cursor_range(ui.ctx(), id).map(|r| r.primary.index.0);
        self.0 = at.map(|at| (egui::util::hash(text), at));
        true
    }

    /// Whether the list was asked for at cursor `at` of the text hashed `text`, and still is
    /// there; once either moved on, forgotten.
    pub fn here(&mut self, text: u64, at: Option<usize>) -> bool {
        let here = at.is_some_and(|at| self.0 == Some((text, at)));
        if !here {
            self.0 = None;
        }
        here
    }
}

/// The list's state between frames.
#[derive(Clone)]
pub(crate) struct List<O> {
    /// The list shown, and its highlighted item: what the keys act on.
    open: Option<(O, usize)>,
    /// The offer Escape closed: not shown again until what's typed, or what it offers (a field's
    /// type read again), changes.
    dismissed: Option<O>,
    /// Where the list was drawn last frame: a press on it takes the focus from the box (a click
    /// elsewhere), so the list stays while the pointer is on it, for the press to reach it. And
    /// its whole size, scroll aside (`popup`): where it goes.
    drawn: Option<(egui::Rect, egui::Vec2)>,
    /// Whether the help follows the item under the pointer (it moved last) or the highlighted one
    /// (an arrow key moved it last): a pointer resting on the list mustn't hold the help while
    /// the arrows move the highlight.
    pointer_leads: bool,
    /// Where its rows scrolled to, and how wide they are.
    scroll: Scroll,
    /// Whether the button pressed on the list is still down: egui takes the focus from the box
    /// when it comes up (a click elsewhere), and it goes back to the box then.
    pressed: bool,
}

impl<O> Default for List<O> {
    fn default() -> Self {
        Self { open: None, dismissed: None, drawn: None, pointer_leads: false, scroll: Scroll::default(), pressed: false }
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

    /// Closes the list as Escape does: not shown again until what's typed, or what it offers,
    /// changes. Whether one was open.
    pub fn close(&mut self) -> bool {
        let Some((offer, _)) = self.open.take() else { return false };
        self.dismissed = Some(offer);
        true
    }

    /// Before the box (its id `id`) sees the keys, with the list open and the box focused: ↑/↓
    /// move the highlight, Enter or Tab accept it (so Enter doesn't break the line, nor Tab indent
    /// or move the focus), Escape closes the list (and the box keeps the focus), Page Up / Page Down
    /// scroll its help when it has one. Returns the item to accept, if any. Without the focus the
    /// keys are another widget's: none is taken.
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
        // Page Up / Page Down scroll the help, not the box, while a list with one is open: the
        // keyboard's way to read a help longer than its box. Not when the list closes this frame:
        // the next list mustn't open scrolled.
        if offer.has_help() && !close && !accept {
            self.scroll.page = (self.scroll.page + page_keys(ui)).clamp(-MAX_PAGES, MAX_PAGES);
        }
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
    /// item's `help` beside it (or under it, in a narrow window), and what each says (`describe`)
    /// for a screen reader, the highlighted one read as the box's, kept while `said` (a key of what
    /// `describe` reads for the offer, asked only while a list shows) gives the same. `visible` is
    /// the part of the box that shows (its scroll's viewport: the text edit's own clip rect is its
    /// whole text).
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
        said: impl FnOnce(&O) -> u64,
        describe: impl Fn(&O, &str) -> Option<String>,
        help: impl FnOnce(&mut Ui, &O, &str, f32),
    ) -> Option<(O, String)> {
        // A page pressed (`keys`), or waiting for a help that changed (`rows`), is for the list it
        // was pressed for, while that list stays open with a help: taken now and handed back below
        // only then.
        let page = std::mem::take(&mut self.scroll.page);
        // Closed: what the rows said goes with the list.
        if self.open.is_none() {
            self.scroll.described = Arc::default();
        }
        // The window left (another app took the focus, the box keeps its own): the list closes, as a
        // code editor's does, and comes back only once what's typed or the cursor changes.
        if !ui.input(|i| i.focused) {
            self.close();
            self.drawn = None;
            self.pressed = false;
            return None;
        }
        // A press on the list: only then does it outlive the box's focus. Pressed this frame
        // counts though the button is up again (a quick tap, a slow frame): the item takes it.
        let on_list = self.drawn.is_some_and(|(r, _)| {
            ui.input(|i| (i.pointer.any_down() || i.pointer.any_pressed()) && i.pointer.hover_pos().is_some_and(|p| r.contains(p)))
        });
        // The button pressed there, once up, took the focus from the box (a click outside it, a
        // single-line box's list being under it), the list having taken the press: what was
        // clicked was the box's list, so the focus goes back to it, and the list shows again
        // next frame if there's still something to offer there (more after the item accepted,
        // or the item that didn't fit).
        if on_list && ui.input(|i| i.pointer.any_pressed()) {
            self.pressed = true;
        } else if self.pressed && ui.input(|i| !i.pointer.any_down()) {
            self.pressed = false;
            ui.memory_mut(|m| m.request_focus(out.response.response.id));
        }
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
        let same = self.open.as_ref().is_some_and(|(o, _)| o.items() == offer.items());
        if !same {
            self.scroll = Scroll::default();
        } else if self.open.as_ref().is_some_and(|(o, _)| *o != offer) {
            // The same items, but what they say may not be (a field's type, read again).
            self.scroll.described = Arc::default();
        }
        // Or what they're read from changed, with the same offer (a layer's count of features).
        let key = said(&offer);
        if self.scroll.said != key {
            self.scroll.described = Arc::default();
            self.scroll.said = key;
        }
        let kept = self.open.as_ref().and_then(|(o, s)| o.items().get(*s));
        let selected = kept.and_then(|item| offer.items().iter().position(|i| i == item)).unwrap_or(0);
        // Just under the cursor, or above it when the list would run off the bottom; kept on screen
        // sideways. Its size is last frame's (a long item, another font), estimated only on the
        // first frame it shows.
        let row = row_height(ui);
        let size = self.drawn.map_or(egui::vec2(MIN_WIDTH + 16.0, row * offer.items().len().min(MAX_ROWS) as f32 + 12.0), |(_, s)| s);
        let screen = ui.ctx().content_rect();
        let (corner, room) = place(cursor, size, screen, row * 3.0, Side::Below);
        let mut clicked = None;
        let mut highlighted = None;
        let has_help = offer.has_help();
        self.scroll.page = if same && has_help { page } else { 0 };
        let scroll = &mut self.scroll;
        let items = offer.items();
        let (area, drawn) = popup(ui, id, corner, room, |ui| {
            // The focus stays on the box: the rows take none.
            let picked = rows(
                ui,
                items,
                selected,
                pointer_leads,
                scroll,
                false,
                has_help,
                |item| icon(&offer, item),
                |i| items.get(i).and_then(|item| describe(&offer, item)),
                |ui, item, width| {
                    help(ui, &offer, item, width);
                },
            );
            clicked = picked.clicked.and_then(|i| items.get(i)).map(|item| (offer.clone(), item.clone()));
            highlighted = picked.highlighted;
        });
        // The focus stays on the box, the highlighted row its active descendant: a screen reader
        // reads that row (and what it says, `describe`) as the highlight moves, as it would a
        // focused one, while typing goes on in the box (#2619). Set each frame the list shows, so
        // gone with it.
        if let Some(row) = highlighted.filter(|_| clicked.is_none() && out.response.response.has_focus()) {
            ui.ctx().accesskit_node_builder(out.response.response.id, |node| node.set_active_descendant(row.accesskit_id()));
        }
        if clicked.is_some() {
            self.open = None;
            self.drawn = None;
            // Closed: a page still waiting (the help changed this frame) goes with the list.
            self.scroll.page = 0;
        } else {
            self.open = Some((offer, selected));
            // Placed by a guess (its first frame) or by another list's size: place it again by
            // what it holds now, not on the next input.
            if drawn != size {
                ui.ctx().request_repaint();
            }
            self.drawn = Some((area, drawn));
        }
        clicked
    }
}

/// The height a list's row takes, the space after it included.
fn row_height(ui: &Ui) -> f32 {
    let s = ui.spacing();
    (ui.text_style_height(&egui::TextStyle::Monospace) + 2.0 * s.button_padding.y).max(s.interact_size.y) + s.item_spacing.y
}

/// A list's rows between frames, kept while the list is the same.
#[derive(Clone, Default)]
struct Scroll {
    /// The highlighted item the list last scrolled to: it scrolls to a new one only (so a wheel
    /// can still move it).
    to: Option<usize>,
    /// The widest row, worked out once: with only the rows in view drawn, theirs alone would
    /// widen and narrow the list as it scrolls.
    widest: Option<f32>,
    /// The item whose help shows: another one's starts from the top of the help's box.
    help_for: Option<usize>,
    /// Where the help's box was, the gap before it included: with the pointer there, the help
    /// stays the one shown (the item the pointer left, to read or scroll its help).
    help_at: Option<egui::Rect>,
    /// What each row says to a screen reader, worked out once per row while the offer and what it's
    /// read from (`said`) are the same (`rows`). Shared, not copied, with each copy of the box's
    /// state (`load` clones it every frame): a row worked out copies it at most once in that frame.
    described: Arc<HashMap<usize, Option<String>>>,
    /// What `described` was read from (`List::show`'s `said`).
    said: u64,
    /// Page Up (-1) or Page Down (1) pressed for the help, not yet applied (`rows`).
    page: i32,
    /// How far the help's box scrolled at most last frame: a page down stops there.
    help_max: f32,
}

/// What a list's rows gave this frame.
#[derive(Default)]
struct Rows {
    /// The item pressed.
    clicked: Option<usize>,
    /// The row with the focus (a screen reader put it there, or `focus` moved it).
    focused: Option<usize>,
    /// The highlighted row's widget, when it's drawn: a box's list names it to a screen reader as
    /// the box's active descendant (`List::show`).
    highlighted: Option<Id>,
}

/// The widest of `items`' rows, each after its `icon`, as `item_row` draws them; the list's
/// narrowest width at least.
fn widest(ui: &Ui, items: &[String], icon: impl Fn(&str) -> Option<&'static str>) -> f32 {
    let s = ui.spacing();
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    items
        .iter()
        .map(|item| {
            let text = ui.painter().layout_no_wrap(item.clone(), font.clone(), egui::Color32::PLACEHOLDER).size().x;
            let icon = if icon(item).is_some() { ICON + s.item_spacing.x } else { 0.0 };
            icon + text + 2.0 * s.button_padding.x
        })
        .fold(MIN_WIDTH, f32::max)
}

/// What a list shows in its popup, whether a box's suggestions (`List`) or a button's (`menu`),
/// so every list looks the same: `items`, the `selected` one highlighted, each after its `icon`,
/// and beside them, in the room the screen leaves (under them when that's too narrow to read: a
/// narrow window), the `help` on the item under the pointer when `pointer_leads`, else on the
/// highlighted one. Scrolls to the highlighted one when it changes (`scroll`); with `focus`, the
/// highlighted row takes the focus too.
///
/// The help, when the list `has_help`, goes in a box of one size for the whole list: as wide as
/// the layout gives it and as tall as the list's rows (`HELP_MIN_ROWS` at least, for a short
/// list), scrolling a longer help. Sized to each help, the popup would change size from item to
/// item and `around` would move it, sideways by its width at the screen's right edge, up and
/// down by its height above the cursor or across to the other side: a row would move out from
/// under the pointer (#2618). Its size depends only on the list, the screen and the font, so it
/// stays put while the list is open.
fn rows(
    ui: &mut Ui,
    items: &[String],
    selected: usize,
    pointer_leads: bool,
    scroll: &mut Scroll,
    focus: bool,
    has_help: bool,
    icon: impl Fn(&str) -> Option<&'static str>,
    describe: impl Fn(usize) -> Option<String>,
    help: impl FnOnce(&mut Ui, &str, f32),
) -> Rows {
    // Worked out once: with only the rows in view drawn, theirs alone would widen and narrow the
    // list as it scrolls; and the help's room beside it depends on it.
    let list = *scroll.widest.get_or_insert_with(|| widest(ui, items, &icon));
    let (layout, help_width) = help_layout(ui.ctx().content_rect().width(), list);
    let mut out = Rows::default();
    ui.with_layout(layout, |ui| {
        let mut hovered = None;
        let row = row_height(ui);
        let spacing = ui.spacing().item_spacing.y;
        // At most `MAX_ROWS` rows show: the list scrolls beyond (a long one mustn't fill the screen).
        let area = egui::ScrollArea::vertical().id_salt("rows").max_height(row * MAX_ROWS as f32 - spacing).auto_shrink([true, true]);
        let mut described = std::mem::take(&mut scroll.described);
        let mut draw = |ui: &mut Ui, i: usize, item: &str| {
            let r = item_row(ui, item, i == selected, icon(item));
            // What the help says, for a screen reader: worked out only when one is there, and once
            // per row while the list is the same (`Scroll::described`), not for each of some 600
            // rows each frame.
            if ui.ctx().has_accesskit_node(r.id) {
                if !described.contains_key(&i) {
                    // An empty one says nothing: none is given (a field with no type nor alias).
                    Arc::make_mut(&mut described).insert(i, describe(i).filter(|d| !d.is_empty()));
                }
                if let Some(Some(d)) = described.get(&i) {
                    let d = d.clone();
                    ui.ctx().accesskit_node_builder(r.id, |node| node.set_description(d));
                }
            }
            // Taken as the button goes down, as in a code editor: the press already takes the
            // focus from the box, and a click (down, then up a frame or more later) could find
            // the list gone by then. A click still counts (assistive technology).
            if r.clicked() || (r.hovered() && ui.input(|i| i.pointer.primary_pressed())) {
                out.clicked = Some(i);
            }
            if r.hovered() {
                hovered = Some(i);
            }
            if i == selected {
                out.highlighted = Some(r.id);
            }
            if r.has_focus() {
                out.focused = Some(i);
                ui.memory_mut(|m| m.set_focus_lock_filter(r.id, list_keys(true)));
            }
            if focus && i == selected {
                r.request_focus();
                out.focused = Some(i);
            }
            r
        };
        // In a column of its own: the scroll area's content takes the layout it's in (beside the
        // help, a row).
        ui.vertical(|ui| {
            // Each `Ui` is a node of the accessibility tree while a screen reader reads the app:
            // every row is drawn then, so one scrolled out of view is still there to reach. Else
            // only those in view are (Ctrl+Space on an empty word offers some 600).
            if ui.ctx().has_accesskit_node(ui.unique_id()) {
                area.show(ui, |ui| {
                    ui.set_min_width(list);
                    for (i, item) in items.iter().enumerate() {
                        let r = draw(ui, i, item);
                        if i == selected && scroll.to != Some(selected) {
                            r.scroll_to_me(None);
                            scroll.to = Some(selected);
                        }
                    }
                });
            } else {
                area.show_rows(ui, row - spacing, items.len(), |ui, range| {
                    ui.set_min_width(list);
                    // The highlighted row may be out of the range drawn: where it would be.
                    if scroll.to != Some(selected) {
                        let top = ui.max_rect().top() + (selected as f32 - range.start as f32) * row;
                        ui.scroll_to_rect(egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=top + row - spacing), None);
                        scroll.to = Some(selected);
                    }
                    for (i, item) in items.iter().enumerate().skip(range.start).take(range.len()) {
                        draw(ui, i, item);
                    }
                });
            }
        });
        scroll.described = described;
        if !has_help {
            return;
        }
        // The help follows the item under the pointer, else the highlighted one; with the pointer
        // gone from a row to the help (to read or scroll it), that row's help stays. After a gap,
        // beside the list or under it.
        let on_help = scroll.help_at.is_some_and(|r| ui.rect_contains_pointer(r));
        let shown = match hovered {
            Some(i) if pointer_leads => i,
            _ if pointer_leads && on_help => scroll.help_for.unwrap_or(selected),
            _ => selected,
        };
        let size = egui::vec2(help_width, row * items.len().clamp(HELP_MIN_ROWS, MAX_ROWS) as f32 - spacing);
        ui.add_space(GAP);
        let boxed = ui.allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Min), |ui| {
            // Set first, so it counts from the box's corner: set after, a minimum counts from
            // where the content ended.
            ui.set_min_size(size);
            // Another item's help starts afresh: all of the scroll's state dropped, not only its
            // offset (a fling on a touchscreen leaves a velocity that would scroll the next help).
            // Its id worked out here as `ScrollArea::begin` does: egui keeps that state after the
            // list closes, and a list opened again starts with no `Scroll`, so it drops it too.
            let id = ui.make_persistent_id(egui::IdSalt::new(HELP_SCROLL));
            let changed = scroll.help_for != Some(shown);
            if changed {
                ui.data_mut(|d| d.remove::<egui::scroll_area::State>(id));
                scroll.help_for = Some(shown);
            }
            let (seen, step) = help_view(ui.max_rect(), ui.clip_rect(), row);
            let mut area = egui::ScrollArea::vertical().id_salt(HELP_SCROLL).max_height(seen).auto_shrink([false, false]);
            // A page from the keyboard: what shows less a line, kept in the help (as far as it
            // scrolled last frame: on the frame another item's help comes, it waits a frame).
            if scroll.page != 0 && !changed {
                let at = egui::scroll_area::State::load(ui.ctx(), id).map_or(0.0, |s| s.clamped_offset().y);
                area = area.vertical_scroll_offset((at + scroll.page as f32 * step).clamp(0.0, scroll.help_max));
                scroll.page = 0;
            }
            let shown_area = area.show(ui, |ui| {
                if let Some(item) = items.get(shown) {
                    help(ui, item, help_width);
                }
            });
            scroll.help_max = (shown_area.content_size.y - shown_area.inner_rect.height()).max(0.0);
        });
        scroll.help_at = Some(boxed.response.rect.expand(GAP));
    });
    out
}

/// How the help scrolls in its box `boxed`, of which the popup, held to its room and maybe
/// scrolled, shows what's inside `clip`: the height its scroll area takes, from the box's top to
/// the shown part's bottom (so the help's end comes into view there), and a page, the shown part
/// less a line (so no line passes unseen). A row at least of each, so a page always moves: with
/// less than a line shown, that sliver goes by unread.
fn help_view(boxed: egui::Rect, clip: egui::Rect, row: f32) -> (f32, f32) {
    let floor = row.min(boxed.height());
    let seen = (clip.bottom() - boxed.top()).clamp(floor, boxed.height());
    let shown = (clip.bottom().min(boxed.bottom()) - clip.top().max(boxed.top())).max(0.0);
    (seen, (shown - row).max(row))
}

/// What a list's row says to a screen reader: its `head`, then its `detail` on a line of its own
/// when there's one (the Script's commands and keys, the expression's items).
pub(crate) fn spoken(head: &str, detail: Option<&str>) -> String {
    match detail.filter(|d| !d.is_empty()) {
        Some(detail) => format!("{head}\n{detail}"),
        None => head.to_string(),
    }
}

/// The most pages pressed ahead of the help's scroll (one waits a frame for a new help).
const MAX_PAGES: i32 = 10;

/// Page Down (1) or Page Up (-1) pressed, taken for a list's help (egui's `TextEdit` uses
/// neither).
fn page_keys(ui: &Ui) -> i32 {
    ui.input_mut(|i| i32::from(i.consume_key(Modifiers::NONE, Key::PageDown)) - i32::from(i.consume_key(Modifiers::NONE, Key::PageUp)))
}

/// The keys a widget of an `open` list keeps from egui: the arrows and Escape are the list's
/// (egui would move the focus to the nearest widget, or drop it); Tab stays egui's.
fn list_keys(open: bool) -> egui::EventFilter {
    egui::EventFilter { tab: false, horizontal_arrows: open, vertical_arrows: open, escape: open }
}

/// Where the pass a button's list was last shown open in is kept.
fn open_id() -> Id {
    Id::unique("completion_menu_open")
}

/// Whether a button's list (`menu`) is open: its keys (Escape, Enter, the arrows) are its own
/// then, as a dialog's are, so the app's shortcuts leave them (Escape switching to Pan).
pub(crate) fn menu_is_open(ctx: &egui::Context) -> bool {
    let pass = ctx.cumulative_pass_nr();
    ctx.data(|d| d.get_temp::<u64>(open_id())).is_some_and(|open| open + 1 >= pass)
}

/// A button's list between frames.
#[derive(Clone, Default)]
struct MenuState {
    open: bool,
    selected: usize,
    pointer_leads: bool,
    scroll: Scroll,
    /// Whether a row had the focus last pass: it goes back to the button as the list closes.
    row_focused: bool,
    /// Where it was drawn last, and its whole size (`popup`).
    drawn: Option<(egui::Rect, egui::Vec2)>,
    /// The pass the button was last shown in.
    pass: u64,
}

/// A button `label` that opens the list of `items` (worked out only while it's open) under it
/// (above it, with no room under it), looking and behaving as a box's suggestion list does
/// (`rows`): the first item highlighted, ↑/↓ move the highlight (and the focus along, from a row a
/// screen reader focused, which takes the highlight), Enter accepts it, a click accepts
/// the item clicked, Escape or a press elsewhere closes the list; each item after its `icon`, with
/// the `help` on the highlighted one, or the one under the pointer, beside it when it `has_help`,
/// and what it says (`describe`) given to a screen reader with each. The same from the keyboard:
/// Tab to the button, Enter (or Space) opens it, and Tab on to another widget closes it. Returns
/// the button, and the item accepted.
pub(crate) fn menu<L>(
    ui: &mut Ui,
    label: impl Into<egui::WidgetText>,
    items: impl FnOnce() -> L,
    icon: impl Fn(&str) -> Option<&'static str>,
    describe: impl Fn(usize) -> Option<String>,
    has_help: bool,
    help: impl FnOnce(&mut Ui, &str, f32),
) -> (egui::Response, Option<String>)
where
    L: std::ops::Deref<Target = [String]>,
{
    let button = ui.button(label);
    let id = button.id.with("menu");
    let pass = ui.ctx().cumulative_pass_nr();
    let mut s: MenuState = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    // Not shown last pass (its dialog closed, its pane hidden): closed, not open again on return,
    // and a row focused then is long gone (the focus mustn't come back to the button for it).
    if s.pass + 1 < pass {
        s.open = false;
        s.row_focused = false;
    }
    // Enter on the button (focused from the keyboard) while the list is open is the list's: it
    // accepts the highlighted item rather than close the list.
    let toggled = button.clicked() && !(s.open && ui.input(|i| i.key_pressed(Key::Enter)));
    if toggled {
        s = MenuState { open: !s.open, ..MenuState::default() };
        // The key that pressed the button (focused from the keyboard) is the list's: a dialog's
        // Enter mustn't also take it and run the dialog with the clause half written.
        ui.input_mut(|i| {
            i.consume_key(Modifiers::NONE, Key::Enter);
            i.consume_key(Modifiers::NONE, Key::Space);
        });
    }
    s.pass = pass;
    // Lent, not copied: a field's values can run to thousands (`fieldvalues`).
    let held = s.open.then(items);
    let items: &[String] = held.as_deref().unwrap_or_default();
    let n = items.len();
    // While it's open the arrows and Escape are the list's, not egui's: they'd move the focus off
    // the button (to another button, which Enter would then press) or drop it. A focused row's
    // too (`rows`).
    ui.memory_mut(|m| m.set_focus_lock_filter(button.id, list_keys(s.open)));
    let mut accepted = None;
    // The focus going to another widget (Tab), or gone to one already: the list closes, as a box's
    // does, before it takes any key that widget's. Not on a press, whose widget may be an item of
    // the list being clicked; nor while nothing has the focus (a click doesn't give it to the
    // button); nor while it's on a row of the list itself (a screen reader focuses a row before
    // it activates it).
    let in_list = |f: Id| ui.ctx().read_response(f).is_some_and(|r| r.layer_id == egui::LayerId::new(egui::Order::Foreground, id));
    let leaving = ui.input(|i| i.key_pressed(Key::Tab)) || ui.memory(|m| m.focused()).is_some_and(|f| f != button.id && !in_list(f));
    if s.open && !toggled && leaving && !ui.input(|i| i.pointer.any_pressed()) {
        s.open = false;
    }
    // A row focused (by a screen reader) takes Enter itself, as its own click: that row, not the
    // highlighted one.
    let on_row = ui.memory(|m| m.focused()).is_some_and(|f| f != button.id && in_list(f));
    // With a row focused, an arrow takes the focus along to the row it highlights: Enter takes
    // that row. (Only a screen reader focuses a row, and every row is drawn then, so the row is
    // there to take it this pass.)
    let mut follow = false;
    // Not the keys that opened it (Enter would accept the first item at once).
    if s.open && n > 0 && !toggled {
        let (up, down, enter, escape) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                !on_row && i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        // Page Up / Page Down scroll the help, as in a box's list (`List::keys`).
        if has_help {
            s.scroll.page = (s.scroll.page + page_keys(ui)).clamp(-MAX_PAGES, MAX_PAGES);
        }
        let selected = s.selected.min(n - 1);
        if up || down {
            s.selected = if up { (selected + n - 1) % n } else { (selected + 1) % n };
            s.pointer_leads = false;
            follow = on_row;
            // Nor does egui move the focus too: a row given the focus last pass hasn't kept its
            // arrows from egui yet (a widget's keys are its own from the pass after it took it).
            ui.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
        }
        // A press neither on the button nor on the list closes it, as a menu's does.
        let drawn = s.drawn.map(|(r, _)| r);
        let elsewhere = ui.input(|i| {
            i.pointer.any_pressed()
                && i.pointer.interact_pos().is_some_and(|p| !button.rect.contains(p) && !drawn.is_some_and(|r| r.contains(p)))
        });
        if escape || elsewhere {
            s.open = false;
        } else if enter {
            accepted = Some(selected);
        }
    }
    if s.open && n > 0 && accepted.is_none() {
        if ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO) {
            s.pointer_leads = true;
        }
        let row = row_height(ui);
        let size = s.drawn.map_or(egui::vec2(MIN_WIDTH + 16.0, row * n.min(MAX_ROWS) as f32 + 12.0), |(_, size)| size);
        let (corner, room) = around(button.rect, size, ui.ctx().content_rect(), row * 3.0, Side::Below);
        let (selected, pointer_leads) = (s.selected.min(n - 1), s.pointer_leads);
        let scroll = &mut s.scroll;
        let mut got = Rows::default();
        let drawn = popup(ui, id, corner, room, |ui| {
            got = rows(ui, items, selected, pointer_leads, scroll, follow, has_help, icon, describe, help)
        });
        // The arrows move the highlight with the focus on the button: the highlighted row is the
        // button's active descendant, read as a box's list's is (`List::show`).
        if let Some(row) = got.highlighted.filter(|_| got.clicked.is_none() && ui.memory(|m| m.has_focus(button.id))) {
            ui.ctx().accesskit_node_builder(button.id, |node| node.set_active_descendant(row.accesskit_id()));
        }
        // Placed by a guess (its first frame): place it again by what it holds now.
        if drawn.1 != size {
            ui.ctx().request_repaint();
        }
        s.drawn = Some(drawn);
        accepted = got.clicked;
        s.row_focused = got.focused.is_some();
        // A row a screen reader focused takes the highlight, and its help: the arrows go on from
        // it.
        if let Some(f) = got.focused.filter(|f| *f != selected) {
            s.selected = f;
            s.pointer_leads = false;
            ui.ctx().request_repaint();
        }
        // The focused row has taken its Enter: a dialog's Enter mustn't also take it.
        if on_row {
            ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
        }
        // Still open: the app's shortcuts leave the keys to it next pass (not after a click on an
        // item, which closed it).
        if accepted.is_none() {
            ui.data_mut(|d| d.insert_temp(open_id(), pass));
        }
    }
    if accepted.is_some() || !s.open {
        // A row had the focus: it goes back to the button rather than to nothing, as the row goes.
        // Escape in the pass after an arrow gave a row the focus has dropped it already (the row
        // hadn't kept Escape from egui yet): the row had it last pass, then. Tab goes on past the
        // list instead, as from the button: the row lets go, and egui's move to the next widget
        // (pending, the row not drawn to take it) gives the focus to the next one that wants it.
        if on_row || (s.row_focused && ui.memory(|m| m.focused()).is_none()) {
            if ui.input(|i| i.key_pressed(Key::Tab) && !i.modifiers.shift) {
                if let Some(row) = ui.memory(|m| m.focused()) {
                    ui.memory_mut(|m| m.surrender_focus(row));
                }
            } else {
                button.request_focus();
            }
        }
        s.row_focused = false;
        s.open = false;
        s.drawn = None;
        s.scroll = Scroll::default();
    }
    ui.data_mut(|d| d.insert_temp(id, s));
    (button, accepted.and_then(|i| items.get(i).cloned()))
}

/// The byte char `at` of `text` starts at (its end for `at` at the end); None past it.
pub(crate) fn byte_at(text: &str, at: usize) -> Option<usize> {
    if at == 0 { Some(0) } else { text.char_indices().nth(at - 1).map(|(b, c)| b + c.len_utf8()) }
}

/// The character at char `at` of `text`.
pub(crate) fn char_at(text: &str, at: usize) -> Option<char> {
    text.get(byte_at(text, at)?..)?.chars().next()
}

/// The cursor and the character before it, and the text's length (bytes), as they were before
/// the box took this frame's keys: what `pairs` tells the box's edit from.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Before {
    pub(crate) at: usize,
    pub(crate) prev: Option<char>,
    pub(crate) len: usize,
}

/// `Before` the box takes the keys, with it focused and no text selected.
pub(crate) fn before(ui: &Ui, box_id: Id, text: &str) -> Option<Before> {
    if !ui.memory(|m| m.has_focus(box_id)) {
        return None;
    }
    let at = crate::widgets::cursor_range(ui.ctx(), box_id).filter(|r| r.is_empty())?.primary.index.0;
    Some(Before { at, prev: at.checked_sub(1).and_then(|i| char_at(text, i)), len: text.len() })
}

/// What this frame's input did to the box, as a box's pairing rule needs it: one of the
/// characters it pairs typed, Backspace, or anything else (an undo or redo, a paste, a cut),
/// which it leaves as it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Keyed {
    Pair,
    Backspace,
    Other,
}

/// After the box: its edit followed through as `rule` says (the text as the box left it, what
/// was there `before`, the cursor now, and `Keyed`; None to leave the edit as it is), the keys
/// that make those edits being one of `paired` typed alone, or Backspace. Read from what the box
/// did rather than from the keys, so it holds whatever keyboard types the character (Shift,
/// AltGr, Option). The cursor stays where the box put it, and the box's undo takes the closer
/// back with what was typed with it. Every box whose brackets and quotes close themselves goes
/// through here, so they all tell a key typed from a paste or an undo alike.
pub(crate) fn pairs(
    ui: &Ui,
    before: Option<Before>,
    out: &TextEditOutput,
    text: &mut String,
    paired: &[char],
    rule: impl FnOnce(&str, Before, usize, Keyed) -> Option<String>,
) {
    let (Some(before), Some(now)) = (before, out.cursor_range.filter(|r| r.is_empty()).map(|r| r.primary.index.0)) else { return };
    if !out.response.response.changed() {
        return;
    }
    // One edit, made by one of those keys: the character typed alone (Shift, AltGr or Option as
    // it takes), or Backspace alone. Typed as text, or committed by an input method: with one
    // running (ibus, fcitx) on Wayland, every key comes that way, a `(` included, between empty
    // compositions that edit nothing. A composition under way (a non-empty `Preedit`) is no key
    // typed: what it holds isn't written yet.
    let keyed = ui.input(|i| {
        use egui::ImeEvent;
        let edits: Vec<&egui::Event> = i
            .events
            .iter()
            .filter(|e| match e {
                egui::Event::Ime(ImeEvent::Preedit { text, .. } | ImeEvent::Commit(text)) => !text.is_empty(),
                egui::Event::Ime(ImeEvent::DeleteSurrounding { .. }) => true,
                egui::Event::Text(_) | egui::Event::Paste(_) | egui::Event::Cut => true,
                // Undo and redo, not every key with Ctrl: Windows reports AltGr, which types a
                // bracket on many keyboards, as Ctrl+Alt.
                egui::Event::Key { key, pressed: true, modifiers, .. } => {
                    *key == Key::Backspace || (modifiers.command && matches!(key, Key::Z | Key::Y))
                }
                _ => false,
            })
            .collect();
        match edits.as_slice() {
            [egui::Event::Text(t) | egui::Event::Ime(ImeEvent::Commit(t))]
                if t.chars().count() == 1 && t.chars().all(|c| paired.contains(&c)) =>
            {
                Keyed::Pair
            }
            [egui::Event::Key { key: Key::Backspace, modifiers, .. }] if modifiers.is_none() => Keyed::Backspace,
            _ => Keyed::Other,
        }
    });
    if let Some(new) = rule(text.as_str(), before, now, keyed) {
        *text = new;
        // The box drew the text as it left it: show the closer from the next frame on, now.
        ui.ctx().request_repaint();
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

    /// A list shows at most `MAX_ROWS` rows and scrolls beyond: a button's list of 30 items is ten
    /// rows high, and the highlight moved past them scrolls into view. (A row scrolled out keeps
    /// no place of its own to look at: its rect is clipped to the list's.)
    #[test]
    fn a_list_shows_at_most_ten_rows() {
        use egui_kittest::kittest::Queryable;
        let items: Vec<String> = (0..30).map(|i| format!("item{i:02}")).collect();
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, 900.0)).build_ui_state(
            move |ui, id: &mut Option<egui::Id>| {
                let (button, _) = super::menu(ui, "Open", || items.clone(), |_| None, |_| None, false, |_, _, _| {});
                *id = Some(button.id.with("menu"));
            },
            None,
        );
        h.run();
        h.get_by_label("Open").click();
        h.run();
        let id = h.state().unwrap();
        let drawn = |h: &egui_kittest::Harness<'_, Option<egui::Id>>| {
            h.ctx.data(|d| d.get_temp::<super::MenuState>(id)).and_then(|s| s.drawn).map(|(r, _)| r).unwrap()
        };
        let top = |h: &egui_kittest::Harness<'_, Option<egui::Id>>, item: &str| h.get_by_label(item).rect().top();
        let row = top(&h, "item01") - top(&h, "item00");
        assert!(row > 10.0, "one row under another: {row}");
        let height = drawn(&h).height();
        assert!(height > row * 9.0 && height < row * 10.0 + super::FRAME * 2.0, "ten rows: {height} for rows of {row}");
        for _ in 0..15 {
            h.key_press(egui::Key::ArrowDown);
            h.run();
        }
        let list = drawn(&h);
        assert!(list.contains(egui::pos2(list.center().x, top(&h, "item15") + row / 2.0)), "scrolled to the highlight");
        let scrolled = h.ctx.data(|d| d.get_temp::<super::MenuState>(id)).and_then(|s| s.scroll.to);
        assert_eq!(scrolled, Some(15), "scrolled to the highlight once");
    }

    /// A button's list at the right edge of a window `size` big, `top` down it, the help of each
    /// item `helps` gives drawn beside it as a box's is, when the list `has_help`: where the popup
    /// is drawn as ↓ moves the highlight from the first item to the last, and where the button is.
    fn popups_by_the_right_edge(size: egui::Vec2, top: f32, has_help: bool, helps: Vec<Option<String>>) -> (Vec<egui::Rect>, egui::Rect) {
        use egui_kittest::kittest::Queryable;
        let items: Vec<String> = (0..helps.len()).map(|i| format!("item{i:02}")).collect();
        let n = items.len();
        let mut h = egui_kittest::Harness::builder().with_size(size).build_ui_state(
            move |ui, id: &mut Option<egui::Id>| {
                ui.add_space(top);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    let help = |ui: &mut egui::Ui, item: &str, width: f32| {
                        let Some(text) = items.iter().position(|i| i == item).and_then(|i| helps[i].as_ref()) else { return };
                        ui.vertical(|ui| {
                            ui.set_max_width(width);
                            ui.label(text);
                        });
                    };
                    let (button, _) = super::menu(ui, "Open", || items.clone(), |_| None, |_| None, has_help, help);
                    *id = Some(button.id.with("menu"));
                });
            },
            None,
        );
        h.run();
        h.get_by_label("Open").click();
        h.run();
        let id = h.state().unwrap();
        let drawn = |h: &egui_kittest::Harness<'_, Option<egui::Id>>| {
            h.ctx.data(|d| d.get_temp::<super::MenuState>(id)).and_then(|s| s.drawn).map(|(r, _)| r).unwrap()
        };
        let mut out = vec![drawn(&h)];
        for _ in 1..n {
            h.key_press(egui::Key::ArrowDown);
            h.run();
            out.push(drawn(&h));
        }
        (out, h.get_by_label("Open").rect())
    }

    /// By the window's right edge, a list whose items have helps of other widths and heights, or
    /// none (the first one too), keeps one place and one size as the highlight moves: the help's
    /// box is the same for every item, so the popup doesn't move sideways at the edge, up or down,
    /// and no row moves out from under the pointer (#2618). In a wide window (the help beside the
    /// list) and in one too narrow for that (the help under it).
    #[test]
    fn a_list_by_the_right_edge_stays_put_as_its_help_changes() {
        let long = "a long help that wraps over several lines ".repeat(40);
        for width in [1000.0, 380.0] {
            let helps = vec![None, Some("short".into()), Some(long.clone()), Some("short".into()), None];
            let (drawn, _) = popups_by_the_right_edge(egui::vec2(width, 700.0), 0.0, true, helps);
            for d in &drawn {
                assert!(
                    (d.min - drawn[0].min).length() < 0.5 && (d.size() - drawn[0].size()).length() < 0.5,
                    "{width}: one place and size: {drawn:?}"
                );
            }
            assert!(drawn[0].right() <= width + 0.5, "{width}: on screen: {drawn:?}");
        }
    }

    /// A help longer than its box scrolls inside it (the wheel over it), and the next item's help
    /// starts from the top of the box again.
    #[test]
    fn a_long_help_scrolls_in_its_box_and_the_next_starts_at_its_top() {
        // With the help beside the list, and under it.
        for width in [1000.0, 380.0] {
            long_help_scrolls_in_its_box(width);
        }
    }

    fn long_help_scrolls_in_its_box(width: f32) {
        use egui_kittest::kittest::Queryable;
        let items: Vec<String> = vec!["item00".into(), "item01".into()];
        let next = format!("next help {}", "another long help over several lines ".repeat(40));
        let shown = next.clone();
        // Frames a 60th of a second apart, as a fling's speed is measured over the last moments.
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(width, 700.0))
            .with_step_dt(1.0 / 60.0)
            .with_max_steps(30)
            .build_ui_state(
                move |ui, _: &mut ()| {
                    // The first help: a line, then a long blank stretch, as between a help's
                    // blocks (a label takes a drag to select its text: a drag on the blank scrolls).
                    let help = |ui: &mut egui::Ui, item: &str, room: f32| {
                        ui.set_max_width(room);
                        if item == "item00" {
                            ui.label("first help");
                            ui.add_space(2000.0);
                        } else {
                            ui.label(&shown);
                        }
                    };
                    let _ = super::menu(ui, "Open", || items.clone(), |_| None, |_| None, true, help);
                },
                (),
            );
        let wheel = |h: &mut egui_kittest::Harness<'_, ()>| {
            h.event(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -40.0),
                modifiers: egui::Modifiers::NONE,
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
            });
            h.run();
        };
        // A fling on a touchscreen (egui drags a scroll only there): a drag on the blank let go
        // while moving; the help coasts on.
        let fling = |h: &mut egui_kittest::Harness<'_, ()>, at: egui::Pos2| {
            let touch =
                |phase, pos| egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(1), phase, pos, force: None };
            h.event(touch(egui::TouchPhase::Start, at));
            h.drag_at(at);
            h.step();
            for step in 1..=5 {
                h.hover_at(at - egui::vec2(0.0, 15.0 * step as f32));
                h.step();
            }
            let end = at - egui::vec2(0.0, 75.0);
            h.drop_at(end);
            h.event(touch(egui::TouchPhase::End, end));
            h.step();
            let before = h.get_by_label("first help").rect().top();
            h.step();
            assert!(h.get_by_label("first help").rect().top() < before - 1.0, "{width}: coasting");
        };
        // The help starts from the top and stays there, frame after frame.
        let still = |h: &mut egui_kittest::Harness<'_, ()>, label: &str, top: f32, what: &str| {
            for _ in 0..5 {
                h.step();
                let at = h.get_by_label(label).rect().top();
                assert!((at - top).abs() < 0.5, "{width}: {what}: {at} for {top}");
            }
        };
        h.run();
        h.get_by_label("Open").click();
        h.run();
        let first = h.get_by_label("first help").rect();
        let top = first.top();
        let blank = first.left_bottom() + egui::vec2(40.0, 60.0);
        h.hover_at(blank);
        wheel(&mut h);
        assert!(h.get_by_label("first help").rect().top() < top - 20.0, "{width}: scrolled in its box");
        // Another item while it coasts: with the first help's scroll kept (its offset, or the
        // fling's velocity), the next would start scrolled, or scroll on by itself.
        fling(&mut h, blank);
        h.key_press(egui::Key::ArrowDown);
        still(&mut h, &next, top, "the next help");
        // Back to the first item: its help from the top and still, not its fling coming back
        // with it.
        h.key_press(egui::Key::ArrowUp);
        still(&mut h, "first help", top, "back to the first help");
        // Closed while it coasts and opened again: the list's own state is new, egui's kept.
        fling(&mut h, blank);
        h.key_press(egui::Key::Escape);
        h.step();
        h.get_by_label("Open").click();
        still(&mut h, "first help", top, "opened again");
    }

    /// The pointer gone from a row to the help (to read or scroll it) keeps that row's help, not
    /// the highlighted item's.
    #[test]
    fn the_pointer_on_the_help_keeps_the_help_of_the_row_it_left() {
        use egui_kittest::kittest::Queryable;
        let items: Vec<String> = vec!["item00".into(), "item01".into()];
        let helps = ["help zero".to_string(), "help one".to_string()];
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, 700.0)).build_ui_state(
            move |ui, _: &mut ()| {
                let help = |ui: &mut egui::Ui, item: &str, _: f32| {
                    if let Some(i) = items.iter().position(|i| i == item) {
                        ui.label(&helps[i]);
                    }
                };
                let _ = super::menu(ui, "Open", || items.clone(), |_| None, |_| None, true, help);
            },
            (),
        );
        h.run();
        h.get_by_label("Open").click();
        h.run();
        h.hover_at(h.get_by_label("item01").rect().center());
        h.run();
        let one = h.get_by_label("help one").rect();
        h.hover_at(one.center());
        h.run();
        assert!(h.query_by_label("help one").is_some() && h.query_by_label("help zero").is_none(), "the row's help stays");
    }

    /// The help's room beside a list is what the list's own width leaves: a list of long names
    /// puts it under the list where a short one leaves it room beside.
    #[test]
    fn the_help_s_room_is_what_the_list_leaves() {
        let (layout, width) = super::help_layout(1000.0, super::MIN_WIDTH);
        assert!(layout.main_dir() == egui::Direction::LeftToRight && width == super::HELP_WIDTH, "beside: {width}");
        let (layout, width) = super::help_layout(500.0, 300.0);
        assert!(layout.main_dir() == egui::Direction::TopDown && width <= 500.0 - super::FRAME, "under: {width}");
    }

    /// A list without help keeps no room for one: it stays as narrow as its rows.
    #[test]
    fn a_list_without_help_stays_narrow() {
        let (drawn, _) = popups_by_the_right_edge(egui::vec2(1000.0, 700.0), 0.0, false, vec![None, None]);
        let width = drawn[0].width();
        assert!(width < super::MIN_WIDTH + super::GAP + super::HELP_MIN_WIDTH, "no room for a help: {width}");
    }

    /// The case of a popup crossing to the other side as its help changed (#2618, a help too tall
    /// for the room on either side, then shorter ones, by a button with a little more room above
    /// it than under it): the popup's size is the list's, not the help's, so it stays on one side
    /// and in one place.
    #[test]
    fn a_popup_keeps_its_side_while_its_list_is_open() {
        let long = "a long help that wraps over several lines ".repeat(40);
        let helps = vec![Some(long), Some("short".into()), Some("short".into())];
        let (drawn, button) = popups_by_the_right_edge(egui::vec2(1000.0, 400.0), 184.0, true, helps);
        let (above, below) = (button.top(), 400.0 - button.bottom());
        assert!(above > below && above - below < 10.0, "a little more room above: {above} {below}");
        for d in &drawn {
            assert!((d.min - drawn[0].min).length() < 0.5 && (d.size() - drawn[0].size()).length() < 0.5, "one place and size: {drawn:?}");
        }
    }

    /// A screen reader focuses a row and then, in a later frame, activates it: the list stays open
    /// on the focus (still on a widget of its own) and takes the row activated.
    #[test]
    fn a_row_focused_by_a_screen_reader_can_be_activated() {
        use egui_kittest::kittest::Queryable;
        let items: Vec<String> = (0..3).map(|i| format!("item{i:02}")).collect();
        let mut h = egui_kittest::Harness::new_ui_state(
            move |ui, got: &mut Option<String>| {
                if let (_, Some(item)) = super::menu(ui, "Open", || items.clone(), |_| None, |_| None, false, |_, _, _| {}) {
                    *got = Some(item);
                }
            },
            None,
        );
        h.run();
        h.get_by_label("Open").click();
        h.run();
        h.get_by_label("item02").focus();
        h.run();
        assert!(h.query_by_label("item02").is_some_and(|n| n.is_focused()), "the row has the focus");
        h.get_by_label("item02").click();
        h.run();
        assert_eq!(h.state().as_deref(), Some("item02"), "the focused row was taken");
        assert!(h.query_by_label("item00").is_none(), "and the list closed");
    }

    /// Enter on a row a screen reader focused takes that row, not the highlighted one.
    #[test]
    fn enter_on_a_focused_row_takes_that_row() {
        use egui_kittest::kittest::Queryable;
        let items: Vec<String> = (0..3).map(|i| format!("item{i:02}")).collect();
        let mut h = egui_kittest::Harness::new_ui_state(
            move |ui, got: &mut Option<String>| {
                if let (_, Some(item)) = super::menu(ui, "Open", || items.clone(), |_| None, |_| None, false, |_, _, _| {}) {
                    *got = Some(item);
                }
            },
            None,
        );
        h.run();
        h.get_by_label("Open").click();
        h.run();
        h.get_by_label("item02").focus();
        h.run();
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().as_deref(), Some("item02"), "the focused row, not the highlighted item00");
        assert!(h.query_by_label("item00").is_none(), "and the list closed");
    }

    /// A row a screen reader focused takes the highlight, and ↑/↓ move the focus along with it
    /// (round from the last row to the first, not to a widget under the list), so Enter takes the
    /// row both are on; the focus goes back to the button as the list closes.
    #[test]
    fn the_arrows_move_a_focused_row_s_focus_along() {
        use egui_kittest::kittest::{NodeT, Queryable};
        let items: Vec<String> = (0..4).map(|i| format!("item{i:02}")).collect();
        let mut h = egui_kittest::Harness::new_ui_state(
            move |ui, got: &mut Option<String>| {
                if let (_, Some(item)) = super::menu(ui, "Open", || items.clone(), |_| None, |_| None, false, |_, _, _| {}) {
                    *got = Some(item);
                }
                let _ = ui.button("Under");
            },
            None,
        );
        h.run();
        h.get_by_label("Open").click();
        h.run();
        h.get_by_label("item01").focus();
        h.run();
        let highlighted = h.get_by_label("item01").accesskit_node().toggled() == Some(egui::accesskit::Toggled::True);
        assert!(highlighted, "the focused row took the highlight");
        let focused = |h: &egui_kittest::Harness<'_, Option<String>>, label: &str| h.query_by_label(label).is_some_and(|n| n.is_focused());
        h.key_press(egui::Key::ArrowDown);
        h.run();
        assert!(focused(&h, "item02"), "from the focused row, not from the highlighted item00");
        h.key_press(egui::Key::ArrowDown);
        h.run();
        h.key_press(egui::Key::ArrowDown);
        h.run();
        assert!(focused(&h, "item00"), "round to the first row");
        h.key_press(egui::Key::ArrowUp);
        h.run();
        assert!(focused(&h, "item03"), "and back to the last");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().as_deref(), Some("item03"), "the row focused and highlighted");
        assert!(focused(&h, "Open"), "the focus back on the button");
    }

    /// Keys in the very next passes after an arrow moved the focus (before the row keeps its keys
    /// from egui): a second arrow moves it one row, not two, and Escape gives it back to the
    /// button.
    #[test]
    fn keys_right_after_an_arrow_act_once() {
        use egui_kittest::kittest::Queryable;
        let items: Vec<String> = (0..6).map(|i| format!("item{i:02}")).collect();
        let mut h = egui_kittest::Harness::new_ui(move |ui| {
            let _ = super::menu(ui, "Open", || items.clone(), |_| None, |_| None, false, |_, _, _| {});
        });
        let focused = |h: &egui_kittest::Harness<'_>, label: &str| h.query_by_label(label).is_some_and(|n| n.is_focused());
        h.run();
        h.get_by_label("Open").click();
        h.run();
        h.get_by_label("item01").focus();
        h.run();
        // Presses alone, one pass each (`key_press` adds a pass for the release between them).
        h.key_down(egui::Key::ArrowDown);
        h.step();
        h.key_down(egui::Key::ArrowDown);
        h.step();
        h.key_up(egui::Key::ArrowDown);
        h.run();
        assert!(focused(&h, "item03"), "one row for each arrow");
        h.key_down(egui::Key::ArrowDown);
        h.step();
        h.key_down(egui::Key::Escape);
        h.step();
        h.key_up(egui::Key::ArrowDown);
        h.key_up(egui::Key::Escape);
        h.run();
        assert!(h.query_by_label("item00").is_none(), "the list closed");
        assert!(focused(&h, "Open"), "the focus back on the button");
    }

    /// A menu hidden (its dialog closed) with a row focused doesn't take the focus to its button
    /// when it shows again.
    #[test]
    fn a_menu_shown_again_leaves_the_focus_alone() {
        use egui_kittest::kittest::Queryable;
        let items: Vec<String> = (0..4).map(|i| format!("item{i:02}")).collect();
        let mut h = egui_kittest::Harness::new_ui_state(
            move |ui, shown: &mut bool| {
                if *shown {
                    let _ = super::menu(ui, "Open", || items.clone(), |_| None, |_| None, false, |_, _, _| {});
                }
            },
            true,
        );
        h.run();
        h.get_by_label("Open").click();
        h.run();
        h.get_by_label("item01").focus();
        h.run();
        *h.state_mut() = false;
        h.run();
        *h.state_mut() = true;
        h.run();
        assert!(h.query_by_label("Open").is_some_and(|n| !n.is_focused()), "the button didn't take the focus");
    }

    /// From a row a screen reader focused, Tab closes the list and goes on to the widget after its
    /// button, and Shift+Tab back to the button, as from the button itself.
    #[test]
    fn tab_from_a_focused_row_goes_on_past_the_list() {
        use egui_kittest::kittest::Queryable;
        for (shift, to) in [(false, "Under"), (true, "Open")] {
            let items: Vec<String> = (0..4).map(|i| format!("item{i:02}")).collect();
            let mut h = egui_kittest::Harness::new_ui(move |ui| {
                let _ = ui.button("Before");
                let _ = super::menu(ui, "Open", || items.clone(), |_| None, |_| None, false, |_, _, _| {});
                let _ = ui.button("Under");
            });
            h.run();
            h.get_by_label("Open").click();
            h.run();
            h.get_by_label("item01").focus();
            h.run();
            let modifiers = if shift { egui::Modifiers::SHIFT } else { egui::Modifiers::NONE };
            h.key_press_modifiers(modifiers, egui::Key::Tab);
            h.run();
            assert!(h.query_by_label("item00").is_none(), "the list closed");
            assert!(h.query_by_label(to).is_some_and(|n| n.is_focused()), "shift {shift}: the focus on {to}");
        }
    }

    /// Without a screen reader only the rows in view are drawn (each asks for its icon), the
    /// highlighted one scrolled into view; with one, every row is, for it to reach.
    #[test]
    fn a_long_list_draws_only_the_rows_in_view() {
        let items: Vec<String> = (0..600).map(|i| format!("item{i:03}")).collect();
        let drawn = std::cell::RefCell::new(Vec::<String>::new());
        let mut scroll = super::Scroll::default();
        let ctx = egui::Context::default();
        // At once: the scroll to the highlight would take more frames than these.
        ctx.all_styles_mut(|s| s.scroll_animation = egui::style::ScrollAnimation::none());
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0));
        let mut frames = |selected: usize, n: usize| {
            for _ in 0..n {
                drawn.borrow_mut().clear();
                let input = egui::RawInput { screen_rect: Some(screen), ..Default::default() };
                // Its textures aren't painted: dropped unapplied, egui's debug check fails.
                ctx.run_ui(input, |ui| {
                    let icon = |item: &str| {
                        drawn.borrow_mut().push(item.to_string());
                        None
                    };
                    super::rows(ui, &items, selected, false, &mut scroll, false, false, icon, |_| None, |_, _, _| {});
                })
                .textures_delta
                .clear();
            }
            drawn.borrow().clone()
        };
        let rows = frames(0, 3);
        assert!(rows.len() > super::MAX_ROWS && rows.len() < 2 * super::MAX_ROWS, "{} drawn", rows.len());
        let rows = frames(300, 3);
        assert!(rows.len() < 2 * super::MAX_ROWS && rows.contains(&"item300".to_string()), "scrolled to item300: {rows:?}");
        ctx.enable_accesskit();
        assert_eq!(frames(300, 2).len(), items.len(), "every row with a screen reader");
    }

    /// The widest row worked out is as wide as the row drawn: the list keeps its width as it
    /// scrolls.
    #[test]
    fn the_widest_row_is_as_wide_as_drawn() {
        let long = "x".repeat(60);
        egui::Context::default()
            .run_ui(egui::RawInput::default(), |ui| {
                for icon in [None, Some("field")] {
                    let drawn = ui.scope(|ui| super::item_row(ui, &long, false, icon)).response.rect.width();
                    let w = super::widest(ui, std::slice::from_ref(&long), |_| icon);
                    assert!(w > super::MIN_WIDTH && (drawn - w).abs() < 0.5, "{icon:?}: {drawn} drawn, {w} worked out");
                }
            })
            .textures_delta
            .clear();
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

    /// Page Down scrolls a help longer than its box by a page, kept in the help, and Page Up
    /// back (#2619): the keyboard's way to read it, in a box's list and in a button's.
    #[test]
    fn page_keys_scroll_the_help() {
        use egui_kittest::kittest::Queryable;
        let long = format!("first line {}", "a long help that wraps over several lines ".repeat(60));
        let help = |long: String| {
            move |ui: &mut egui::Ui, _: &str, width: f32| {
                ui.set_max_width(width);
                ui.label(&long);
                ui.label("the end");
            }
        };
        fn check<S>(h: &mut egui_kittest::Harness<'_, S>, long: &str, what: &str) {
            let top = h.get_by_label(long).rect().top();
            // The popup, held to its room: the part of the help that shows ends at its bottom.
            let row = h.get_by_label("one").rect().center();
            let popup = |h: &egui_kittest::Harness<'_, S>| {
                h.ctx.memory(|m| {
                    m.layer_ids().filter(|l| l.order == egui::Order::Foreground).filter_map(|l| m.area_rect(l.id)).find(|r| r.contains(row))
                })
            };
            // Its scroll's view ends the frame's margin above the popup's bottom.
            let margin = h.ctx.global_style().spacing.menu_margin.bottom as f32;
            let shown = popup(h).map_or(0.0, |p| p.bottom() - margin - top);
            h.key_press(egui::Key::PageDown);
            h.run();
            let down = h.get_by_label(long).rect().top();
            assert!(down < top - 50.0, "{what}: a page down: {down} from {top}");
            assert!(top - down < shown, "{what}: no line passed unseen: a page of {} with {shown} shown", top - down);
            h.key_press(egui::Key::PageDown);
            h.run();
            let further = h.get_by_label(long).rect().top();
            assert!(further < down - 50.0, "{what}: a page further: {further} from {down}");
            for _ in 0..30 {
                h.key_press(egui::Key::PageDown);
                h.run();
            }
            // At the help's end: its last line inside the popup.
            let popup = popup(h);
            let end = h.get_by_label("the end").rect();
            assert!(
                popup.is_some_and(|p| p.top() <= end.top() && end.bottom() <= p.bottom()),
                "{what}: the end shown: {end:?} in {popup:?}"
            );
            let last = h.get_by_label(long).rect();
            h.key_press(egui::Key::PageDown);
            h.run();
            assert_eq!(h.get_by_label(long).rect(), last, "{what}: no further than the help's end");
            for _ in 0..31 {
                h.key_press(egui::Key::PageUp);
                h.run();
            }
            let back = h.get_by_label(long).rect().top();
            assert!((back - top).abs() < 0.5, "{what}: back to the top: {back} for {top}");
        }
        // A box's list, in a tall window and in one too short for the help's box.
        for height in [700.0, 200.0] {
            let shown = help(long.clone());
            let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, height)).build_ui_state(
                move |ui, (text, list): &mut (String, super::List<Words>)| {
                    let id = egui::Id::unique("box");
                    let _ = list.keys(ui, id);
                    let label = ui.label("Expression");
                    let out = egui::TextEdit::singleline(text).id(id).show(ui);
                    let _ = out.response.response.clone().labelled_by(label.id);
                    out.response.response.request_focus();
                    let visible = ui.clip_rect();
                    let offer = Words(vec!["one".into(), "two".into()]);
                    list.show(
                        ui,
                        egui::Id::unique("list"),
                        &out,
                        visible,
                        |_| Some(offer),
                        |_, _| None,
                        |_| 0,
                        |_, _| None,
                        |ui, _, item, width| shown(ui, item, width),
                    );
                },
                (String::new(), super::List::default()),
            );
            h.run_steps(3);
            check(&mut h, &long, &format!("a box's list, {height} high"));
        }
        // A button's list.
        let shown = help(long.clone());
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, 700.0)).build_ui(move |ui| {
            let _ = super::menu(ui, "Open", || vec!["one".into(), "two".into()], |_| None, |_| None, true, &shown);
        });
        h.run();
        h.get_by_label("Open").click();
        h.run();
        check(&mut h, &long, "a button's list");
    }

    /// The help's view in its box: all of it shown, its bottom cut off by a short popup, or its
    /// top scrolled out of a popup scrolled down; a page never more than what shows.
    #[test]
    fn the_help_pages_through_what_shows() {
        let boxed = egui::Rect::from_min_size(egui::pos2(0.0, 100.0), egui::vec2(300.0, 300.0));
        let at = |top: f32, bottom: f32| egui::Rect::from_x_y_ranges(0.0..=600.0, top..=bottom);
        // All shown.
        assert_eq!(super::help_view(boxed, at(0.0, 600.0), 20.0), (300.0, 280.0));
        // The bottom cut off: the area ends where the popup does, a page is what shows.
        assert_eq!(super::help_view(boxed, at(0.0, 250.0), 20.0), (150.0, 130.0));
        // The top scrolled out (the popup scrolled down): the area keeps the box's top, a page is
        // only the 100 that show.
        assert_eq!(super::help_view(boxed, at(250.0, 350.0), 20.0), (250.0, 80.0));
        // Under the list with hardly any of it shown: a row, not nothing.
        assert_eq!(super::help_view(boxed, at(0.0, 105.0), 20.0), (20.0, 20.0));
    }

    /// What the rows say to a screen reader is worked out once and shared, not copied, frame
    /// after frame (a box's state is cloned on each load), and goes when the list closes.
    #[test]
    fn a_list_s_descriptions_are_kept_once_and_go_with_it() {
        use std::sync::Arc;
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, 700.0)).build_ui_state(
            |ui, list: &mut super::List<Words>| {
                let id = egui::Id::unique("box");
                let _ = list.keys(ui, id);
                let label = ui.label("Expression");
                let filter = egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true };
                let mut text = String::new();
                let out = egui::TextEdit::singleline(&mut text).id(id).event_filter(filter).show(ui);
                let _ = out.response.response.clone().labelled_by(label.id);
                if !out.response.response.has_focus() {
                    out.response.response.request_focus();
                }
                let visible = ui.clip_rect();
                let offer = Words(vec!["one".into(), "two".into()]);
                let describe = |_: &Words, item: &str| Some(format!("about {item}"));
                list.show(ui, egui::Id::unique("list"), &out, visible, |_| Some(offer), |_, _| None, |_| 0, describe, |_, _, _, _| {});
            },
            super::List::default(),
        );
        h.run_steps(3);
        let first = Arc::clone(&h.state().scroll.described);
        assert_eq!(first.len(), 2, "each row described");
        h.run_steps(3);
        assert!(Arc::ptr_eq(&first, &h.state().scroll.described), "not worked out nor copied again");
        drop(first);
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        assert!(h.state().open.is_none(), "closed");
        assert!(h.state().scroll.described.is_empty(), "gone with the list");
    }

    /// What the rows said is kept while `said` gives the same key, and read again when it gives
    /// another (what they're read from changed), with the list still open.
    #[test]
    fn a_list_s_descriptions_are_read_again_when_their_key_changes() {
        use std::sync::Arc;
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, 700.0)).build_ui_state(
            |ui, (list, key): &mut (super::List<Words>, u64)| {
                let id = egui::Id::unique("box");
                let _ = list.keys(ui, id);
                let label = ui.label("Expression");
                let filter = egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true };
                let mut text = String::new();
                let out = egui::TextEdit::singleline(&mut text).id(id).event_filter(filter).show(ui);
                let _ = out.response.response.clone().labelled_by(label.id);
                if !out.response.response.has_focus() {
                    out.response.response.request_focus();
                }
                let visible = ui.clip_rect();
                let offer = Words(vec!["one".into(), "two".into()]);
                let k = *key;
                let describe = move |_: &Words, item: &str| Some(format!("{item} {k}"));
                list.show(ui, egui::Id::unique("list"), &out, visible, |_| Some(offer), |_, _| None, |_| k, describe, |_, _, _, _| {});
            },
            (super::List::default(), 1),
        );
        h.run_steps(3);
        let first = Arc::clone(&h.state().0.scroll.described);
        assert_eq!(first.get(&0), Some(&Some("one 1".to_string())));
        h.run_steps(2);
        assert!(Arc::ptr_eq(&first, &h.state().0.scroll.described), "the same key: kept");
        h.state_mut().1 = 2;
        h.run_steps(2);
        assert!(h.state().0.open.is_some(), "still open");
        assert_eq!(h.state().0.scroll.described.get(&0), Some(&Some("one 2".to_string())), "read again");
    }

    /// A page pressed on the frame a list closes (Escape, Enter, a click on a row with another
    /// help) is dropped with it: the next list's help opens at its top.
    #[test]
    fn a_page_pressed_as_a_list_closes_is_dropped() {
        use egui_kittest::kittest::Queryable;
        let long = format!("first line {}", "a long help that wraps over several lines ".repeat(60));
        // A click on "two" changes the help on the frame it closes the list: the page waits then.
        for close in [Some(egui::Key::Escape), Some(egui::Key::Enter), None] {
            let shown = long.clone();
            let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, 700.0)).build_ui_state(
                move |ui, (text, list): &mut (String, super::List<Words>)| {
                    let id = egui::Id::unique("box");
                    if let Some((_, item)) = list.keys(ui, id) {
                        text.push_str(&item);
                    }
                    let label = ui.label("Expression");
                    // As the boxes do: Escape is the list's while one is open, the focus stays.
                    let filter = egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true };
                    let out = egui::TextEdit::singleline(text).id(id).event_filter(filter).show(ui);
                    let _ = out.response.response.clone().labelled_by(label.id);
                    // Asked for once: asked every frame, egui would drop the filter and let
                    // Escape take the focus.
                    if !out.response.response.has_focus() {
                        out.response.response.request_focus();
                    }
                    let visible = ui.clip_rect();
                    // Another list for each text: a new one opens once something's typed.
                    let offer = Words(vec![format!("one{}", text.len()), "two".into()]);
                    let clicked = list.show(
                        ui,
                        egui::Id::unique("list"),
                        &out,
                        visible,
                        |_| Some(offer),
                        |_, _| None,
                        |_| 0,
                        |_, _| None,
                        |ui, _, item, width| {
                            ui.set_max_width(width);
                            if item == "two" {
                                ui.label(format!("second {shown}"));
                            } else {
                                ui.label(&shown);
                            }
                        },
                    );
                    if let Some((_, item)) = clicked {
                        text.push_str(&item);
                    }
                },
                (String::new(), super::List::default()),
            );
            h.run_steps(3);
            let top = h.get_by_label(&long).rect().top();
            // All in one frame (kittest's `key_press` and `click` give each event its own).
            let key = |key| egui::Event::Key { key, pressed: true, modifiers: egui::Modifiers::NONE, repeat: false, physical_key: None };
            let mut events = vec![key(egui::Key::PageDown)];
            match close {
                Some(close) => events.push(key(close)),
                None => {
                    // The pointer on the first row, then on "two" as it clicks: the help follows it.
                    h.hover_at(h.get_by_label("one0").rect().center());
                    h.run();
                    let pos = h.get_by_label("two").rect().center();
                    let button = |pressed| egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    };
                    events.splice(0..0, [egui::Event::PointerMoved(pos), button(true), button(false)]);
                }
            }
            h.input_mut().events.extend(events);
            h.run();
            // Enter and a click opened the next list (more to offer after the item); Escape needs
            // a key typed.
            if close == Some(egui::Key::Escape) {
                h.event(egui::Event::Text("x".into()));
                h.run();
            }
            h.run_steps(4);
            // The first item's help, or the one under the pointer left on "two".
            let second = format!("second {long}");
            let at = h.query_by_label(&long).or_else(|| h.query_by_label(&second)).map(|n| n.rect().top());
            let Some(at) = at else { panic!("{close:?}: no help shown") };
            assert!((at - top).abs() < 0.5, "{close:?}: the next list's help at its top: {at} for {top}");
        }
    }

    /// A button's list opened from the keyboard keeps the focus on the button, its highlighted
    /// row the button's active descendant with what it says: a screen reader follows the arrows
    /// (#2619).
    #[test]
    fn a_screen_reader_reads_a_button_s_highlighted_entry() {
        use egui_kittest::kittest::{NodeT, Queryable};
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1000.0, 700.0)).build_ui(|ui| {
            let describe = |i: usize| Some(format!("about {i}"));
            let _ = super::menu(ui, "Open", || vec!["one".into(), "two".into()], |_| None, describe, true, |_, _, _| {});
        });
        h.run();
        h.get_by_label("Open").focus();
        h.run();
        h.key_press(egui::Key::Enter);
        h.run();
        let read = |h: &egui_kittest::Harness<'_>| {
            let node = h.get_by_label("Open");
            node.accesskit_node().active_descendant().map(|row| (row.label().unwrap_or_default(), row.description().unwrap_or_default()))
        };
        assert_eq!(read(&h), Some(("one".into(), "about 0".into())));
        h.key_press(egui::Key::ArrowDown);
        h.run();
        assert_eq!(read(&h), Some(("two".into(), "about 1".into())), "it follows the highlight");
        h.key_press(egui::Key::Escape);
        h.run();
        assert_eq!(read(&h), None, "closed: no row named");
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
                    list.show(
                        ui,
                        egui::Id::unique("list"),
                        &out,
                        visible,
                        |_| Some(offer),
                        |_, _| None,
                        |_| 0,
                        |_, _| None,
                        |_, _, _, _| {},
                    );
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
        let (corner, _) = super::place(cursor, egui::vec2(200.0, 100.0), screen, 60.0, super::Side::Below);
        assert_eq!(corner, egui::pos2(100.0, 150.0), "under, clear of the line below");
        let low = cursor.translate(egui::vec2(0.0, 400.0));
        let (corner, _) = super::place(low, egui::vec2(200.0, 100.0), screen, 60.0, super::Side::Below);
        assert_eq!(corner.y + 100.0, 470.0, "above, clear of the line over it");
        // Asked to go over the line: there when it fits, under it when it doesn't.
        let (corner, _) = super::place(low, egui::vec2(200.0, 100.0), screen, 60.0, super::Side::Above);
        assert_eq!(corner.y + 100.0, 470.0, "over, clear of the line over it");
        let (corner, _) = super::place(cursor, egui::vec2(200.0, 100.0), screen, 60.0, super::Side::Above);
        assert_eq!(corner, egui::pos2(100.0, 150.0), "no room over it: under");
    }
}
