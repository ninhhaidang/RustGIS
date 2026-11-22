//! What frames drew otherwise than their layers ask (Never silent): a dashed line drawn solid,
//! a fill left off a block diagram's walls, ground with no height taken as 0 under a block.
//!
//! The drawing code has no session to tell, and a frame comes many times a second, so a frame
//! keeps what it approximated on the thread drawing it, one notice per layer and kind (its key),
//! each once however many frames say it again ([`note`]): the first text a kind is noted with
//! stays, with the count it had then. Whoever draws for someone reads them: [`crate::shadows_said`]
//! gives them among its notices, which every export, page and preview the engine or the
//! interface draws goes through; [`crate::render_placed`] among its gaps; and the map view takes
//! what its frames said ([`take_keyed`]) after each frame it shows, saying each kind once a
//! project. A map view's frame drawn on a worker thread hands what it approximated over to the
//! map view's [`Relay`] for the map view to take with its own.

use std::cell::{Cell, RefCell};

/// The most notices a thread (or the relay) keeps unread: a thread whose frames nobody reads (a
/// pass drawn and thrown away) never holds more.
const KEPT: usize = 64;

/// One thing approximated: its kind (the layer and what), and what to say of it.
type Entry = (String, String);

thread_local! {
    /// What frames drawn on this thread approximated, each kind once, since [`take`].
    static SAID: RefCell<Vec<Entry>> = const { RefCell::new(Vec::new()) };
    /// The map layer a frame on this thread is drawing ([`layer`]), by name.
    static LAYER: RefCell<Option<String>> = const { RefCell::new(None) };
    /// How many dashed lines of [`LAYER`] were drawn solid ([`dashes_drawn_solid`]).
    static SOLID_DASHES: Cell<u32> = const { Cell::new(0) };
    /// Whether a selected feature's highlight is being drawn ([`Highlighting`]): its redraws of a
    /// line already drawn aren't counted again.
    static HIGHLIGHTING: Cell<bool> = const { Cell::new(false) };
    /// How many kinds [`SAID`] turned away, full ([`KEPT`]): said when it is taken.
    static TURNED_AWAY: Cell<usize> = const { Cell::new(0) };
}

/// Where a worker drawing frames for the map view hands what they approximated ([`Relay::hand_over`]),
/// for the map view to take with its own ([`take_keyed`]): each kind once, and the number of kinds
/// turned away as it was full. Shared by the map view and its workers; the map view replaces it
/// when it opens another project, so what its old frames hand over goes nowhere.
#[derive(Clone, Default)]
pub struct Relay(std::sync::Arc<std::sync::Mutex<(Vec<Entry>, usize)>>);

impl Relay {
    /// Hands what frames drawn on this thread approximated over (see [`Relay`]): called by the
    /// worker thread that drew them. The kinds turned away carry over as a count.
    pub fn hand_over(&self) {
        let (mine, n) = keep();
        if mine.is_empty() && n == 0 {
            return;
        }
        if let Ok(mut relayed) = self.0.lock() {
            let mut turned = n;
            for e in mine {
                if !add(&mut relayed.0, e) {
                    turned = turned.saturating_add(1);
                }
            }
            relayed.1 = relayed.1.saturating_add(turned);
        }
    }

    /// What was handed over, and forgets it.
    fn take(&self) -> (Vec<Entry>, usize) {
        self.0.lock().map(|mut r| std::mem::take(&mut *r)).unwrap_or_default()
    }
}

/// Adds `entry` to `to`, unless its kind is there already; `false` when `to` holds [`KEPT`] and
/// the kind is turned away.
fn add(to: &mut Vec<Entry>, entry: Entry) -> bool {
    if to.iter().any(|(k, _)| *k == entry.0) {
        return true;
    }
    if to.len() >= KEPT {
        return false;
    }
    to.push(entry);
    true
}

/// What to say of `n` kinds turned away, as one more entry of its own.
fn turned_away(n: usize) -> Entry {
    ("\0more".to_string(), format!("{n} more kind(s) of approximation in the map's frames weren't kept to be said"))
}

/// Keeps `text` under kind `key`, unless this thread keeps that kind already; past [`KEPT`] kinds,
/// it is counted as turned away instead, and the count said when they are taken.
pub(crate) fn note(key: String, text: String) {
    if !SAID.with(|s| add(&mut s.borrow_mut(), (key, text))) {
        turn_away(1);
    }
}

/// Counts `n` more kinds this thread turned away.
fn turn_away(n: usize) {
    TURNED_AWAY.with(|c| c.set(c.get().saturating_add(n)));
}

/// What frames drawn on this thread approximated since the last call, each kind once (and how
/// many kinds were turned away, as one more notice), and forgets it.
pub(crate) fn take() -> Vec<String> {
    let (entries, n) = keep();
    entries.into_iter().chain((n > 0).then(|| turned_away(n))).map(|(_, t)| t).collect()
}

/// What frames drawn on this thread approximated so far, with their kinds, and how many kinds were
/// turned away; forgotten here, for [`restore`] or to say.
pub(crate) fn keep() -> (Vec<Entry>, usize) {
    (SAID.with(|s| std::mem::take(&mut *s.borrow_mut())), TURNED_AWAY.with(|c| c.replace(0)))
}

/// [`take`] with each notice's kind, and what workers handed over to `relays` ([`Relay`]), the
/// kinds turned away by all said in one more notice: for the map view, which says each kind once
/// a project.
pub(crate) fn take_keyed(relays: &[&Relay]) -> Vec<Entry> {
    let (mut out, mut n) = keep();
    for relay in relays {
        let (entries, m) = relay.take();
        for e in entries {
            if !add(&mut out, e) {
                n = n.saturating_add(1);
            }
        }
        n = n.saturating_add(m);
    }
    if n > 0 {
        out.push(turned_away(n));
    }
    out
}

/// Puts what [`keep`] took back in front of what frames said since: what a caller took to read on
/// its own ([`crate::render_placed`]) leaves the rest, and the count turned away, for whoever
/// reads after it.
pub(crate) fn restore((kept, n): (Vec<Entry>, usize)) {
    let (since, m) = keep();
    let mut turned = n.saturating_add(m);
    SAID.with(|s| {
        let mut s = s.borrow_mut();
        for e in kept.into_iter().chain(since) {
            if !add(&mut s, e) {
                turned = turned.saturating_add(1);
            }
        }
    });
    turn_away(turned);
}

/// Starts drawing the map layer `name` (`None`: between layers): what the layer before it
/// approximated (see [`dashes_drawn_solid`]) is noted under its name.
pub(crate) fn layer(name: Option<&str>) {
    let solid = SOLID_DASHES.with(|c| c.replace(0));
    LAYER.with(|l| {
        let mut l = l.borrow_mut();
        if solid > 0
            && let Some(was) = l.as_deref()
        {
            note(format!("{was}\0dashes"), solid_dashes_text(was, solid));
        }
        match name {
            Some(n) if l.as_deref() != Some(n) => *l = Some(n.to_string()),
            Some(_) => {}
            None => *l = None,
        }
    });
}

/// Forgets the layer being drawn and what it approximated so far, unsaid: a frame stopped
/// part-way (an error, a cancel) leaves it for no one ([`LayerScope`]).
fn discard() {
    SOLID_DASHES.with(|c| c.set(0));
    LAYER.with(|l| *l.borrow_mut() = None);
}

/// Held while a frame draws its layers: a frame that returns part-way leaves nothing of the layer
/// it was drawing for the next frame on this thread to say. A frame drawn to the end has said it
/// already ([`layer`] with `None`).
pub(crate) struct LayerScope;

impl Drop for LayerScope {
    fn drop(&mut self) {
        discard();
    }
}

/// A dashed line of the layer being drawn ([`layer`]) is drawn solid: a round of its pattern is
/// under a pixel, or it would cut the line into too many dashes. Called once for a line however
/// many of its strokes are dashed, and not for a selected line's highlight ([`Highlighting`]),
/// which draws the line again: the notice counts lines, not drawing passes.
pub(crate) fn dashes_drawn_solid() {
    if HIGHLIGHTING.with(Cell::get) {
        return;
    }
    SOLID_DASHES.with(|c| c.set(c.get().saturating_add(1)));
}

/// Held while a selected feature's highlight is drawn (its halo and its strokes again over the
/// line already drawn): what those passes draw solid isn't counted again.
pub(crate) struct Highlighting(bool);

impl Highlighting {
    pub(crate) fn start() -> Self {
        Self(HIGHLIGHTING.with(|h| h.replace(true)))
    }
}

impl Drop for Highlighting {
    fn drop(&mut self) {
        let was = self.0;
        HIGHLIGHTING.with(|h| h.set(was));
    }
}

/// What [`dashes_drawn_solid`] says of layer `name`, `n` of whose lines were drawn solid.
fn solid_dashes_text(name: &str, n: u32) -> String {
    let max = crate::MAX_DASHES as u64;
    format!(
        "{name}: {n} dashed line(s) drawn solid, as a round of the dash pattern was under a pixel at the scale drawn, or a line would take more than {},{:03} dashes",
        max / 1000,
        max % 1000
    )
}

/// What a block diagram's walls drew of walls layer `name`: `n` of the polygons along them fill
/// with a gradient or shapeburst, which walls don't draw.
pub(crate) fn walls_left_plain(name: &str, n: usize) {
    note(
        format!("{name}\0walls"),
        format!(
            "{name}: gradient and shapeburst fills aren't drawn on the block diagram's walls, so {n} of its polygons along them show there in their solid fill or the walls' colour"
        ),
    );
}

/// What to say of a block diagram whose `share` (0 to 1) of the ground it read for its base
/// had no height (off the DEM, the DEM's nodata, an elevation tile that failed), taken as 0
/// ([`crate::terrain::Block::without_height_notice`]).
pub(crate) fn ground_without_height_text(share: f64) -> String {
    let pct = share * 100.0;
    let said = if pct < 1.0 { "under 1%".to_string() } else { format!("about {:.0}%", pct.round()) };
    format!(
        "Block diagram: {said} of the ground in it has no height (off the DEM, the DEM's nodata, or elevation tiles that failed), so it is taken as 0 m and the base goes below that"
    )
}

/// Notes [`ground_without_height_text`].
pub(crate) fn ground_without_height(text: String) {
    note("\0block ground".to_string(), text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_is_kept_once_named_by_its_layer_with_its_first_count_and_taken_once() {
        take();
        // Two lines drawn solid in Roads, none in Rivers: one notice, Roads', counting two.
        layer(Some("Roads"));
        dashes_drawn_solid();
        dashes_drawn_solid();
        layer(Some("Rivers"));
        layer(None);
        // Another frame, another count: the same kind, said once with the first.
        layer(Some("Roads"));
        dashes_drawn_solid();
        layer(None);
        let said = take();
        assert_eq!(said, vec![solid_dashes_text("Roads", 2)]);
        assert!(said[0].contains("2 dashed line(s)") && said[0].contains("100,000"), "{said:?}");
        assert!(take().is_empty(), "taken once");
        // Nothing approximated, nothing said.
        layer(Some("Roads"));
        layer(None);
        assert!(take().is_empty());
    }

    #[test]
    fn a_highlight_drawn_over_a_line_is_not_counted_again() {
        take();
        layer(Some("Roads"));
        dashes_drawn_solid();
        {
            let _h = Highlighting::start();
            dashes_drawn_solid();
            dashes_drawn_solid();
        }
        layer(None);
        assert_eq!(take(), vec![solid_dashes_text("Roads", 1)]);
        // Once it is over, lines count again.
        layer(Some("Roads"));
        dashes_drawn_solid();
        layer(None);
        assert!(!HIGHLIGHTING.with(Cell::get));
    }

    #[test]
    fn a_thread_nobody_reads_keeps_a_bounded_number() {
        take();
        for i in 0..KEPT * 2 {
            walls_left_plain(&format!("Layer {i}"), 1);
        }
        // Those turned away are counted, said in one more notice.
        let said = take();
        assert_eq!(said.len(), KEPT + 1);
        assert!(said.last().is_some_and(|s| s.starts_with(&format!("{KEPT} more kind(s)"))), "{said:?}");
        assert!(take().is_empty());
    }

    #[test]
    fn a_frame_stopped_part_way_leaves_nothing_behind() {
        take();
        {
            let _scope = LayerScope;
            layer(Some("Roads"));
            dashes_drawn_solid();
        }
        layer(None);
        assert!(take().is_empty());
    }

    #[test]
    fn restoring_keeps_what_came_before_and_since() {
        take();
        ground_without_height(ground_without_height_text(0.004));
        let kept = keep();
        walls_left_plain("Geology", 3);
        restore(kept.clone());
        let said = take();
        assert_eq!(said.len(), 2);
        assert!(said.first().is_some_and(|s| s.contains("under 1%")), "{said:?}");
    }

    #[test]
    fn restoring_keeps_the_count_turned_away() {
        take();
        for i in 0..KEPT + 5 {
            walls_left_plain(&format!("Layer {i}"), 1);
        }
        let kept = keep();
        walls_left_plain("Later", 1);
        restore(kept);
        let said = take();
        // The 5 turned away before, and the one since that no longer fits.
        assert!(said.len() == KEPT + 1 && said.last().is_some_and(|s| s.starts_with("6 more kind(s)")), "{said:?}");
    }

    #[test]
    fn a_workers_frames_are_handed_over_whole() {
        take();
        let relay = Relay::default();
        let from_a_worker = |work: &(dyn Fn() + Sync)| {
            std::thread::scope(|s| {
                s.spawn(|| {
                    work();
                    relay.hand_over();
                });
            });
        };
        from_a_worker(&|| walls_left_plain("Geology", 3));
        let said = take_keyed(&[&relay]);
        assert!(said.len() == 1 && said[0].0 == "Geology\0walls", "{said:?}");
        assert!(take_keyed(&[&relay]).is_empty(), "taken once");
        // A worker that turned kinds away hands their number over whole.
        from_a_worker(&|| {
            for i in 0..KEPT * 2 {
                walls_left_plain(&format!("Layer {i}"), 1);
            }
        });
        let said = take_keyed(&[&relay]);
        assert!(said.len() == KEPT + 1 && said.last().is_some_and(|(_, t)| t.starts_with(&format!("{KEPT} more kind(s)"))), "{said:?}");
        // What a worker hands to a relay the map view let go of reaches no other.
        let old = Relay::default();
        std::thread::scope(|s| {
            s.spawn(|| {
                walls_left_plain("Geology", 3);
                old.hand_over();
            });
        });
        assert!(take_keyed(&[&relay]).is_empty());
    }
}
