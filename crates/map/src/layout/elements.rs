//! The kinds of element a layout holds, and what each one takes.

use super::*;

/// What an element is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ElementKind {
    MapFrame {
        /// The map shown (by name).
        map: String,
        /// Centre (longitude/latitude) and scale denominator on paper.
        center: Coord,
        scale: f64,
        #[serde(default)]
        shape: FrameShape,
        #[serde(default)]
        grid: Option<Grid>,
        /// Another frame whose extent this one outlines (an extent indicator).
        #[serde(default)]
        extent_of: Option<u32>,
        /// The extent indicator's outline colour and width (points): red and 2 unless set.
        #[serde(default = "indicator_color", skip_serializing_if = "is_indicator_color")]
        extent_color: Rgba,
        #[serde(default = "indicator_width", skip_serializing_if = "is_indicator_width")]
        extent_width: f64,
        /// The outline drawn round the frame, along its shape: its colour and width (points), 0
        /// for none (#2557). Dark grey and 0.8 unless set, and left out then, so layouts saved
        /// before keep their bytes.
        #[serde(default = "frame_border_color", skip_serializing_if = "is_frame_border_color")]
        border_color: Rgba,
        #[serde(default = "frame_border_width", skip_serializing_if = "is_frame_border_width")]
        border_width: f64,
        /// The map turned to a bearing (degrees clockwise from north) and tilted to a pitch
        /// (degrees from looking straight down, 0 to 85), as the map view is.
        #[serde(default, skip_serializing_if = "is_zero")]
        bearing: f64,
        #[serde(default, skip_serializing_if = "is_zero")]
        pitch: f64,
    },
    NorthArrow {
        frame: u32,
        #[serde(default)]
        style: ArrowStyle,
        /// The filled parts.
        #[serde(default = "ink")]
        fill: Rgba,
        /// The outline and the `N`.
        #[serde(default = "ink")]
        outline: Rgba,
    },
    ScaleBar {
        frame: u32,
        #[serde(default)]
        units: ScaleUnits,
        /// Divisions along the bar, 1 to [`MAX_SCALE_DIVISIONS`].
        #[serde(default = "divisions")]
        divisions: usize,
        #[serde(default)]
        style: ScaleBarStyle,
        /// The lines, the alternating bar's dark divisions and the labels.
        #[serde(default = "ink")]
        color: Rgba,
        /// The alternating bar's light divisions.
        #[serde(default = "white")]
        fill: Rgba,
        /// The size of its labels, in points, within [`LAYOUT_TEXT_SIZE`] (#2581): the room over
        /// the bar and between labels follows in proportion, the bar keeping its size.
        /// [`SCALE_BAR_TEXT_PT`] unless set, and left out then, so layouts saved before keep their
        /// bytes.
        #[serde(default = "scale_bar_text_pt", skip_serializing_if = "is_scale_bar_text_pt")]
        text_size: f64,
        /// Parts the first division after zero is split into, 0 (none) to [`MAX_SCALE_SUBDIVISIONS`].
        #[serde(default, skip_serializing_if = "is_default")]
        subdivisions: usize,
        /// Divisions drawn left of zero, 0 to [`MAX_SCALE_BEFORE_ZERO`].
        #[serde(default, skip_serializing_if = "is_default")]
        divisions_before_zero: usize,
        /// Where the numbers sit against the bar.
        #[serde(default, skip_serializing_if = "is_default")]
        label_position: ScaleLabelPosition,
        /// Text printed in place of the units' abbreviation. `None` prints the abbreviation and
        /// `Some("")` prints nothing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit_label: Option<String>,
        /// Where the unit label sits.
        #[serde(default, skip_serializing_if = "is_default")]
        unit_label_position: UnitLabelPosition,
        /// Decimal places in the numbers, 0 to [`MAX_SCALE_DECIMALS`], their thousands set apart;
        /// none (auto), the decimals each label needs, as bars always printed.
        #[serde(default, deserialize_with = "de_scale_decimals", skip_serializing_if = "Option::is_none")]
        decimals: Option<u8>,
        /// What gives way when the bar's length does not fit its box.
        #[serde(default, skip_serializing_if = "is_default")]
        fit: ScaleBarFit,
    },
    Legend {
        frame: u32,
        title: String,
        /// List every class of every visible layer. Off (the default), the legend shows what the
        /// frame draws: classes some drawn feature has, `<all other values>` only when used, and
        /// no layer that draws nothing at the frame's scale (#207).
        #[serde(default)]
        all_classes: bool,
        /// Leave `<all other values>` out of the legend, in either mode. Only the legend changes:
        /// the features it stands for are drawn as before.
        #[serde(default)]
        hide_other: bool,
        /// Include the grey fallback for Graduated Colors' missing numeric values. Absent in
        /// older layouts, whose appearance is preserved; this changes only the legend.
        #[serde(default, skip_serializing_if = "is_false")]
        show_no_data: bool,
        /// Layers (by name, any case) the legend leaves out though the map draws them: a mask, a
        /// highlight, a basemap-like layer. Empty, it lists every layer it would otherwise.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        exclude: Vec<String>,
        /// The columns its rows are laid in, 1 to [`MAX_LEGEND_COLUMNS`], under its title: as even
        /// as they can be, a layer's name staying with the row under it.
        #[serde(default = "one_column", skip_serializing_if = "is_one_column")]
        columns: usize,
        /// The label it shows for a layer instead of the layer's name, by the layer's name (any
        /// case, as the map spells it): its row, or the name over its classes (#1654). The layer
        /// keeps its name everywhere else. A layer not in it shows its name.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        labels: BTreeMap<String, String>,
        /// The colour of its text and lines (#2257); none, the dark grey of [`INK`]. Left out
        /// when none, so layouts saved before keep their bytes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<Rgba>,
        /// The size of its layers' names and its classes' labels, in points, within
        /// [`LAYOUT_TEXT_SIZE`] (#2581): its title, rows, swatches and gaps follow in proportion,
        /// a point symbol keeping the size the map draws it at. [`LEGEND_TEXT_PT`] unless set,
        /// and left out then, so layouts saved before keep their bytes.
        #[serde(default = "legend_text_pt", skip_serializing_if = "is_legend_text_pt")]
        text_size: f64,
    },
    /// The colour scale of a raster layer in a frame's map: a gradient bar for a stretch, steps
    /// for classes, a block per value for unique values and colour tables (#326).
    Colorbar {
        frame: u32,
        /// The raster layer (by name); `None` is the frame map's top shown raster with a colour scale
        /// that isn't drawn by classes ([`colorbar_layer`]).
        #[serde(default)]
        layer: Option<String>,
        /// Drawn beside the bar, e.g. the units: `Elevation (m)`.
        #[serde(default)]
        title: String,
        #[serde(default)]
        orientation: Orientation,
        /// Labelled values along a continuous bar, the ends included (2 labels the minimum and
        /// maximum). Classes and unique values label their own breaks and values.
        #[serde(default = "ticks")]
        ticks: usize,
        #[serde(default)]
        tick_side: TickSide,
        /// Run the other way: low values on the right of a horizontal bar (left by default), at
        /// the top of a vertical one (the bottom by default).
        #[serde(default)]
        reverse: bool,
        /// The colour of its text and lines (#2257); none, the dark grey of [`INK`]. Left out
        /// when none, so layouts saved before keep their bytes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<Rgba>,
    },
    /// Text; dynamic tokens (`{page}`, `{date}`, `{scale}`…) expand when drawn.
    Text {
        text: String,
        size: f64,
        color: Rgba,
        #[serde(default)]
        bold: bool,
        /// Slanted, as water and ocean names are.
        #[serde(default, skip_serializing_if = "is_false")]
        italic: bool,
        /// Where each line sits across the element.
        #[serde(default, skip_serializing_if = "is_left")]
        align: TextAlign,
        /// Space added between letters, in points (spaced-out ocean names); negative tightens.
        #[serde(default, skip_serializing_if = "is_zero")]
        letter_spacing: f64,
        /// Sans-serif or serif; left out when sans, so layouts saved before keep their bytes. With
        /// `font_name`, the family drawn where that one isn't installed.
        #[serde(default, skip_serializing_if = "is_sans")]
        font: FontFamily,
        /// An installed font family, by the name the font gives it ("Garamond", #762): drawn in
        /// it where it is installed, else in `font`. Left out when none; kept as a label's font is
        /// (trimmed, bounded, none when not a name).
        #[serde(default, deserialize_with = "crate::symbology::de_font", skip_serializing_if = "Option::is_none")]
        font_name: Option<String>,
        /// Turned about the centre of its box, in degrees clockwise as a frame's `bearing` is
        /// (90 reads top to bottom, -90 bottom to top), -180 to 180 ([`rotation()`]); the element's
        /// box stays the unturned one. Left out when 0, so layouts saved before keep their bytes.
        #[serde(default, skip_serializing_if = "is_zero")]
        rotation: f64,
        /// Broken into lines at spaces to fit the width of its (unturned) box when drawn, after
        /// its dynamic text is filled in (#2346); off, its lines break only where the text has a
        /// line break. Left out when off, so layouts saved before keep their bytes.
        #[serde(default, skip_serializing_if = "is_false")]
        wrap: bool,
        /// Where its block of lines sits down the (unturned) box. Left out when top, so layouts
        /// saved before keep their bytes.
        #[serde(default, skip_serializing_if = "is_top")]
        valign: TextVAlign,
        /// The space between its lines, as a multiple of the usual ([`MIN_LINE_SPACING`] to
        /// [`MAX_LINE_SPACING`]). Left out when usual, so layouts saved before keep their bytes.
        #[serde(default = "line_spacing", skip_serializing_if = "is_line_spacing", deserialize_with = "de_line_spacing")]
        line_spacing: f64,
        /// Its words in upper or lower case when drawn, after its dynamic text is filled in. Left
        /// out when as typed, so layouts saved before keep their bytes.
        #[serde(default, skip_serializing_if = "is_no_case")]
        case: TextCase,
    },
    Picture {
        /// Base64 PNG or JPEG.
        image: String,
    },
    /// The attribute table of a layer in a frame's map.
    Table {
        frame: u32,
        layer: String,
        #[serde(default = "rows")]
        rows: usize,
        /// The colour of its text and lines (#2257); none, the dark grey of [`INK`]. Left out
        /// when none, so layouts saved before keep their bytes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<Rgba>,
    },
    /// A bar chart of a numeric field of a layer in a frame's map: one bar per feature, or one
    /// per value of a `group` field ([`ElementKind::chart_data`], #2487).
    Chart {
        frame: u32,
        layer: String,
        field: String,
        /// Bars are labelled with this field, if set (not read when grouped: a bar is labelled
        /// with its value of `group`).
        #[serde(default)]
        label: Option<String>,
        /// The colour of its labels and title (#2257); none, the dark grey of [`INK`]. Its bars
        /// keep their own colour. Left out when none, so layouts saved before keep their bytes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<Rgba>,
        /// One bar per value of this field, its height `statistic` of `field` over the features
        /// with that value; none, one bar per feature. The fields below are left out at their
        /// defaults, so layouts saved before keep their bytes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group: Option<String>,
        /// What a grouped bar measures (sum by default); a bar per feature is its own value.
        #[serde(default, skip_serializing_if = "is_default")]
        statistic: StatKind,
        #[serde(default, skip_serializing_if = "is_default")]
        order: ChartOrder,
        /// The most bars drawn, 1 to [`MAX_CHART_BARS`] (read clamped).
        #[serde(default = "chart_bars", skip_serializing_if = "is_chart_bars")]
        bars: usize,
        /// The bars past `bars` gathered in a last "Other" bar (so `bars` - 1 of their own),
        /// rather than left out.
        #[serde(default, skip_serializing_if = "is_false")]
        other: bool,
        /// Bars running across (the default) or columns standing up ([`ChartLook`]). This and the
        /// options below are left out at their defaults, so layouts saved before keep their bytes.
        #[serde(default, skip_serializing_if = "is_default")]
        orientation: Orientation,
        /// Each bar's value written past its end.
        #[serde(default, skip_serializing_if = "is_false")]
        values: bool,
        /// The decimals those values are written with, 0 to [`MAX_CHART_DECIMALS`]; none, up to
        /// two, the zeros they end in dropped.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decimals: Option<u8>,
        /// Its own title, "" for none; none, the title it is given ([`ElementKind::chart_title`]).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        /// Its bars' colour; none, [`CHART_BAR`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bar_color: Option<Rgba>,
        /// Where bars running across have their labels in the labels' column: at its left (the
        /// default), or at its right, against the bars. Columns have theirs centred under them.
        #[serde(default, skip_serializing_if = "is_left")]
        label_align: TextAlign,
        /// Each bar in the colour its features have on the map, by the layer's symbology classes
        /// (#2487 part 3): a bar per feature its feature's, a grouped bar the colour its features
        /// share. A bar whose features have several, and an Other bar, keep `bar_color`.
        #[serde(default, skip_serializing_if = "is_false")]
        classes: bool,
        /// Each grouped bar split by the values of this field, a stretch per value, with a key over
        /// the bars (#2487 part 3b). Read only when the chart groups by another field and sums or
        /// counts, so that its stretches add up to their bar ([`ElementKind::chart_stack`]).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stack: Option<String>,
        /// Drawn as its mirror image, its text reading as ever (#2487 part 4): bars running across
        /// from the right, their labels at the right; columns hanging from the top, their labels
        /// over them. Beside a chart drawn the usual way, the two face each other.
        #[serde(default, skip_serializing_if = "is_false")]
        mirror: bool,
        /// The value at the far end of its bars' room, so that charts given the same one share a
        /// scale; a bar past it runs to that end. None, or one not above 0 (not read), its bars'
        /// own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        /// Its labels left out, their room given to the bars.
        #[serde(default, skip_serializing_if = "is_false")]
        hide_labels: bool,
        /// A line round each bar, or each stretch of a stacked one; none by default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bar_outline: Option<ChartOutline>,
    },
    /// A rectangle drawn on the page (a neatline or a backdrop). `width` is the outline's, in
    /// points (0: none).
    Rectangle {
        fill: Rgba,
        outline: Rgba,
        width: f64,
        /// Its corners rounded by this much, in millimetres (0: square corners).
        #[serde(default, skip_serializing_if = "is_zero")]
        radius: f64,
        /// Dashed: the pattern along the outline, in points, as a line layer's stroke takes it
        /// ([`Dash`]); `None` is solid. Left out when solid, so layouts saved before keep their bytes.
        #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::symbology::de_dash")]
        dash: Option<Dash>,
    },
    /// An ellipse filling the element's box: a circle in a square one.
    Ellipse {
        fill: Rgba,
        outline: Rgba,
        width: f64,
        /// Dashed: the pattern along the outline, in points, as a line layer's stroke takes it
        /// ([`Dash`]); `None` is solid. Left out when solid, so layouts saved before keep their bytes.
        #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::symbology::de_dash")]
        dash: Option<Dash>,
    },
    /// A straight line across the element's box, `width` points wide.
    Line {
        color: Rgba,
        width: f64,
        #[serde(default)]
        dir: LineDir,
        /// Arrowheads: none makes it a plain line.
        #[serde(default)]
        head: LineHead,
        /// Dashed: the pattern along the line, in points, as a line layer's stroke takes it
        /// ([`Dash`]); `None` is solid. Only the shaft is dashed: arrowheads stay solid. Left out
        /// when solid, so layouts saved before keep their bytes.
        #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::symbology::de_dash")]
        dash: Option<Dash>,
    },
}

pub(super) fn rows() -> usize {
    12
}

impl ElementKind {
    /// A text element in the plain style: upright, left-aligned, normally spaced.
    pub fn text(text: impl Into<String>, size: f64, color: Rgba, bold: bool) -> ElementKind {
        ElementKind::Text {
            text: text.into(),
            size,
            color,
            bold,
            italic: false,
            align: TextAlign::Left,
            letter_spacing: 0.0,
            font: FontFamily::Sans,
            font_name: None,
            rotation: 0.0,
            wrap: false,
            valign: TextVAlign::Top,
            line_spacing: LINE_SPACING,
            case: TextCase::None,
        }
    }

    /// A north arrow for `frame`, in the default style.
    pub fn north_arrow(frame: u32) -> ElementKind {
        ElementKind::NorthArrow { frame, style: ArrowStyle::default(), fill: ink(), outline: ink() }
    }

    /// A scale bar for `frame`, in the default style.
    pub fn scale_bar(frame: u32) -> ElementKind {
        ElementKind::ScaleBar {
            frame,
            units: ScaleUnits::default(),
            divisions: divisions(),
            style: ScaleBarStyle::default(),
            color: ink(),
            fill: white(),
            text_size: SCALE_BAR_TEXT_PT,
            subdivisions: 0,
            divisions_before_zero: 0,
            label_position: ScaleLabelPosition::default(),
            unit_label: None,
            unit_label_position: UnitLabelPosition::default(),
            decimals: None,
            fit: ScaleBarFit::default(),
        }
    }

    /// Whether this is a text whose block alignment, line spacing or case is not the default, once
    /// brought into range as [`Layout::clamp`] does: what a layout file needs a newer reader for.
    pub fn text_uses_block(&self) -> bool {
        let ElementKind::Text { valign, line_spacing, case, .. } = self else {
            return false;
        };
        valign.kept() != TextVAlign::Top || case.kept() != TextCase::None || line_spacing_in_range(*line_spacing) != LINE_SPACING
    }

    /// Whether this is a scale bar with any of the details set: subdivisions, divisions before
    /// zero, a label or unit label placement, a unit label, decimals or a fit. Its text size
    /// isn't one: [`ElementKind::has_custom_text_size`] tells that.
    pub fn scale_bar_uses_details(&self) -> bool {
        let ElementKind::ScaleBar {
            frame,
            subdivisions,
            divisions_before_zero,
            label_position,
            unit_label,
            unit_label_position,
            decimals,
            fit,
            ..
        } = self
        else {
            return false;
        };
        let ElementKind::ScaleBar {
            subdivisions: s0,
            divisions_before_zero: z0,
            label_position: lp0,
            unit_label: u0,
            unit_label_position: up0,
            decimals: d0,
            fit: f0,
            ..
        } = ElementKind::scale_bar(*frame)
        else {
            return false;
        };
        // One subdivision is none: it clamps to 0, and an imported text is checked before that.
        // Before clamp a file's invalid decimals read as 255 (a detail): over-requires for an invalid file only.
        let subdivisions = if *subdivisions == 1 { 0 } else { *subdivisions };
        (subdivisions, *divisions_before_zero, *label_position, unit_label, *unit_label_position, *decimals, *fit)
            != (s0, z0, lp0, &u0, up0, d0, f0)
    }

    pub fn code(&self) -> &'static str {
        match self {
            ElementKind::MapFrame { .. } => "mapFrame",
            ElementKind::NorthArrow { .. } => "northArrow",
            ElementKind::ScaleBar { .. } => "scaleBar",
            ElementKind::Legend { .. } => "legend",
            ElementKind::Colorbar { .. } => "colorbar",
            ElementKind::Text { .. } => "text",
            ElementKind::Picture { .. } => "picture",
            ElementKind::Table { .. } => "table",
            ElementKind::Chart { .. } => "chart",
            ElementKind::Rectangle { .. } => "rectangle",
            ElementKind::Ellipse { .. } => "ellipse",
            ElementKind::Line { .. } => "line",
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            ElementKind::MapFrame { .. } => "Map Frame",
            ElementKind::NorthArrow { .. } => "North Arrow",
            ElementKind::ScaleBar { .. } => "Scale Bar",
            ElementKind::Legend { .. } => "Legend",
            ElementKind::Colorbar { .. } => "Colorbar",
            ElementKind::Text { .. } => "Text",
            ElementKind::Picture { .. } => "Picture",
            ElementKind::Table { .. } => "Table Frame",
            ElementKind::Chart { .. } => "Chart Frame",
            ElementKind::Rectangle { .. } => "Rectangle",
            ElementKind::Ellipse { .. } => "Ellipse",
            ElementKind::Line { .. } => "Line",
        }
    }
    /// Whether an element of this kind takes a background (#2257): all but a map frame, which
    /// draws its map and the paper behind it itself.
    pub fn takes_background(&self) -> bool {
        !matches!(self, ElementKind::MapFrame { .. })
    }

    /// Whether an element of this kind takes an ink (#2257): a legend, colorbar, table or chart,
    /// whose text and lines are otherwise dark grey. The other kinds' colours are their own options.
    pub fn takes_ink(&self) -> bool {
        matches!(self, ElementKind::Legend { .. } | ElementKind::Colorbar { .. } | ElementKind::Table { .. } | ElementKind::Chart { .. })
    }

    /// Whether `key` given as null changes an element of this kind, giving it back its default:
    /// an ink's `color`, a chart's `title` (the title it is given), `barColor`, `max` (its bars'
    /// own scale) and `barOutline` (none), a legend's or scale bar's `textSize` (#2581), and a
    /// scale bar's `unitLabel` (its unit's abbreviation).
    pub fn null_resets(&self, key: &str) -> bool {
        (self.takes_ink() && key == "color")
            || (matches!(self, ElementKind::Chart { .. }) && matches!(key, "title" | "barColor" | "max" | "barOutline"))
            || (matches!(self, ElementKind::Legend { .. } | ElementKind::ScaleBar { .. }) && key == "textSize")
            || (matches!(self, ElementKind::ScaleBar { .. }) && key == "unitLabel")
    }

    /// Its ink, when it [takes one](ElementKind::takes_ink) and has one; `None` draws in [`INK`].
    pub fn ink(&self) -> Option<Rgba> {
        match self {
            ElementKind::Legend { color, .. }
            | ElementKind::Colorbar { color, .. }
            | ElementKind::Table { color, .. }
            | ElementKind::Chart { color, .. } => *color,
            _ => None,
        }
    }

    /// Its ink to change, when it [takes one](ElementKind::takes_ink).
    pub fn ink_mut(&mut self) -> Option<&mut Option<Rgba>> {
        match self {
            ElementKind::Legend { color, .. }
            | ElementKind::Colorbar { color, .. }
            | ElementKind::Table { color, .. }
            | ElementKind::Chart { color, .. } => Some(color),
            _ => None,
        }
    }

    /// Whether it is a legend whose text, a scale bar whose labels or a map frame whose grid's
    /// labels aren't their default size (#2581).
    pub fn has_custom_text_size(&self) -> bool {
        match self {
            ElementKind::Legend { text_size, .. } => !is_legend_text_pt(text_size),
            ElementKind::ScaleBar { text_size, .. } => !is_scale_bar_text_pt(text_size),
            ElementKind::MapFrame { grid: Some(g), .. } => !is_grid_label_pt(&g.label_size),
            _ => false,
        }
    }

    /// Whether it is a map frame whose border isn't the default one (#2557).
    pub fn has_custom_border(&self) -> bool {
        matches!(self, ElementKind::MapFrame { border_color, border_width, .. }
            if !is_frame_border_color(border_color) || !is_frame_border_width(border_width))
    }

    /// The map frame this element belongs to.
    pub fn frame(&self) -> Option<u32> {
        match self {
            ElementKind::NorthArrow { frame, .. }
            | ElementKind::ScaleBar { frame, .. }
            | ElementKind::Legend { frame, .. }
            | ElementKind::Colorbar { frame, .. }
            | ElementKind::Table { frame, .. }
            | ElementKind::Chart { frame, .. } => Some(*frame),
            _ => None,
        }
    }

    /// Whether a chart uses what a chart had none of before (grouping, its statistic, its order,
    /// its limit, an "Other" bar): an older reader would draw it as one bar per feature, the 20
    /// largest, and save it again without them; a statistic kept while ungrouped is lost so too.
    pub fn chart_uses_data_options(&self) -> bool {
        matches!(self, ElementKind::Chart { group, statistic, order, bars, other, .. }
            if group.is_some() || *statistic != StatKind::default() || *order != ChartOrder::default() || *bars != CHART_BARS || *other)
    }

    /// The bars a chart draws from `ds`, its layer's features (#2487); `None` for another kind, or
    /// when its field or group field isn't in `ds`. Ungrouped, a bar per feature with a number in
    /// its field; grouped, a bar per value of the group field ([`CHART_BLANK`] for none), its
    /// statistic of the field over that value's features. Ordered, then cut to its limit, the
    /// bars past it left out or gathered in a last [`CHART_OTHER`] bar, the statistic over their
    /// features (their sum, ungrouped), whatever the order.
    ///
    /// It holds a value and an index per feature with a number (16 bytes) ungrouped. Grouped, it
    /// holds a 48-byte tally and an index per value, with the value's name once: at most the
    /// group field's own text again when every feature has a value of its own; a number or a
    /// yes/no value also has its key and index. Labels are read only for the bars drawn, except
    /// sorted by label: then, while it sorts, a label and its lowercase copy are held for every
    /// bar not yet cut to its limit (every feature with a number, ungrouped), about the label
    /// field's own text twice over.
    ///
    /// It reads no colours ([`ChartData::colors`] stays empty): [`ElementKind::chart_data_of`] reads
    /// them from the layer for a chart coloured by classes.
    pub fn chart_data(&self, ds: &rustgis_data::Dataset) -> Option<ChartData> {
        self.chart_data_with(ds, None)
    }

    /// [`ElementKind::chart_data`] of `layer`'s features, a chart coloured by classes reading each
    /// feature's colour as the map frame draws it: in `env`, the frame's scale and the series
    /// page ([`crate::Layer::styler_with`]), by the feature's own geometry, the layer's where it
    /// has none ([`crate::symbology::Symbol::shown_colour`]). A feature its symbol draws nothing
    /// of, in a hidden class, has none. The layer's transparency isn't read: a bar is drawn whole.
    pub fn chart_data_of(&self, layer: &crate::Layer, env: &rustgis_data::query::Env) -> Option<ChartData> {
        if !self.chart_uses_classes() {
            return self.chart_data(&layer.dataset);
        }
        let styler = layer.styler_with(env.clone());
        let ds = &layer.dataset;
        let colour = |row: usize| {
            let g = ds.features.geometry(row).map_or(ds.geometry_type, rustgis_data::Geometry::kind);
            styler.symbol(row).shown_colour(g)
        };
        self.chart_data_with(ds, Some(&colour))
    }

    /// [`ElementKind::chart_data`], each bar coloured by `colour` (a feature row's colour, none for
    /// a feature drawn with nothing) when given: a bar per feature its row's, a grouped bar the
    /// colour all the features its statistic reads share (a count's every feature, another's those
    /// with a number), those drawn with nothing aside. A bar without one keeps the chart's.
    /// Grouped, it holds a colour (5 bytes) per value besides, and reads no more colours of a value
    /// once its features have two; each colour read is the feature's symbol, as drawing the map
    /// reads it.
    ///
    /// Stacked ([`ElementKind::chart_stack`]), the colours go to the values of the stack field
    /// instead: each one takes the colour all its features on the bars shown share, else its
    /// palette colour, so the key holds one colour per value. Stacked, it also holds a sum, a
    /// colour and two indexes (32 bytes, and the B-tree's own room) for each pair of a group value
    /// and a stack value that some feature has, at most one per feature, and the stack field's
    /// names once, as the group field's.
    pub(super) fn chart_data_with(&self, ds: &rustgis_data::Dataset, colour: Option<&dyn Fn(usize) -> Option<Rgba>>) -> Option<ChartData> {
        let ElementKind::Chart { field, label, group, statistic, order, bars, other, .. } = self else { return None };
        let fi = ds.field_index(field)?;
        let gi = match group {
            Some(g) => Some(ds.field_index(g)?),
            None => None,
        };
        // Stacked only grouped by another field, summing or counting; a stack field the layer
        // hasn't (deleted since, say) leaves the bars whole, which `stack_gone` says.
        let si = self.chart_stack().and_then(|st| ds.field_index(st));
        let stack_gone = self.chart_stack().is_some() && si.is_none();
        let number = |row: usize| ds.value(row, fi).as_f64().filter(|v| v.is_finite());
        let (mut no_number, mut out_of_range) = (0, 0);
        // Grouped, a tally per value of the group field, in the order the values first come, and
        // each one's name (none: blank, apart from a value that reads "(blank)").
        let mut groups: Vec<Tally> = Vec::new();
        let mut names: Vec<Option<String>> = Vec::new();
        // Each bar's value and its group, or ungrouped its feature's row.
        let mut drawn: Vec<(f64, usize)> = Vec::new();
        // Coloured, grouped: the colour each value's features share, by the tallies' order.
        let mut shades: Vec<Shade> = Vec::new();
        // Stacked: the total of each pair of a group value and a stack value, and coloured the
        // colour the pair's features share, so a value's colour is read only over the bars shown.
        // Only a sum or a count stacks, so a total is all a stretch needs.
        let mut stacks = ValueIndex::default();
        let mut cells = std::collections::BTreeMap::<(usize, usize), (f64, Shade)>::new();
        // The pair the last features were in, their total and colour: features in a row of the
        // same pair (as sorted or nearby rows often are) are added up before the map is looked up.
        let mut run: Option<((usize, usize), (f64, Shade))> = None;
        let stat = match gi {
            Some(gi) => {
                let mut index = ValueIndex::default();
                for row in 0..ds.features.len() {
                    let at = index.slot(ds.value(row, gi));
                    if at == groups.len() {
                        groups.push(Tally::default());
                    }
                    let Some(t) = groups.get_mut(at) else { continue };
                    t.records += 1;
                    // A count reads no number.
                    let n = if *statistic == StatKind::Count { None } else { number(row) };
                    let counted = *statistic == StatKind::Count
                        || match n {
                            Some(n) => {
                                t.add(n, *statistic == StatKind::Mean);
                                true
                            }
                            None => {
                                no_number += 1;
                                false
                            }
                        };
                    if let Some(si) = si.filter(|_| counted) {
                        let pair = (at, stacks.slot(ds.value(row, si)));
                        // Another pair ends the run: its total and colour go to its pair's cell.
                        let same = run.as_ref().is_some_and(|(p, _)| *p == pair);
                        if !same && let Some((p, cell)) = run.replace((pair, (0.0, Shade::Unread))) {
                            add_cell(&mut cells, p, cell);
                        }
                        if let Some((_, (total, shade))) = &mut run {
                            // A sum adds the feature's number; a count, the feature.
                            *total += n.unwrap_or(1.0);
                            // Once mixed, a pair's colour is settled: no more symbols are read.
                            if let Some(colour) = colour.filter(|_| !matches!(shade, Shade::Mixed)) {
                                shade.add(colour(row));
                            }
                        }
                    } else if let Some(colour) = colour.filter(|_| counted) {
                        shades.resize(groups.len(), Shade::Unread);
                        if let Some(shade) = shades.get_mut(at).filter(|s| !matches!(s, Shade::Mixed)) {
                            shade.add(colour(row));
                        }
                    }
                }
                if let Some((pair, cell)) = run.take() {
                    add_cell(&mut cells, pair, cell);
                }
                // A value whose features have no number has no bar: they are counted in `no_number`;
                // one whose statistic is past a number's range, in `out_of_range`.
                names = index.names();
                for (i, t) in groups.iter().enumerate() {
                    match t.value(*statistic) {
                        Some(v) => drawn.push((v, i)),
                        None if t.numbers > 0 => out_of_range += 1,
                        None => {}
                    }
                }
                *statistic
            }
            None => {
                for row in 0..ds.features.len() {
                    match number(row) {
                        Some(n) => drawn.push((n, row)),
                        None => no_number += 1,
                    }
                }
                StatKind::Sum
            }
        };
        let li = label.as_deref().and_then(|l| ds.field_index(l));
        let name = |i: usize| match gi {
            Some(_) => names.get(i).and_then(|n| n.as_deref()).unwrap_or(CHART_BLANK).to_string(),
            None => li.map(|l| ds.value(i, l).to_string()).unwrap_or_default(),
        };
        // Stable sorts: equal values keep the layer's order.
        match order {
            ChartOrder::Descending => drawn.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)),
            ChartOrder::Ascending => drawn.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)),
            ChartOrder::Category => drawn.sort_by_cached_key(|(_, i)| name(*i).to_lowercase()),
            ChartOrder::Layer => {}
        }
        let limit = (*bars).clamp(1, MAX_CHART_BARS);
        let mut left_out = 0;
        let mut gathered = None;
        // Stacked, the groups an Other bar gathers, whose stretches it adds up.
        let mut rest_groups: Vec<usize> = Vec::new();
        if drawn.len() > limit {
            if *other {
                let rest = drawn.split_off(limit - 1);
                let mut t = Tally::default();
                for (v, i) in &rest {
                    match gi {
                        Some(_) => t.merge(groups.get(*i)),
                        None => {
                            t.records += 1;
                            t.add(*v, false);
                        }
                    }
                }
                gathered = t.value(stat);
                // A gathered statistic past a number's range has no bar: its bars are said.
                if gathered.is_none() {
                    out_of_range += rest.len();
                } else if si.is_some() {
                    rest_groups = rest.iter().map(|(_, i)| *i).collect();
                }
            } else {
                left_out = drawn.len() - limit;
                drawn.truncate(limit);
            }
        }
        // Stacked, the key and each bar's stretches, an Other bar's from the groups it gathers.
        let (key, stretches, key_kept, key_gathered, whole) = match si {
            None => (Vec::new(), Vec::new(), 0, 0, 0),
            Some(_) => {
                let mut bar_of = vec![None; groups.len()];
                for (bar, (_, g)) in drawn.iter().enumerate() {
                    if let Some(b) = bar_of.get_mut(*g) {
                        *b = Some(bar);
                    }
                }
                for g in &rest_groups {
                    if let Some(b) = bar_of.get_mut(*g) {
                        *b = Some(drawn.len());
                    }
                }
                let bars = drawn.len() + usize::from(gathered.is_some());
                stack_bars(&cells, &bar_of, bars, stacks.names(), colour.is_some(), *order == ChartOrder::Category)
            }
        };
        // Coloured, each bar's colour, those without one counted; an Other bar keeps the chart's.
        let (mut mixed, mut unshown) = (0, 0);
        let colors: Vec<Option<Rgba>> = match colour.filter(|_| si.is_none()) {
            None => Vec::new(),
            Some(colour) => drawn
                .iter()
                .map(|(_, i)| {
                    let shown = match gi {
                        None => colour(*i),
                        Some(_) => match shades.get(*i) {
                            Some(Shade::One(c)) => Some(*c),
                            Some(Shade::Mixed) => {
                                mixed += 1;
                                return None;
                            }
                            _ => None,
                        },
                    };
                    if shown.is_none() {
                        unshown += 1;
                    }
                    shown
                })
                .chain(gathered.map(|_| None))
                .collect(),
        };
        let mut out: Vec<(f64, String)> = drawn.into_iter().map(|(v, i)| (v, name(i))).collect();
        out.extend(gathered.map(|v| (v, CHART_OTHER.to_string())));
        let title = self.chart_title().unwrap_or_default();
        // An Other bar is named, so it isn't taken for one feature's.
        let labelled = gi.is_some() || li.is_some() || gathered.is_some();
        Some(ChartData {
            bars: out,
            labelled,
            left_out,
            out_of_range,
            no_number,
            title,
            colors,
            mixed,
            unshown,
            key,
            stretches,
            key_kept,
            key_gathered,
            whole,
            stack_gone,
        })
    }

    /// A chart's title: its own, "" for none, or the title it is given: its field, or the
    /// statistic of its field by the group; `None` for another kind.
    pub fn chart_title(&self) -> Option<String> {
        match self {
            ElementKind::Chart { title: Some(t), .. } => Some(t.clone()),
            _ => self.chart_given_title(),
        }
    }

    /// The title a chart is given when it has none of its own: its field, or the statistic of
    /// its field by the group; `None` for another kind.
    pub fn chart_given_title(&self) -> Option<String> {
        let ElementKind::Chart { field, group, statistic, .. } = self else { return None };
        Some(match (group, statistic) {
            (Some(g), StatKind::Count) => format!("Count by {g}"),
            (Some(g), s) => format!("{} of {field} by {g}", s.label()),
            (None, _) => field.clone(),
        })
    }

    /// Whether a chart uses what a chart had none of before #2487's look options (columns,
    /// values, their decimals, its own title or none, a bar colour, labels against the bars): an
    /// older reader would draw it as bars running across in the default blue, with the title it
    /// is given, and save it again without them.
    pub fn chart_uses_look_options(&self) -> bool {
        // Only part 2's options: facing another chart is its own capability ([`Self::chart_uses_facing`]).
        let part_two = |l: ChartLook| ChartLook { mirror: false, max: None, hide_labels: false, bar_outline: None, ..l };
        matches!(self, ElementKind::Chart { title: Some(_), .. }) || self.chart_look().is_some_and(|l| part_two(l) != ChartLook::default())
    }

    /// Whether a chart colours its bars by its layer's classes, which a chart had no way to before
    /// #2487 part 3: an older reader would draw them all in its bar colour, and save it again
    /// without.
    pub fn chart_uses_classes(&self) -> bool {
        matches!(self, ElementKind::Chart { classes: true, .. })
    }

    /// The field a chart stacks its bars by: only when it groups by another field and sums or
    /// counts, so that a bar's stretches add up to it. Stored otherwise, it is drawn unstacked
    /// (commands refuse it or let it go, saying so).
    pub fn chart_stack(&self) -> Option<&str> {
        match self {
            ElementKind::Chart { stack: Some(st), group, statistic, .. }
                if chart_stack_refusal(group.as_deref(), *statistic, st).is_none() =>
            {
                Some(st)
            }
            _ => None,
        }
    }

    /// Whether a chart has a stack field (a blank one stacks nothing), which a chart had no way to
    /// before #2487 part 3b: an older reader would draw its bars whole, and save it again without.
    pub fn chart_uses_stack(&self) -> bool {
        matches!(self, ElementKind::Chart { stack: Some(s), .. } if !s.trim().is_empty())
    }

    /// How a chart draws its bars; `None` for another kind.
    pub fn chart_look(&self) -> Option<ChartLook> {
        let ElementKind::Chart { orientation, values, decimals, bar_color, label_align, mirror, max, hide_labels, bar_outline, .. } = self
        else {
            return None;
        };
        Some(ChartLook {
            orientation: *orientation,
            values: *values,
            decimals: *decimals,
            bar_color: bar_color.unwrap_or(CHART_BAR),
            label_align: *label_align,
            mirror: *mirror,
            max: chart_max(*max),
            hide_labels: *hide_labels,
            bar_outline: chart_outline(*bar_outline),
        })
    }

    /// Whether a chart faces another: mirrored, given a maximum or with its labels hidden, which a
    /// chart had no way to before #2487 part 4: an older reader would draw it the usual way round,
    /// on its bars' own scale, labelled, and save it again without. A maximum not above 0 is let go
    /// when the layout is read ([`Layout::clamp`]), so a stored one is always drawn to.
    pub fn chart_uses_facing(&self) -> bool {
        matches!(self, ElementKind::Chart { mirror, max, hide_labels, .. } if *mirror || *hide_labels || max.is_some())
    }

    /// Whether a chart draws a line round its bars, which a chart had no way to before #2487 part
    /// 4b: an older reader would draw them without, and save it again without.
    pub fn chart_uses_outline(&self) -> bool {
        matches!(self, ElementKind::Chart { bar_outline, .. } if chart_outline(*bar_outline).is_some())
    }
}
