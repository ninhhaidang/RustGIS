//! The tools.

mod check_geometry;
mod geometry;

use geo::{
    BooleanOps, Contains, ConvexHull, CoordsIter, InteriorPoint, Intersects, MultiPolygon, Orient, Relate, Simplify, orient::Direction,
};
use rustgis_data::raster::Crs;
use rustgis_data::{Coord, Dataset, Feature, Field, FieldType, Geometry, GeometryType, Value};
use rustgis_geom::Extent;
use serde_json::Value as Json;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet, TryReserveError};
use std::ops::ControlFlow;

use crate::convert::{from_line, from_multipolygon, from_point, to_geo, to_multiline, to_multipolygon};
use crate::geodesic::{PARALLEL_FEATURES, VERTEX_BUDGET_ERROR, geodesic_buffer_with_limit, geodesic_buffers_with_limit};
use crate::{Inputs, Output, OutputLayer, ParamKind as K, ParamSpec as P, ToolResult, ToolSpec};

const UNITS: &[&str] = &["Kilometers", "Meters", "Miles", "Feet"];
const KRIGING_MODELS: &[&str] = &["Spherical", "Exponential", "Gaussian"];
const MATCH_OPTIONS: &[&str] =
    &["Intersects", "Contains", "Within", "Have their center in", "Largest overlap", "Closest", "Within a distance"];
/// The match options that measure a distance, for which `distance`, `units` and `distanceField` apply.
const DISTANCE_MATCHES: &[&str] = &["Closest", "Within a distance"];
const JOIN_OPERATIONS: &[&str] = &["Join one to one", "Join one to many"];
const TARGET_RECORDS: &[&str] = &["Keep all", "Keep only matching"];
const KERNEL_TYPES: &[&str] = &["Quartic", "Gaussian"];
/// The separator CONCATENATE joins values with, in the tools that take a statistics list.
const SEPARATOR: P = P {
    name: "separator",
    label: "Separator",
    kind: K::Text,
    required: false,
    default: "\", \"",
    help: "Put between the values CONCATENATE joins.",
};

pub static TOOLS: &[ToolSpec] = &[
    ToolSpec {
        id: "polygonToLine",
        name: "Polygon To Line",
        toolbox: "Data Management › Features",
        summary: "Converts polygon boundaries, including holes, to multipart lines. Shared boundaries are kept for each feature.",
        params: &[
            P { name: "input", label: "Input Features", kind: K::PolygonLayer, required: true, default: "", help: "" },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: polygon_to_line,
    },
    ToolSpec {
        id: "splitLineAtVertices",
        name: "Split Line At Vertices",
        toolbox: "Data Management › Features",
        summary: "Writes a separate two-vertex line for each consecutive vertex pair.",
        params: &[
            P { name: "input", label: "Input Features", kind: K::Layer, required: true, default: "", help: "" },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: split_line_at_vertices,
    },
    ToolSpec {
        id: "featureEnvelopeToPolygon",
        name: "Feature Envelope To Polygon",
        toolbox: "Data Management › Features",
        summary: "Writes one axis-aligned bounding rectangle per feature in stored longitude/latitude coordinates. Zero-area envelopes are skipped.",
        params: &[
            P { name: "input", label: "Input Features", kind: K::Layer, required: true, default: "", help: "" },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: feature_envelope_to_polygon,
    },
    ToolSpec {
        id: "unsplitLine",
        name: "Unsplit Line",
        toolbox: "Data Management › Features",
        summary: "Joins exact endpoint matches through degree-two nodes when all attributes agree. Branches stay separate.",
        params: &[
            P { name: "input", label: "Input Features", kind: K::Layer, required: true, default: "", help: "" },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: geometry::unsplit,
    },
    ToolSpec {
        id: "checkGeometry",
        name: "Check Geometry",
        toolbox: "Data Management › Features",
        summary: "Writes the features with invalid geometry, such as self-intersecting or unclosed rings, holes outside their shell or overlapping parts, and says what is wrong with each.",
        params: &[
            P { name: "input", label: "Input Features", kind: K::VectorLayer, required: true, default: "", help: "" },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: check_geometry::run,
    },
    ToolSpec {
        id: "buffer",
        name: "Buffer",
        toolbox: "Analysis › Proximity",
        summary: "Creates polygons around input features at a given distance.",
        params: &[
            P { name: "input", label: "Input Features", kind: K::Layer, required: true, default: "", help: "" },
            P { name: "distance", label: "Distance", kind: K::Number, required: true, default: "100", help: "" },
            P { name: "units", label: "Units", kind: K::Choice(UNITS), required: false, default: "\"Kilometers\"", help: "" },
            P { name: "dissolve", label: "Dissolve", kind: K::Choice(&["None", "All"]), required: false, default: "\"None\"", help: "" },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: buffer,
    },
    ToolSpec {
        id: "clip",
        name: "Clip",
        toolbox: "Analysis › Extract",
        summary: "Extracts the parts of input features that fall inside clip polygons.",
        params: &[
            P { name: "input", label: "Input Features", kind: K::Layer, required: true, default: "", help: "" },
            P { name: "clip", label: "Clip Features", kind: K::PolygonLayer, required: true, default: "", help: "" },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: clip,
    },
    ToolSpec {
        id: "erase",
        name: "Erase",
        toolbox: "Analysis › Overlay",
        summary: "Removes the parts of input features that fall inside erase polygons.",
        params: &[
            P { name: "input", label: "Input Features", kind: K::Layer, required: true, default: "", help: "" },
            P { name: "erase", label: "Erase Features", kind: K::PolygonLayer, required: true, default: "", help: "" },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: erase,
    },
    ToolSpec {
        id: "intersect",
        name: "Intersect",
        toolbox: "Analysis › Overlay",
        summary: "Splits input features where they overlap overlay polygons, keeping both layers' attributes.",
        params: &[
            P { name: "input", label: "Input Features", kind: K::Layer, required: true, default: "", help: "" },
            P { name: "overlay", label: "Overlay Features", kind: K::PolygonLayer, required: true, default: "", help: "" },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: intersect,
    },
    ToolSpec {
        id: "identity",
        name: "Identity",
        toolbox: "Analysis › Overlay",
        summary: "Splits input features by overlay polygons, keeping every part: those inside get the overlay's attributes.",
        params: &[
            P { name: "input", label: "Input Features", kind: K::Layer, required: true, default: "", help: "" },
            P { name: "overlay", label: "Identity Features", kind: K::PolygonLayer, required: true, default: "", help: "" },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: identity,
    },
    ToolSpec {
        id: "union",
        name: "Union",
        toolbox: "Analysis › Overlay",
        summary: "Combines two polygon layers into every piece either covers, with the attributes of both where they overlap.",
        params: &[
            P { name: "input", label: "Input Features", kind: K::PolygonLayer, required: true, default: "", help: "" },
            P { name: "overlay", label: "Union Features", kind: K::PolygonLayer, required: true, default: "", help: "" },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: union,
    },
    ToolSpec {
        id: "symmetricalDifference",
        name: "Symmetrical Difference",
        toolbox: "Analysis › Overlay",
        summary: "Keeps the parts of two polygon layers that only one of them covers, with that layer's attributes.",
        params: &[
            P { name: "input", label: "Input Features", kind: K::PolygonLayer, required: true, default: "", help: "" },
            P { name: "overlay", label: "Overlay Features", kind: K::PolygonLayer, required: true, default: "", help: "" },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: symmetrical_difference,
    },
    ToolSpec {
        id: "update",
        name: "Update",
        toolbox: "Analysis › Overlay",
        summary: "Replaces the parts of input polygons that update polygons cover with the update polygons themselves.",
        params: &[
            P { name: "input", label: "Input Features", kind: K::PolygonLayer, required: true, default: "", help: "" },
            P { name: "update", label: "Update Features", kind: K::PolygonLayer, required: true, default: "", help: "" },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: update,
    },
    ToolSpec {
        id: "split",
        name: "Split",
        toolbox: "Analysis › Extract",
        summary: "Clips input features by each split polygon, writing one layer per split polygon or split field value.",
        params: &[
            P { name: "input", label: "Input Features", kind: K::Layer, required: true, default: "", help: "" },
            P { name: "split", label: "Split Features", kind: K::PolygonLayer, required: true, default: "", help: "" },
            P {
                name: "field",
                label: "Split Field",
                kind: K::Text,
                required: false,
                default: "",
                help: "A field of the split features. Polygons sharing a value make one layer, named by it. Leave empty for one layer per polygon, named by its id.",
            },
            P {
                name: "output",
                label: "Output Name",
                kind: K::Text,
                required: false,
                default: "",
                help: "The start of each layer's name, before an underscore and its value. Leave empty for the input's name.",
            },
        ],
        run: split,
    },
    ToolSpec {
        id: "dissolve",
        name: "Dissolve",
        toolbox: "Data Management › Generalization",
        summary: "Merges polygons that share a field value into one feature per value.",
        params: &[
            P { name: "input", label: "Input Features", kind: K::PolygonLayer, required: true, default: "", help: "" },
            P { name: "field", label: "Dissolve Field", kind: K::Field, required: false, default: "", help: "" },
            P {
                name: "statistics",
                label: "Statistics",
                kind: K::FieldStatistics { input: "input", statistics: crate::StatisticList::Summary },
                required: false,
                default: "",
                help: "Summary fields to add, as (field, statistic) pairs over each group's polygons, e.g. [{\"field\": \"pop\", \"statistic\": \"SUM\"}] adds SUM_pop. The statistics of Summary Statistics.",
            },
            SEPARATOR,
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: dissolve,
    },
    ToolSpec {
        id: "near",
        name: "Near",
        toolbox: "Analysis › Proximity",
        summary: "Adds the distance to the nearest feature of another layer, its id and the nearest point on it.",
        params: &[
            P { name: "input", label: "Input Features", kind: K::Layer, required: true, default: "", help: "" },
            P { name: "near", label: "Near Features", kind: K::Layer, required: true, default: "", help: "" },
            P {
                name: "distance",
                label: "Search Distance",
                kind: K::Number,
                required: false,
                default: "",
                help: "Features with nothing this near get empty values. Leave empty to search without a limit.",
            },
            P { name: "units", label: "Distance Units", kind: K::Choice(UNITS), required: false, default: "\"Kilometers\"", help: "" },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: near,
    },
    ToolSpec {
        id: "convexHull",
        name: "Minimum Bounding Geometry",
        toolbox: "Data Management › Features",
        summary: "Encloses each feature, or all features, in its convex hull.",
        params: &[
            P { name: "input", label: "Input Features", kind: K::Layer, required: true, default: "", help: "" },
            P { name: "group", label: "Group Option", kind: K::Choice(&["All", "None"]), required: false, default: "\"All\"", help: "" },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: convex_hull,
    },
    ToolSpec {
        id: "featureToPoint",
        name: "Feature To Point",
        toolbox: "Data Management › Features",
        summary: "Creates a point inside each input feature.",
        params: &[
            P { name: "input", label: "Input Features", kind: K::Layer, required: true, default: "", help: "" },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: feature_to_point,
    },
    ToolSpec {
        id: "merge",
        name: "Merge",
        toolbox: "Data Management › General",
        summary: "Appends vector layers of the same geometry type into a new layer, optionally combining their attributes.",
        params: &[
            P {
                name: "inputs",
                label: "Input Layers",
                kind: K::VectorLayers,
                required: true,
                default: "",
                help: "Layer names or local file paths (desktop), in order; selections are honored in each layer.",
            },
            P {
                name: "attributes",
                label: "Preserve Attributes",
                kind: K::Bool,
                required: false,
                default: "true",
                help: "Combines matching fields. Incompatible field types get separate names; missing values are empty.",
            },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: crate::merge::run,
    },
    ToolSpec {
        id: "featureVerticesToPoints",
        name: "Feature Vertices To Points",
        toolbox: "Data Management › Features",
        summary: "Creates a point at each vertex of the input features.",
        params: &[
            P { name: "input", label: "Input Features", kind: K::VectorLayer, required: true, default: "", help: "" },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: feature_vertices_to_points,
    },
    ToolSpec {
        id: "pointsAlongLines",
        name: "Points Along Lines",
        toolbox: "Data Management › Features",
        summary: "Creates points at a set distance along each line, or each polygon ring.",
        params: &[
            P {
                name: "input",
                label: "Input Features",
                kind: K::VectorLayer,
                required: true,
                default: "",
                help: "A line or polygon layer. Each part of a line, and each ring of a polygon, is walked on its own.",
            },
            P {
                name: "distance",
                label: "Distance",
                kind: K::Number,
                required: true,
                default: "1",
                help: "The distance between points, measured on the ground.",
            },
            P { name: "units", label: "Units", kind: K::Choice(UNITS), required: false, default: "\"Kilometers\"", help: "" },
            P {
                name: "startOffset",
                label: "Start Offset",
                kind: K::Number,
                required: false,
                default: "0",
                help: "How far along each part the first point is, in the same units.",
            },
            P {
                name: "endOffset",
                label: "End Offset",
                kind: K::Number,
                required: false,
                default: "0",
                help: "How far before each part's end the points stop, in the same units.",
            },
            P {
                name: "includeEnd",
                label: "Include End Point",
                kind: K::Bool,
                required: false,
                default: "false",
                help: "Also a point where each part's points stop, when the distance doesn't land there. (The first is always at the start offset.)",
            },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: points_along_lines,
    },
    ToolSpec {
        id: "xyTableToPoint",
        name: "XY Table To Point",
        toolbox: "Data Management › Features",
        summary: "Creates points from a table's x and y coordinate fields.",
        params: &[
            P { name: "input", label: "Input Table", kind: K::Layer, required: true, default: "", help: "" },
            P {
                name: "x",
                label: "X Field",
                kind: K::CoordinateField,
                required: true,
                default: "",
                help: "Longitude or easting: numbers, or text holding them. A single comma is the decimal separator (1,234 is 1.234).",
            },
            P {
                name: "y",
                label: "Y Field",
                kind: K::CoordinateField,
                required: true,
                default: "",
                help: "Latitude or northing, read as the X field is.",
            },
            P {
                name: "crs",
                label: "Coordinate System",
                kind: K::Text,
                required: false,
                default: "\"EPSG:4326\"",
                help: "The coordinates' system, as EPSG:<code> or WKT; longitude/latitude (EPSG:4326) when empty.",
            },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: xy_table_to_point,
    },
    ToolSpec {
        id: "simplify",
        name: "Simplify",
        toolbox: "Cartography › Generalization",
        summary: "Removes vertices that deviate less than a tolerance (Douglas–Peucker).",
        params: &[
            P { name: "input", label: "Input Features", kind: K::Layer, required: true, default: "", help: "" },
            P { name: "tolerance", label: "Tolerance (km)", kind: K::Number, required: true, default: "25", help: "" },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: simplify,
    },
    ToolSpec {
        id: "tissot",
        name: "Tissot's Indicatrix",
        toolbox: "Cartography › Projections",
        summary: "Geodesic circles on a grid that show how the map's projection distorts scale, area and angles.",
        params: &[
            P { name: "spacing", label: "Grid Spacing (degrees)", kind: K::Number, required: false, default: "30", help: "" },
            P { name: "radius", label: "Circle Radius (km)", kind: K::Number, required: false, default: "500", help: "" },
            P {
                name: "area",
                label: "Area",
                kind: K::Choice(&["World", "Map Extent"]),
                required: false,
                default: "\"World\"",
                help: "Map Extent fills the view: for a country or a state, use a smaller spacing and radius.",
            },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: crate::tissot::run,
    },
    ToolSpec {
        id: "a5Grid",
        name: "A5 Grid",
        toolbox: "Analysis › DGGS",
        summary: "A5's equal-area pentagonal cells at a resolution, over the map's extent, a layer or the world, as polygons with their ids.",
        params: &[
            P {
                name: "resolution",
                label: "Resolution (0–30)",
                kind: K::Number,
                required: false,
                default: "5",
                help: "Each step divides a cell into about four: resolution 5 cells are about 33,000 km², 9 about 130 km², 12 about 2 km².",
            },
            P {
                name: "area",
                label: "Area",
                kind: K::Choice(&["Map Extent", "Input Layer", "World"]),
                required: false,
                default: "\"Map Extent\"",
                help: "Input Layer covers its polygons (or the extent of its points and lines).",
            },
            P { name: "input", label: "Input Layer", kind: K::Layer, required: false, default: "", help: "" },
            P {
                name: "containment",
                label: "Cells",
                kind: K::Choice(&["Overlapping", "Center"]),
                required: false,
                default: "\"Overlapping\"",
                help: "Overlapping covers the area without gaps; Center keeps the cells whose centre is inside.",
            },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: crate::dggs::grid,
    },
    ToolSpec {
        id: "a5Aggregate",
        name: "Aggregate Points to A5",
        toolbox: "Analysis › DGGS",
        summary: "Counts a layer's points by the A5 cell holding them (cells of equal area, so the count is a density), with a field's sum and mean.",
        params: &[
            P { name: "input", label: "Input Points", kind: K::PointLayer, required: true, default: "", help: "" },
            P { name: "resolution", label: "Resolution (0–30)", kind: K::Number, required: false, default: "9", help: "" },
            P {
                name: "field",
                label: "Field to Sum and Average",
                kind: K::NumericField,
                required: false,
                default: "",
                help: "A feature's value counts once in each cell: a multipoint's, however many of its points fall in the cell.",
            },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: crate::dggs::aggregate,
    },
    ToolSpec {
        id: "selectByLocation",
        name: "Select Layer By Location",
        toolbox: "Data Management › Layers and Table Views",
        summary: "Selects features that intersect features in another layer.",
        params: &[
            P { name: "input", label: "Input Features", kind: K::Layer, required: true, default: "", help: "" },
            P { name: "select", label: "Selecting Features", kind: K::Layer, required: true, default: "", help: "" },
            P { name: "distance", label: "Search Distance", kind: K::Number, required: false, default: "0", help: "" },
            P { name: "units", label: "Units", kind: K::Choice(UNITS), required: false, default: "\"Kilometers\"", help: "" },
            P { name: "invert", label: "Invert", kind: K::Choice(&["No", "Yes"]), required: false, default: "\"No\"", help: "" },
        ],
        run: select_by_location,
    },
    ToolSpec {
        id: "selectByAttributes",
        name: "Select Layer By Attribute",
        toolbox: "Data Management › Layers and Table Views",
        summary: "Selects features with a SQL where clause, e.g. pop_est > 1000000.",
        params: &[
            P { name: "input", label: "Input Rows", kind: K::Layer, required: true, default: "", help: "" },
            P { name: "where", label: "Where Clause", kind: K::Text, required: true, default: "", help: "" },
        ],
        run: select_by_attributes,
    },
    ToolSpec {
        id: "countPoints",
        name: "Summarize Within",
        toolbox: "Analysis › Summarize",
        summary: "Counts the points that fall inside each polygon.",
        params: &[
            P { name: "polygons", label: "Input Polygons", kind: K::PolygonLayer, required: true, default: "", help: "" },
            P { name: "points", label: "Input Summary Features", kind: K::PointLayer, required: true, default: "", help: "" },
            P {
                name: "statistics",
                label: "Statistics",
                kind: K::FieldStatistics { input: "points", statistics: crate::StatisticList::Summary },
                required: false,
                default: "",
                help: "Summary fields to add, as (field, statistic) pairs over the points in each polygon, e.g. [{\"field\": \"pop\", \"statistic\": \"SUM\"}] adds SUM_pop. The statistics of Summary Statistics.",
            },
            SEPARATOR,
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: count_points,
    },
    ToolSpec {
        id: "calculateGeometry",
        name: "Calculate Geometry Attributes",
        toolbox: "Data Management › Features",
        summary: "Adds geodesic area (km²), length (km) or centroid coordinates as fields.",
        params: &[P { name: "input", label: "Input Features", kind: K::Layer, required: true, default: "", help: "" }],
        run: calculate_geometry,
    },
    ToolSpec {
        id: "summaryStatistics",
        name: "Summary Statistics",
        toolbox: "Analysis › Statistics",
        summary: "Statistics of fields, for the whole table or for each group of records sharing the case fields' values.",
        params: &[
            P { name: "input", label: "Input Table", kind: K::Layer, required: true, default: "", help: "" },
            P {
                name: "statistics",
                label: "Statistics",
                kind: K::FieldStatistics { input: "input", statistics: crate::StatisticList::Summary },
                required: false,
                default: "",
                help: "The statistics to compute, as (field, statistic) pairs, e.g. [{\"field\": \"income\", \"statistic\": \"MEDIAN\"}]. Statistics: COUNT, SUM, MEAN, MIN, MAX, RANGE, STD, VARIANCE, MEDIAN, P1 to P99, IQR, CV, MODE, MINORITY, UNIQUE, FIRST, LAST, CONCATENATE, NULLS; a text field takes COUNT, MIN, MAX, FIRST, LAST, UNIQUE, CONCATENATE, MODE, MINORITY and NULLS. Leave empty to use Statistics Field.",
            },
            P {
                name: "field",
                label: "Statistics Field",
                kind: K::NumericField,
                required: false,
                default: "",
                help: "Without Statistics: the field whose count, sum, mean, min, max and standard deviation are computed.",
            },
            P {
                name: "case",
                label: "Case Fields",
                kind: K::Fields { input: "input" },
                required: false,
                default: "",
                help: "Groups records by the combination of these fields' values. Scripts and the AI Assistant can give several fields as a list.",
            },
            SEPARATOR,
        ],
        run: summary_statistics,
    },
    ToolSpec {
        id: "pivotTable",
        name: "Pivot Table",
        toolbox: "Analysis › Statistics",
        summary: "Turns a field's values into fields: a row for each combination of the row fields' values, a column for each value of the pivot field.",
        params: &[
            P { name: "input", label: "Input Table", kind: K::Layer, required: true, default: "", help: "" },
            P {
                name: "fields",
                label: "Row Fields",
                kind: K::Fields { input: "input" },
                required: false,
                default: "",
                help: "Makes a row for each combination of these fields' values. Leave empty for one row for the whole table. Scripts and the AI Assistant can give several fields as a list.",
            },
            P {
                name: "pivot",
                label: "Pivot Field",
                kind: K::Field,
                required: true,
                default: "",
                help: "Makes a field for each of this field's values, sorted by value.",
            },
            P {
                name: "value",
                label: "Value Field",
                kind: K::Field,
                required: true,
                default: "",
                help: "The field whose values fill the table.",
            },
            P {
                name: "statistic",
                label: "Statistic",
                kind: K::Text,
                required: false,
                default: "",
                help: "Combines the values of the records sharing a row and a column: COUNT, SUM, MEAN, MIN, MAX, RANGE, STD, VARIANCE, MEDIAN, P1 to P99, IQR, CV, MODE, MINORITY, UNIQUE, FIRST, LAST, CONCATENATE or NULLS; a text field takes COUNT, MIN, MAX, FIRST, LAST, UNIQUE, CONCATENATE, MODE, MINORITY and NULLS. Leave empty to place each value as it is; then a row and column can hold one record at most.",
            },
            P {
                name: "separator",
                label: "Separator",
                kind: K::Text,
                required: false,
                default: "\", \"",
                help: "Put between the values CONCATENATE joins.",
            },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: pivot_table,
    },
    ToolSpec {
        id: "tabulateIntersection",
        name: "Tabulate Intersection",
        toolbox: "Analysis › Statistics",
        summary: "How much of each zone each class of polygons covers, with the classes' totals shared out to the zones by area.",
        params: &[
            P { name: "zones", label: "Input Zone Features", kind: K::PolygonLayer, required: true, default: "", help: "" },
            P {
                name: "zoneFields",
                label: "Zone Fields",
                kind: K::Fields { input: "zones" },
                required: true,
                default: "",
                help: "Zones sharing these fields' values make one zone. Scripts and the AI Assistant can give several fields as a list.",
            },
            P { name: "classes", label: "Input Class Features", kind: K::PolygonLayer, required: true, default: "", help: "" },
            P {
                name: "classFields",
                label: "Class Fields",
                kind: K::Fields { input: "classes" },
                required: false,
                default: "",
                help: "Class features sharing these fields' values make one class. Leave empty to treat them all as one class.",
            },
            P {
                name: "sumFields",
                label: "Sum Fields",
                kind: K::Fields { input: "classes" },
                required: false,
                default: "",
                help: "Numeric fields of the class features whose values are shared out to the zones by the part of each feature's area in them.",
            },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: tabulate_intersection,
    },
    ToolSpec {
        id: "exportFeatures",
        name: "Export Features",
        toolbox: "Conversion",
        summary: "Copies the (selected) features of a layer to a new layer.",
        params: &[
            P { name: "input", label: "Input Features", kind: K::Layer, required: true, default: "", help: "" },
            P { name: "where", label: "Expression", kind: K::Text, required: false, default: "", help: "" },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: export_features,
    },
    ToolSpec {
        id: "idwInterpolation",
        name: "IDW Interpolation",
        toolbox: "Analysis › Interpolation",
        summary: "Interpolates a raster surface from feature values by inverse distance weighting, as QGIS does.",
        params: &[
            P {
                name: "input",
                label: "Vector Layer",
                kind: K::VectorLayer,
                required: true,
                default: "",
                help: "Points, or lines and polygons whose vertices are used as points.",
            },
            P { name: "field", label: "Interpolation Attribute", kind: K::NumericField, required: true, default: "", help: "" },
            P {
                name: "power",
                label: "Distance Coefficient P",
                kind: K::Number,
                required: false,
                default: "2",
                help: "How fast a point's weight falls off with distance (weight = 1/distanceᴾ). 0 averages every point.",
            },
            P {
                name: "extent",
                label: "Extent",
                kind: K::Text,
                required: false,
                default: "",
                help: "xmin,ymin,xmax,ymax in degrees; the input's extent when empty.",
            },
            P {
                name: "pixelSize",
                label: "Pixel Size (°)",
                kind: K::Number,
                required: false,
                default: "",
                help: "The output cell size in degrees; 300 cells across the extent when empty.",
            },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: idw_interpolation,
    },
    ToolSpec {
        id: "spatialJoin",
        name: "Spatial Join",
        toolbox: "Analysis › Overlay",
        summary: "Joins attributes from a join layer to a target layer based on their spatial relationship.",
        params: &[
            P {
                name: "target",
                label: "Target Features",
                kind: K::Layer,
                required: true,
                default: "",
                help: "The features that receive joined attributes.",
            },
            P {
                name: "join",
                label: "Join Features",
                kind: K::Layer,
                required: true,
                default: "",
                help: "The features whose attributes are joined.",
            },
            P {
                name: "match",
                label: "Match Option",
                kind: K::Choice(MATCH_OPTIONS),
                required: false,
                default: "\"Intersects\"",
                help: "The spatial relationship between target and join features. Have their center in uses the point Feature To Point gives; Largest overlap joins the polygon covering most of a polygon (by area) or line (by length); Closest joins the nearest feature; Within a distance joins those within the search distance, nearest first.",
            },
            P {
                name: "distance",
                label: "Search Distance",
                kind: K::Number,
                required: false,
                default: "",
                help: "Closest: how far to look (no limit when empty). Within a distance: required. Measured on the ground.",
            },
            P { name: "units", label: "Distance Units", kind: K::Choice(UNITS), required: false, default: "\"Kilometers\"", help: "" },
            P {
                name: "distanceField",
                label: "Distance Field",
                kind: K::Text,
                required: false,
                default: "",
                help: "A field added for the distance to the joined feature, in the distance units (Closest and Within a distance).",
            },
            P {
                name: "operation",
                label: "Join Operation",
                kind: K::Choice(JOIN_OPERATIONS),
                required: false,
                default: "\"Join one to one\"",
                help: "One to one joins the first match and counts total matches; one to many creates a row for each match.",
            },
            P {
                name: "keepAll",
                label: "Target Records",
                kind: K::Choice(TARGET_RECORDS),
                required: false,
                default: "\"Keep all\"",
                help: "Whether target features without matching join features are kept.",
            },
            P {
                name: "prefix",
                label: "Field Prefix",
                kind: K::Text,
                required: false,
                default: "\"join_\"",
                help: "Prefix added to join feature fields to avoid collision.",
            },
            P {
                name: "merge",
                label: "Merge Rules",
                kind: K::FieldStatistics { input: "join", statistics: crate::StatisticList::Merge },
                required: false,
                default: "",
                help: "One to one: how the values of several matches are combined, e.g. [{\"field\": \"units\", \"rule\": \"sum\"}]. Rules: first, last, sum, mean, min, max, count, join (with a \"delimiter\"). Other fields keep the first match.",
            },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: spatial_join,
    },
    ToolSpec {
        id: "kriging",
        name: "Ordinary Kriging",
        toolbox: "Analysis › Interpolation",
        summary: "Interpolates a 2-band raster surface (Prediction and Variance) from point features using Ordinary Kriging.",
        params: &[
            P {
                name: "input",
                label: "Vector Layer",
                kind: K::VectorLayer,
                required: true,
                default: "",
                help: "Points (or bounding-box centres for lines and polygons) with values to interpolate.",
            },
            P { name: "field", label: "Interpolation Attribute", kind: K::NumericField, required: true, default: "", help: "" },
            P {
                name: "model",
                label: "Variogram Model",
                kind: K::Choice(KRIGING_MODELS),
                required: false,
                default: "\"Spherical\"",
                help: "Spatial autocorrelation model: Spherical, Exponential, or Gaussian.",
            },
            P {
                name: "neighbours",
                label: "Max Neighbors",
                kind: K::Number,
                required: false,
                default: "12",
                help: "Number of nearest points to solve with (default 12, max 32).",
            },
            P {
                name: "nugget",
                label: "Nugget",
                kind: K::Number,
                required: false,
                default: "",
                help: "Micro-scale variance/measurement error at zero distance (default 0).",
            },
            P {
                name: "sill",
                label: "Sill",
                kind: K::Number,
                required: false,
                default: "",
                help: "Total asymptotic variance (defaults to sample variance).",
            },
            P {
                name: "range",
                label: "Range (Degrees)",
                kind: K::Number,
                required: false,
                default: "",
                help: "Spatial correlation distance in degrees (defaults to 1/3 extent diagonal).",
            },
            P {
                name: "extent",
                label: "Extent",
                kind: K::Text,
                required: false,
                default: "",
                help: "xmin,ymin,xmax,ymax in degrees; the input's extent when empty.",
            },
            P {
                name: "pixelSize",
                label: "Pixel Size (°)",
                kind: K::Number,
                required: false,
                default: "",
                help: "The output cell size in degrees; up to 250 cells across the extent when empty.",
            },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "" },
        ],
        run: kriging_interpolation,
    },
    ToolSpec {
        id: "zonalStats",
        name: "Zonal Statistics",
        toolbox: "Analysis › Raster",
        summary: "Calculates raster summary statistics for each polygon feature.",
        params: &[
            P {
                name: "raster",
                label: "Input Raster",
                kind: K::RasterLayer,
                required: true,
                default: "",
                help: "The raster containing pixel values to summarize.",
            },
            P {
                name: "polygons",
                label: "Zone Polygons",
                kind: K::PolygonLayer,
                required: true,
                default: "",
                help: "Polygon layer defining the zones.",
            },
            P {
                name: "bands",
                label: "Bands",
                kind: K::Text,
                required: false,
                default: "\"1\"",
                help: "1-based band number(s) (e.g. 1 or [1, 2]); defaults to band 1.",
            },
            P {
                name: "stats",
                label: "Statistics",
                kind: K::Text,
                required: false,
                default: "\"All\"",
                help: "Statistical operations: All, Mean, Min, Max, Sum, Count, StdDev (comma-separated or array); defaults to All.",
            },
            P {
                name: "prefix",
                label: "Field Prefix",
                kind: K::Text,
                required: false,
                default: "",
                help: "Prefix for generated fields; defaults to raster/band name.",
            },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "Output polygon layer name." },
        ],
        run: zonal_stats,
    },
    ToolSpec {
        id: "rasterToPmtiles",
        name: "Raster to PMTiles",
        toolbox: "Analyze › Tools",
        summary: "Exports a styled raster layer as Web Mercator PNG tiles in a PMTiles archive.",
        params: &[
            P {
                name: "input",
                label: "Input Layer",
                kind: K::RasterLayer,
                required: true,
                default: "",
                help: "The raster's current style, bands and resampling are used.",
            },
            P {
                name: "minZoom",
                label: "Minimum Zoom",
                kind: K::Number,
                required: true,
                default: "",
                help: "Minimum tile zoom, an integer from 0 to 16.",
            },
            P {
                name: "maxZoom",
                label: "Maximum Zoom",
                kind: K::Number,
                required: true,
                default: "",
                help: "Maximum tile zoom, an integer from minZoom to 16.",
            },
            P {
                name: "output",
                label: "Output",
                kind: K::SaveFile { ext: "pmtiles" },
                required: false,
                default: "",
                help: "Write a .pmtiles archive.",
            },
        ],
        run: raster_to_pmtiles,
    },
    ToolSpec {
        id: "vectorToPmtiles",
        name: "Vector to PMTiles",
        toolbox: "Analyze › Tools",
        summary: "Exports a vector layer as a Web Mercator vector-tile PMTiles archive.",
        params: &[
            P { name: "input", label: "Input Layer", kind: K::VectorLayer, required: true, default: "", help: "" },
            P {
                name: "minZoom",
                label: "Minimum Zoom",
                kind: K::Number,
                required: true,
                default: "",
                help: "Minimum tile zoom, an integer from 0 to 16.",
            },
            P {
                name: "maxZoom",
                label: "Maximum Zoom",
                kind: K::Number,
                required: true,
                default: "",
                help: "Maximum tile zoom, an integer from minZoom to 16.",
            },
            P {
                name: "sourceLayer",
                label: "Source-layer Name",
                kind: K::Text,
                required: false,
                default: "",
                help: "Name inside the vector-tile archive; defaults to the input layer name.",
            },
            P {
                name: "output",
                label: "Output",
                kind: K::SaveFile { ext: "pmtiles" },
                required: false,
                default: "",
                help: "Write a .pmtiles archive.",
            },
        ],
        run: vector_to_pmtiles,
    },
    ToolSpec {
        id: "kernelDensity",
        name: "Kernel Density",
        toolbox: "Analysis › Density",
        summary: "Calculates a magnitude-per-unit-area surface from point features using a kernel function.",
        params: &[
            P {
                name: "input",
                label: "Input Points",
                kind: K::PointLayer,
                required: true,
                default: "",
                help: "Point features to calculate density from (for multipoints, each point component receives the feature's weight).",
            },
            P {
                name: "field",
                label: "Population Field",
                kind: K::NumericField,
                required: false,
                default: "",
                help: "Numeric weight or count for each point (default 1.0; multipoint parts each receive this weight).",
            },
            P {
                name: "radius",
                label: "Search Radius",
                kind: K::Number,
                required: false,
                default: "",
                help: "Search bandwidth in CRS linear units (default or 0: Silverman's rule of thumb, or 1000 units for single/coincident points).",
            },
            P {
                name: "pixelSize",
                label: "Pixel Size",
                kind: K::Number,
                required: false,
                default: "",
                help: "Output cell size in CRS linear units (default: ~250 cells along the longer side of the extent).",
            },
            P {
                name: "kernel",
                label: "Kernel Type",
                kind: K::Choice(KERNEL_TYPES),
                required: false,
                default: "\"Quartic\"",
                help: "Kernel function: Quartic (biweight, default) or Gaussian (truncated at 3σ).",
            },
            P {
                name: "extent",
                label: "Extent",
                kind: K::Text,
                required: false,
                default: "",
                help: "xmin,ymin,xmax,ymax in CRS linear units; points expanded by bandwidth when empty.",
            },
            P {
                name: "crs",
                label: "Coordinate System",
                kind: K::Text,
                required: false,
                default: "",
                help: "Projected coordinate system (e.g. EPSG:32618); defaults to the layer's CRS when projected, or required when the layer has none or is in degrees.",
            },
            P { name: "output", label: "Output Name", kind: K::Text, required: false, default: "", help: "Output raster layer name." },
        ],
        run: kernel_density_tool,
    },
    ToolSpec {
        id: "joinField",
        name: "Join Field",
        toolbox: "Data Management › Joins",
        summary: "Joins attributes from a source table or layer to a target layer or table based on matching key values.",
        params: &[
            P {
                name: "target",
                label: "Target Table / Layer",
                kind: K::Layer,
                required: true,
                default: "",
                help: "The layer or table that receives joined attributes.",
            },
            P {
                name: "targetField",
                label: "Target Key Field",
                kind: K::Field,
                required: true,
                default: "",
                help: "The key field in the target layer/table used for matching.",
            },
            P {
                name: "joinTable",
                label: "Source Table / Layer",
                kind: K::Layer,
                required: true,
                default: "",
                help: "The table or layer whose attributes are joined.",
            },
            P {
                name: "joinField",
                label: "Source Key Field",
                kind: K::Text,
                required: false,
                default: "",
                help: "The key field in the source table/layer used for matching; defaults to the target key field name when empty.",
            },
            P {
                name: "fields",
                label: "Transfer Fields",
                kind: K::Text,
                required: false,
                default: "",
                help: "Source fields to transfer (comma-separated or JSON array; all non-key fields if omitted).",
            },
            P {
                name: "prefix",
                label: "Field Prefix",
                kind: K::Text,
                required: false,
                default: "",
                help: "Optional prefix prepended to transferred field names.",
            },
            P {
                name: "duplicates",
                label: "Duplicate Keys",
                kind: K::Choice(crate::attribute_join::DuplicateStrategy::ALL),
                required: false,
                default: "\"First\"",
                help: "How to handle duplicate keys in the source table: First (first row), Last (last row), or Error.",
            },
            P {
                name: "output",
                label: "Output Name",
                kind: K::Text,
                required: false,
                default: "",
                help: "Name for the output joined layer or table.",
            },
        ],
        run: join_field_tool,
    },
];

// ---------------------------------------------------------------------------------------------
// helpers

fn s<'a>(p: &'a Json, k: &str) -> &'a str {
    p.get(k).and_then(Json::as_str).unwrap_or("")
}

fn num(p: &Json, k: &str) -> Option<f64> {
    p.get(k).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))).filter(|f: &f64| f.is_finite())
}

fn input<'a>(inp: &'a dyn Inputs, p: &Json, k: &str) -> Result<&'a Dataset, String> {
    // A number is a standalone table's id, and only that; text is a name.
    if let Some(n) = p.get(k).filter(|v| v.is_number()) {
        let table = n.as_u64().and_then(|id| u32::try_from(id).ok()).and_then(|id| inp.table(id));
        return table.ok_or_else(|| format!("no standalone table with id {n}"));
    }
    let name = s(p, k);
    inp.dataset(name).ok_or_else(|| format!("no layer named '{name}'"))
}

/// The selected rows of `len`, or all of them when nothing is selected.
fn selected_rows(sel: &[usize], len: usize) -> impl Iterator<Item = usize> + '_ {
    let all = 0..if sel.is_empty() { len } else { 0 };
    sel.iter().copied().filter(move |r| *r < len).chain(all)
}

/// Selected rows, or all rows when nothing is selected.
fn rows(inp: &dyn Inputs, p: &Json, k: &str, ds: &Dataset) -> Vec<usize> {
    selected_rows(inp.selection(s(p, k)), ds.features.len()).collect()
}

/// The features [`rows`] lists, walked in place: nothing is allocated, so a tool can count what
/// it will write before it allocates anything.
fn selected_features<'a>(sel: &'a [usize], ds: &'a Dataset) -> impl Iterator<Item = &'a rustgis_data::Feature> + 'a {
    selected_rows(sel, ds.features.len()).filter_map(|r| ds.features.get(r))
}

fn pmtiles_zooms(p: &Json) -> Result<(u8, u8), String> {
    let zoom = |name: &str| -> Result<u8, String> {
        let value = p.get(name).ok_or_else(|| format!("{name} is required"))?;
        let parsed = match value {
            Json::Number(n) => n.as_f64().filter(|n| n.is_finite() && (0.0..=16.0).contains(n) && n.fract() == 0.0).map(|n| n as u8),
            Json::String(s) => s.trim().parse::<u8>().ok().filter(|n| *n <= 16),
            _ => None,
        };
        parsed.ok_or_else(|| format!("{name} must be an integer from 0 to 16"))
    };
    let min_zoom = zoom("minZoom")?;
    let max_zoom = zoom("maxZoom")?;
    if min_zoom > max_zoom {
        return Err("minZoom must not exceed maxZoom".into());
    }
    Ok((min_zoom, max_zoom))
}

fn vector_to_pmtiles(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let ds = input(inp, p, "input")?;
    let (min_zoom, max_zoom) = pmtiles_zooms(p)?;
    let source_layer = {
        let name = s(p, "sourceLayer").trim();
        if name.is_empty() { &ds.name } else { name }
    };
    let output = s(p, "output").trim();
    let default_name = ds.name.rsplit(['/', '\\']).find(|name| !name.is_empty()).unwrap_or("output");
    let file_name = if output.is_empty() { format!("{default_name}.pmtiles") } else { output.to_string() };
    let selected = inp.selection(s(p, "input"));
    let (bytes, messages) = inp.vector_to_pmtiles(ds, selected, min_zoom, max_zoom, source_layer)?;
    Ok(ToolResult { output: Output::File { file_name, bytes }, messages })
}

fn raster_to_pmtiles(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let name = s(p, "input");
    let (min_zoom, max_zoom) = pmtiles_zooms(p)?;
    let output = s(p, "output").trim();
    let default_name = name.rsplit(['/', '\\']).find(|part| !part.is_empty()).unwrap_or("output");
    let file_name = if output.is_empty() { format!("{default_name}.pmtiles") } else { output.to_string() };
    let bytes = inp.raster_to_pmtiles(name, min_zoom, max_zoom)?;
    Ok(ToolResult { output: Output::File { file_name, bytes }, messages: vec![] })
}

/// The name of a tool's new layer, from its `output` parameter.
/// File-producing tools use `ParamKind::SaveFile` and `Output::File` instead.
fn out_name(p: &Json, default: String) -> String {
    let n = s(p, "output").trim();
    if n.is_empty() { default } else { n.to_string() }
}

/// Scale factor of Web Mercator at a latitude (metres on the map per metre on the ground).
fn merc_scale(lat: f64) -> f64 {
    1.0 / lat.clamp(-85.0, 85.0).to_radians().cos().max(0.01)
}

fn feature_lat(g: &Geometry) -> f64 {
    g.label_point().map(|c| c.y).unwrap_or(0.0)
}

/// A distance in `units` (one of [`UNITS`], as `resolve_params` spells it; empty for the default,
/// kilometres) in metres. Any other unit is an error, never kilometres (#406).
fn to_metres(d: f64, units: &str) -> Result<f64, String> {
    let unit = if units.is_empty() { "Kilometers" } else { units };
    rustgis_geom::distance_unit_metres(&unit.to_ascii_lowercase())
        .map(|factor| d * factor)
        .ok_or_else(|| format!("Units must be one of {} (not '{units}')", UNITS.join(", ")))
}

fn result(ds: Dataset, msg: String) -> Result<ToolResult, String> {
    Ok(ToolResult { output: Output::Layer(ds), messages: vec![msg] })
}

/// A copy of a feature's attributes for an output with the same schema.
fn copy_schema(ds: &Dataset, name: String, g: GeometryType) -> Dataset {
    Dataset::new(name, g, ds.fields.clone())
}

/// Gives an overlay tool's output its input's source CRS, for export, and marks it incomplete
/// when the input or the layer it is overlaid with lost features on reading, so it isn't taken
/// for complete.
fn keep_source_crs(out: &mut Dataset, ds: &Dataset, overlay: &Dataset) {
    out.crs = ds.crs;
    out.crs_incomplete = ds.crs_incomplete || overlay.crs_incomplete;
}

/// Finds the index of a field by case-insensitive name, explicitly detecting collisions.
///
/// If multiple fields in `ds.fields` match `name` case-insensitively, returns an error
/// detailing the collision rather than silently selecting an arbitrary field.
pub fn find_field_case_insensitive(ds: &Dataset, name: &str) -> Result<usize, String> {
    if let Some(idx) = ds.fields.iter().position(|f| f.name == name) {
        return Ok(idx);
    }
    let mut matches = Vec::new();
    for (idx, f) in ds.fields.iter().enumerate() {
        if f.name.eq_ignore_ascii_case(name) {
            matches.push(idx);
        }
    }
    match matches.as_slice() {
        [] => Err(format!("no field named '{name}'")),
        [only] => Ok(*only),
        _ => {
            let names: Vec<String> = matches.iter().filter_map(|&i| ds.fields.get(i).map(|f| f.name.clone())).collect();
            Err(format!(
                "field name '{name}' is ambiguous: case-insensitive collision between {}",
                names.iter().map(|n| format!("'{n}'")).collect::<Vec<_>>().join(" and ")
            ))
        }
    }
}

/// Union of polygons; rings are oriented first because the overlay infers its fill rule from
/// the first ring's winding, and input files mix orientations.
fn union_all<'a>(polys: impl IntoIterator<Item = &'a geo::Polygon<f64>>) -> MultiPolygon<f64> {
    let oriented: Vec<geo::Polygon<f64>> = polys.into_iter().map(|p| p.orient(Direction::Default)).collect();
    geo::unary_union(oriented.iter())
}

// ---------------------------------------------------------------------------------------------
// tools

/// Per-feature outline complexity, and cumulative pre-union complexity for Dissolve All.
/// Two million coordinates occupy roughly 32 MB before containers and overlay scratch.
const MAX_BUFFER_VERTICES: usize = 2_000_000;
/// Retained ordinary output/selector storage, separate from overlay complexity. Temporary
/// geometry on each worker and the already loaded input are additional to this allowance. The
/// browser's budget, and the fewest bytes [`max_buffer_bytes`] allows on the desktop.
const MAX_BUFFER_BYTES: usize = if cfg!(target_arch = "wasm32") { 128 << 20 } else { 512 << 20 };
/// The most bytes [`max_buffer_bytes`] allows, however much memory the computer has.
const BUFFER_BYTES_CEILING: usize = 8 << 30;
/// Drain results regularly instead of keeping a second whole-layer result array.
const BUFFER_BATCH: usize = 256;
/// Bound actual candidate checks across all targets for large distance-selector sets, with
/// or without a grid. Small sets keep their bounded-width linear scan.
const MAX_SELECTOR_CHECKS: usize = 50_000_000;
const BUFFER_STORAGE_ERROR: &str = "Buffer output or distance selectors exceed their memory budget; select fewer features";

/// Buffer's retained-storage budget on this computer: on the desktop a quarter of its memory, from
/// [`MAX_BUFFER_BYTES`] up to [`BUFFER_BYTES_CEILING`] (#1291 step 2); fixed in the browser.
pub fn max_buffer_bytes() -> usize {
    rustgis_data::memory::scaled(rustgis_data::memory::physical(), MAX_BUFFER_BYTES, BUFFER_BYTES_CEILING, 1)
}

/// [`BUFFER_STORAGE_ERROR`] with the budget it ran out of, so a run refused here but not on a
/// larger computer says why.
fn with_budget(e: String, bytes: usize) -> String {
    if e == BUFFER_STORAGE_ERROR { format!("{e} (this computer's budget is {} MiB)", bytes >> 20) } else { e }
}

/// Shared retained-storage accounting; workers charge payloads before handing them back.
struct BufferStorage(std::sync::atomic::AtomicUsize);

impl BufferStorage {
    /// A fixed byte allowance, independent of the machine's reported free memory.
    fn new(bytes: usize) -> Self {
        Self(std::sync::atomic::AtomicUsize::new(bytes))
    }

    /// Reserve bytes atomically, without wrapping or retaining an over-budget result.
    fn charge(&self, bytes: usize) -> Result<(), &'static str> {
        use std::sync::atomic::Ordering;
        let mut left = self.0.load(Ordering::Relaxed);
        loop {
            let next = left.checked_sub(bytes).ok_or(BUFFER_STORAGE_ERROR)?;
            match self.0.compare_exchange_weak(left, next, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => return Ok(()),
                Err(current) => left = current,
            }
        }
    }

    /// Grow retained row storage geometrically, charging spare capacity as well as used rows.
    fn reserve<T>(&self, rows: &mut Vec<T>, additional: usize) -> Result<(), &'static str> {
        let needed = rows.len().checked_add(additional).ok_or(BUFFER_STORAGE_ERROR)?;
        if needed <= rows.capacity() {
            return Ok(());
        }
        let size = std::mem::size_of::<T>().max(1);
        let old = rows.capacity();
        let affordable = old.saturating_add(self.0.load(std::sync::atomic::Ordering::Relaxed) / size);
        let grown = needed.max(old.saturating_mul(2)).min(affordable);
        if grown < needed {
            return Err(BUFFER_STORAGE_ERROR);
        }
        self.charge((grown - old).saturating_mul(size))?;
        rows.try_reserve_exact(grown - rows.len()).map_err(|_| BUFFER_STORAGE_ERROR)?;
        // Allocators may give more than requested. Account for that capacity too.
        self.charge(rows.capacity().saturating_sub(grown).saturating_mul(size))
    }
}

/// Ordinary per-feature Buffer: bounded result batches, a per-feature complexity limit and
/// one byte budget for row capacity, geometry allocations and copied attributes together.
fn buffer_rows(
    geoms: &[(&Feature, &Geometry)],
    metres: f64,
    vertices: usize,
    bytes: usize,
    fields: usize,
) -> Result<Vec<Feature>, &'static str> {
    let storage = BufferStorage::new(bytes);
    let mut output = Vec::new();
    crate::control::begin(geoms.len());
    for batch in geoms.chunks(BUFFER_BATCH) {
        let made = crate::parallel::try_map(
            batch,
            PARALLEL_FEATURES,
            || (),
            |_, _, (f, g)| -> Result<Option<Feature>, &'static str> {
                let Some(geometry) = geodesic_buffer_with_limit(g, metres, vertices)?.geometry else { return Ok(None) };
                // Charge the requested attribute storage before copying potentially large text.
                // Fallible allocation also makes an exhausted allocator an ordinary tool error.
                let requested = f.attrs.iter().take(fields).fold(
                    rustgis_data::source::chunk_memory::geometry_bytes(&geometry)
                        .saturating_add(fields.saturating_mul(std::mem::size_of::<Value>())),
                    |n, v| n.saturating_add(if let Value::Text(text) = v { text.len() } else { 0 }),
                );
                storage.charge(requested)?;
                let mut attrs = Vec::new();
                attrs.try_reserve_exact(fields).map_err(|_| BUFFER_STORAGE_ERROR)?;
                for value in f.attrs.iter().take(fields) {
                    attrs.push(try_clone_value(value).map_err(|_| BUFFER_STORAGE_ERROR)?);
                }
                attrs.resize(fields, Value::Null);
                let feature = Feature { id: 0, geometry: Some(geometry), attrs };
                let actual = rustgis_data::source::chunk_memory::feature_bytes(&feature);
                storage.charge(actual.saturating_sub(requested))?;
                Ok(Some(feature))
            },
        )?;
        for mut feature in made.into_iter().flatten() {
            storage.reserve(&mut output, 1)?;
            feature.id = (output.len() as u64).saturating_add(1);
            output.push(feature);
        }
    }
    Ok(output)
}

/// Buffer with the output budget shared by all selected input features.
fn buffer(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let bytes = max_buffer_bytes();
    buffer_with_limits(inp, p, MAX_BUFFER_VERTICES, bytes).map_err(|e| with_budget(e, bytes))
}

/// Independent complexity and retained-output budgets; injectable for small boundary tests.
fn buffer_with_limits(inp: &dyn Inputs, p: &Json, limit: usize, bytes: usize) -> Result<ToolResult, String> {
    let ds = input(inp, p, "input")?;
    let d = num(p, "distance").ok_or("Distance must be a number")?;
    let metres = to_metres(d, s(p, "units"))?;
    if metres.abs() > 5_000_000.0 {
        return Err("Distance is too large (limit 5000 km)".into());
    }
    let mut out = copy_schema(ds, out_name(p, format!("{}_Buffer", ds.name)), GeometryType::Polygon);
    let mut all: Vec<MultiPolygon<f64>> = Vec::new();
    let picked: Vec<&Feature> = rows(inp, p, "input", ds).into_iter().filter_map(|r| ds.features.get(r)).collect();
    let geoms: Vec<(&Feature, &Geometry)> = picked.iter().filter_map(|f| Some((*f, f.geometry.as_ref()?))).collect();
    let without = picked.len() - geoms.len();
    let empty = if s(p, "dissolve") != "All" {
        let features = buffer_rows(&geoms, metres, limit, bytes, out.fields.len())?;
        let empty = geoms.len() - features.len();
        out.features = features.into();
        empty
    } else {
        // Dissolve still accumulates outlines for one overlay, so retain its shared complexity
        // limit, including multipoint discs before union and the final union's coordinates.
        let buffers = geodesic_buffers_with_limit(&geoms.iter().map(|(_, g)| *g).collect::<Vec<_>>(), metres, limit)?;
        let empty = buffers.iter().filter(|g| g.is_none()).count();
        for geom in buffers.into_iter().flatten() {
            all.extend(to_multipolygon(&geom));
        }
        if !all.is_empty() {
            out.fields.clear();
            crate::control::check()?;
            // The union isn't counted: the buffers' count ends here rather than stand at its end.
            crate::control::begin(0);
            let u = union_all(all.iter().flat_map(|m| m.0.iter()));
            if u.coords_count() > limit {
                return Err(VERTEX_BUDGET_ERROR.into());
            }
            if let Some(geom) = from_multipolygon(&u) {
                out.push(Some(geom), vec![]);
            }
        }
        empty
    };
    let n = out.features.len();
    let units = if s(p, "units").is_empty() { "Kilometers" } else { s(p, "units") };
    let mut r = result(out, format!("Buffer: {n} features at {d} {units}"))?;
    if without > 0 {
        r.messages.push(format!("{without} of {} features lack geometry, so they have no buffer", picked.len()));
    }
    if empty > 0 {
        r.messages
            .push(format!("{empty} of {} features made no buffer at {d} {units} (nothing left, or no finite coordinates)", picked.len()));
    }
    Ok(r)
}

/// A feature as the overlay can take it: a feature of several polygons, which may overlap, unioned
/// into one; any other as it is, so a single polygon's edges stay exactly where they are.
fn dissolved(g: &Geometry) -> std::borrow::Cow<'_, Geometry> {
    match g {
        Geometry::Polygon(polys) if polys.len() > 1 => to_multipolygon(g)
            .and_then(|m| from_multipolygon(&union_all(m.0.iter())))
            .map_or(std::borrow::Cow::Borrowed(g), std::borrow::Cow::Owned),
        _ => std::borrow::Cow::Borrowed(g),
    }
}

/// The part of a feature inside `mask`, or outside it when `inside` is false. Inside, a polygon's
/// overlap with it, a line's stretches in it, the points in it or on its edge; outside, the rest.
/// `None` when nothing is left, or only a shared edge or corner.
///
/// The mask's polygons are in Web Mercator, oriented as `union_all` leaves them. The feature's may
/// wind either way (the reference corpus has a clockwise parcel), but must not overlap each other:
/// pass a feature of several polygons through [`dissolved`] first, once for all its masks.
fn part(g: &Geometry, mask: &MultiPolygon<f64>, inside: bool) -> Option<Geometry> {
    match g {
        Geometry::Polygon(_) => {
            to_multipolygon(g).and_then(|m| from_multipolygon(&if inside { m.intersection(mask) } else { m.difference(mask) }))
        }
        Geometry::Line(_) => to_multiline(g).and_then(|m| from_line(&mask.clip(&m, !inside))),
        Geometry::Point(_) | Geometry::MultiPoint(_) => {
            let pts: &[Coord] = match g {
                Geometry::Point(c) => std::slice::from_ref(c),
                Geometry::MultiPoint(v) => v,
                _ => &[],
            };
            let kept: Vec<Coord> = pts
                .iter()
                .copied()
                .filter(|c| {
                    let m = rustgis_geom::lonlat_to_merc(*c);
                    mask.intersects(&geo::Point::new(m.x, m.y)) == inside
                })
                .collect();
            match kept.len() {
                0 => None,
                1 => kept.first().map(|c| Geometry::Point(*c)),
                _ => Some(Geometry::MultiPoint(kept)),
            }
        }
    }
}

/// Clip each input feature to the union of only the selected clip polygons whose projected
/// extents meet it. Union candidates together so overlapping masks never duplicate output.
fn clip(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let ds = input(inp, p, "input")?;
    let clip_ds = input(inp, p, "clip")?;
    if clip_ds.geometry_type != GeometryType::Polygon {
        return Err(format!("'{}' is not a polygon layer", clip_ds.name));
    }
    let masks: Vec<_> = rows(inp, p, "clip", clip_ds)
        .into_iter()
        .filter_map(|r| clip_ds.features.get(r)?.geometry.as_ref().and_then(to_multipolygon))
        .map(|m| {
            let ext = merc_extent(&m);
            // Union even one polygon: collapsed rings have no filled area. Keep the original
            // bounds too: rounding an edge must not hide a line the boolean overlay clips to it.
            let mask = union_all(m.0.iter());
            let ext = ext.union(&merc_extent(&mask));
            (mask, ext)
        })
        .filter(|(m, _)| !m.0.is_empty())
        .collect();
    let index = ExtentIndex::new(masks.iter().map(|(_, ext)| *ext));
    // Single candidates reuse their normalized mask. Retain eight recent multi-mask unions
    // so alternating neighbourhoods can reuse work too; evict the least recently used.
    const MAX_CACHED_MASKS: usize = 8;
    let mut cached_masks: std::collections::VecDeque<(Vec<usize>, MultiPolygon<f64>)> = std::collections::VecDeque::new();
    let mut out = copy_schema(ds, out_name(p, format!("{}_Clip", ds.name)), ds.geometry_type);
    for r in rows(inp, p, "input", ds) {
        let Some(f) = ds.features.get(r) else { continue };
        let Some(g) = f.geometry.as_ref() else { continue };
        let ext = projected_extent(g);
        // i_overlay uses 29 integer bits about the domain centre. Allow several grid steps
        // for normalization and clipping: a line just outside an edge can snap onto it.
        // This widens only the candidate search; the boolean overlay still decides the result.
        let domain = index.total.union(&ext);
        let margin = domain.width().max(domain.height()) / f64::from(1_u32 << 27);
        let ext = ext.padded(0.0, margin);
        let near: Vec<_> = index.candidates(&ext).into_iter().filter(|i| masks.get(*i).is_some_and(|(_, e)| e.intersects(&ext))).collect();
        if near.is_empty() {
            continue;
        }
        let mask = if let [i] = near.as_slice() {
            let Some((m, _)) = masks.get(*i) else { continue };
            m
        } else {
            if let Some(pos) = cached_masks.iter().position(|(ids, _)| *ids == near) {
                if let Some(entry) = cached_masks.remove(pos) {
                    cached_masks.push_back(entry);
                }
            } else {
                if cached_masks.len() == MAX_CACHED_MASKS {
                    cached_masks.pop_front();
                }
                let mask = union_all(near.iter().filter_map(|i| masks.get(*i)).flat_map(|(m, _)| m.0.iter()));
                cached_masks.push_back((near, mask));
            }
            let Some((_, mask)) = cached_masks.back() else { continue };
            mask
        };
        if let Some(c) = part(&dissolved(g), mask, true) {
            out.push(Some(c), f.attrs.clone());
        }
    }
    let n = out.features.len();
    result(out, format!("Clip: {n} features kept"))
}

/// Erase: each input feature's part outside every erase polygon, as [`outside`] gives it (a point
/// on an edge, or a line along one, counts as inside and is erased); a feature no erase polygon
/// meets is kept as it is. Features left with nothing, or without geometry, are left out; the
/// input must have geometry.
fn erase(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let ds = input(inp, p, "input")?;
    if ds.geometry_type == GeometryType::None {
        return Err(format!("'{}' has no geometry", ds.name));
    }
    let (er, zones) = overlay_zones(inp, p, "erase")?;
    let index = ExtentIndex::new(zones.iter().map(|z| z.ext));
    let mut out = copy_schema(ds, out_name(p, format!("{}_Erase", ds.name)), ds.geometry_type);
    keep_source_crs(&mut out, ds, er);
    for r in rows(inp, p, "input", ds) {
        let Some(f) = ds.features.get(r) else { continue };
        if let Some(rest) = f.geometry.as_ref().and_then(|g| outside(g, &zones, &index)) {
            out.push_next(Some(rest), f.attrs.clone());
        }
    }
    let n = out.features.len();
    result(out, format!("Erase: {n} features remain"))
}

/// An overlay polygon: its row in the overlay layer, its extent and its polygons in Web Mercator,
/// oriented for the overlay, its parts unioned when it has several (as Clip's mask is).
struct Zone {
    row: usize,
    ext: Extent,
    poly: MultiPolygon<f64>,
}

/// A feature's polygons as a mask for the overlay: parts of one feature may overlap, which the
/// overlay can't take as they are, so several are unioned; one polygon is only oriented, so its
/// edges stay exactly where they are.
fn mask_of(poly: MultiPolygon<f64>) -> MultiPolygon<f64> {
    if poly.0.len() > 1 { union_all(poly.0.iter()) } else { poly.orient(Direction::Default) }
}

/// The polygon layer of an overlay tool's parameter `k` (`overlay`, `erase`, `update`, `split`),
/// which must be polygons, and its (selected) features with geometry as [`Zone`]s, in order.
fn overlay_zones<'a>(inp: &'a dyn Inputs, p: &Json, k: &str) -> Result<(&'a Dataset, Vec<Zone>), String> {
    let ov = input(inp, p, k)?;
    if ov.geometry_type != GeometryType::Polygon {
        return Err(format!("'{}' is not a polygon layer", ov.name));
    }
    let zones = rows(inp, p, k, ov)
        .into_iter()
        .filter_map(|row| {
            let g = ov.features.get(row)?.geometry.as_ref()?;
            Some(Zone { row, ext: g.extent(), poly: mask_of(to_multipolygon(g)?) })
        })
        .collect();
    Ok((ov, zones))
}

/// An overlay tool's output layer: the input's fields, then the overlay's, an overlay field whose
/// name is taken getting a suffix (`name_2`). It keeps the input's source CRS for export.
fn overlay_schema(ds: &Dataset, ov: &Dataset, name: String, g: GeometryType) -> Dataset {
    let mut out = copy_schema(ds, name, g);
    keep_source_crs(&mut out, ds, ov);
    for f in &ov.fields {
        let name = unique_field(&out, &f.name);
        out.fields.push(Field { name, ..f.clone() });
    }
    out
}

/// A row of an overlay tool's output: the input feature's values, then the overlay feature's, each
/// padded to its layer's fields (all empty for a side with no feature).
fn overlay_row(ds: &Dataset, a: Option<&[Value]>, ov: &Dataset, b: Option<&[Value]>) -> Vec<Value> {
    let mut row = Vec::with_capacity(ds.fields.len() + ov.fields.len());
    for (values, n) in [(a, ds.fields.len()), (b, ov.fields.len())] {
        row.extend((0..n).map(|i| values.and_then(|v| v.get(i)).cloned().unwrap_or(Value::Null)));
    }
    row
}

/// What an overlay tool keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Overlay {
    /// The parts of input features inside overlay polygons.
    Intersect,
    /// Those, and the parts of input features outside every overlay polygon.
    Identity,
    /// Those, and the parts of overlay polygons outside every input polygon.
    Union,
    /// Union's parts that only one layer covers: the input's outside every overlay polygon, the
    /// overlay's outside every input polygon.
    SymmetricalDifference,
}

impl Overlay {
    /// The tool's name, for its messages.
    fn name(self) -> &'static str {
        match self {
            Overlay::Intersect => "Intersect",
            Overlay::Identity => "Identity",
            Overlay::Union => "Union",
            Overlay::SymmetricalDifference => "Symmetrical Difference",
        }
    }

    /// The end of its output's default name, after the input's and an underscore.
    fn suffix(self) -> &'static str {
        match self {
            Overlay::SymmetricalDifference => "SymDiff",
            op => op.name(),
        }
    }

    /// Whether it keeps input features' parts inside overlay polygons.
    fn keeps_inside(self) -> bool {
        self != Overlay::SymmetricalDifference
    }

    /// Whether it keeps input features' parts outside every overlay polygon, and input features
    /// without geometry.
    fn keeps_outside(self) -> bool {
        self != Overlay::Intersect
    }

    /// Whether it adds overlay polygons' parts outside every input polygon; it then needs an input
    /// of polygons.
    fn keeps_overlay_rest(self) -> bool {
        matches!(self, Overlay::Union | Overlay::SymmetricalDifference)
    }
}

/// A feature cut by overlay zones: its part in each zone it overlaps, with the zone's index, in
/// zone order; and, when `outside` is set, its part outside them all. Parts that only touch are
/// left out. A feature no zone overlaps is its own part outside, coordinates untouched; one that
/// is cut comes out rebuilt, its overlapping parts unioned (see [`dissolved`]).
fn cut(g: &Geometry, zones: &[Zone], index: &ExtentIndex, outside: bool) -> (Vec<(usize, Geometry)>, Option<Geometry>) {
    let ext = g.extent();
    let meets: Vec<usize> =
        index.candidates(&ext).into_iter().filter(|zi| zones.get(*zi).is_some_and(|z| ext.intersects(&z.ext))).collect();
    // A feature of overlapping parts is unioned once, for all the zones it may meet, and only
    // when it may meet one.
    let whole = if meets.is_empty() { std::borrow::Cow::Borrowed(g) } else { dissolved(g) };
    let inside: Vec<(usize, Geometry)> = meets.into_iter().filter_map(|zi| Some((zi, part(&whole, &zones.get(zi)?.poly, true)?))).collect();
    if !outside {
        return (inside, None);
    }
    let hit: Vec<&Zone> = inside.iter().filter_map(|(zi, _)| zones.get(*zi)).collect();
    let rest = rest_outside(g, &whole, &hit);
    (inside, rest)
}

/// A feature's part outside the zones that cut it, for [`cut`] and [`outside`], which find those
/// zones differently: `cut` makes each part inside anyway, so a zone cuts when that part is
/// something; `outside` makes none, so it tests each zone with a predicate. `whole` is the feature
/// as [`dissolved`] gives it. With no zone, the feature as it is, coordinates untouched; with one,
/// cut by that zone itself (a union would move its edges by a rounding error, and a line along one
/// could come out on both sides of it); with several, by their union.
fn rest_outside(g: &Geometry, whole: &Geometry, hit: &[&Zone]) -> Option<Geometry> {
    match hit {
        [] => Some(g.clone()),
        [only] => part(whole, &only.poly, false),
        _ => part(whole, &union_all(hit.iter().flat_map(|z| z.poly.0.iter())), false),
    }
}

/// A feature's part outside every zone, as [`part`] gives it, for tools that keep only that
/// (Erase, Update): it is cut once, by the zones that cut it (that zone itself, so its edges stay
/// exactly where they are, or their union), without [`cut`]'s part in each zone. Only zones whose
/// extent meets the feature's are tested. A feature no zone cuts is kept as it is, coordinates
/// untouched: a polygon that at most touches them, a line or point that doesn't meet them.
fn outside(g: &Geometry, zones: &[Zone], index: &ExtentIndex) -> Option<Geometry> {
    let ext = g.extent();
    let near: Vec<&Zone> = index.candidates(&ext).into_iter().filter_map(|zi| zones.get(zi)).filter(|z| ext.intersects(&z.ext)).collect();
    if near.is_empty() {
        return Some(g.clone());
    }
    // The feature as the cut takes it: overlapping parts unioned, for the tests and the cut alike.
    let whole = dissolved(g);
    let geo_g = to_geo(&whole);
    // The zones that cut it: a polygon is cut by those it overlaps, a line or point by those it
    // meets (an edge counts as inside). Each is tested on its own, before any union.
    let cuts = |z: &&Zone| match g {
        Geometry::Polygon(_) => {
            let m = z.poly.relate(&geo_g);
            m.is_intersects() && !m.is_touches()
        }
        _ => z.poly.intersects(&geo_g),
    };
    let hit: Vec<&Zone> = near.into_iter().filter(cuts).collect();
    rest_outside(g, &whole, &hit)
}

/// Intersect, Identity, Union and Symmetrical Difference. Each input feature in turn gives its
/// part in each overlay polygon, in overlay order, with the attributes of both (all but Symmetrical
/// Difference); then (all but Intersect) its part outside them all, with empty overlay attributes.
/// Union and Symmetrical Difference then add each overlay polygon's part outside every input
/// polygon, with empty input attributes. Parts that only touch are left out, and features of one
/// layer that overlap each other are not split against each other. Intersect leaves out input
/// features without geometry; the others keep them, as they are, and an input feature no overlay
/// polygon meets too, coordinates untouched (a feature that is cut comes out rebuilt, its
/// overlapping parts unioned).
fn overlay(inp: &dyn Inputs, p: &Json, op: Overlay) -> Result<ToolResult, String> {
    let ds = input(inp, p, "input")?;
    if ds.geometry_type == GeometryType::None {
        return Err(format!("'{}' has no geometry", ds.name));
    }
    if op.keeps_overlay_rest() && ds.geometry_type != GeometryType::Polygon {
        return Err(format!("'{}' is not a polygon layer", ds.name));
    }
    let (ov, zones) = overlay_zones(inp, p, "overlay")?;
    let index = ExtentIndex::new(zones.iter().map(|z| z.ext));
    let mut out = overlay_schema(ds, ov, out_name(p, format!("{}_{}", ds.name, op.suffix())), ds.geometry_type);
    let zone_attrs = |z: &Zone| ov.features.get(z.row).map(|o| o.attrs.as_slice());
    // For Union and Symmetrical Difference, the rows of the input polygons that overlap each zone.
    let mut covering: Vec<Vec<usize>> = vec![Vec::new(); zones.len()];
    for r in rows(inp, p, "input", ds) {
        let Some(f) = ds.features.get(r) else { continue };
        let Some(g) = &f.geometry else {
            // All but Intersect keep every input feature: one without geometry, as it is.
            if op.keeps_outside() {
                out.push_next(None, overlay_row(ds, Some(&f.attrs), ov, None));
            }
            continue;
        };
        let (inside, rest) = cut(g, &zones, &index, op.keeps_outside());
        for (zi, piece) in inside {
            if op.keeps_inside() {
                out.push_next(Some(piece), overlay_row(ds, Some(&f.attrs), ov, zones.get(zi).and_then(zone_attrs)));
            }
            if op.keeps_overlay_rest()
                && let Some(c) = covering.get_mut(zi)
            {
                c.push(r);
            }
        }
        if let Some(rest) = rest {
            out.push_next(Some(rest), overlay_row(ds, Some(&f.attrs), ov, None));
        }
    }
    if op.keeps_overlay_rest() {
        for (z, inputs) in zones.iter().zip(&covering) {
            let rest = if inputs.is_empty() {
                from_multipolygon(&z.poly)
            } else {
                let polys: Vec<MultiPolygon<f64>> =
                    inputs.iter().filter_map(|r| ds.features.get(*r)?.geometry.as_ref()).filter_map(to_multipolygon).collect();
                from_multipolygon(&z.poly.difference(&union_all(polys.iter().flat_map(|m| m.0.iter()))))
            };
            if let Some(rest) = rest {
                out.push_next(Some(rest), overlay_row(ds, None, ov, zone_attrs(z)));
            }
        }
    }
    let n = out.features.len();
    result(out, format!("{}: {n} features", op.name()))
}

/// Intersect: see [`overlay`].
fn intersect(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    overlay(inp, p, Overlay::Intersect)
}

/// Identity: see [`overlay`].
fn identity(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    overlay(inp, p, Overlay::Identity)
}

/// Union: see [`overlay`].
fn union(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    overlay(inp, p, Overlay::Union)
}

/// Symmetrical Difference: see [`overlay`].
fn symmetrical_difference(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    overlay(inp, p, Overlay::SymmetricalDifference)
}

/// A value as a field of type `ty` holds it: unchanged when it already fits; else converted as
/// typing it into the field would, so text that isn't a number becomes empty in a numeric field.
/// A number becomes text at full precision, and a fractional one an integer by truncation; one
/// beyond an integer field's range, or not finite, becomes empty. Text becomes a Boolean as typing
/// it would: true for true, yes, 1 or y, else false.
fn converted(v: &Value, ty: FieldType) -> Value {
    match (v, ty) {
        (Value::Int(_), FieldType::Integer)
        | (Value::Float(_), FieldType::Double)
        | (Value::Text(_), FieldType::Text)
        | (Value::Bool(_), FieldType::Boolean) => v.clone(),
        (Value::Null, _) => Value::Null,
        (Value::Int(i), FieldType::Double) => Value::Float(*i as f64),
        // In range, truncated; out of range (or not a number), empty rather than clamped. i64's
        // bounds as f64 are exactly -2⁶³ and 2⁶³.
        (Value::Float(f), FieldType::Integer) if (i64::MIN as f64..i64::MAX as f64).contains(&f.trunc()) => Value::Int(f.trunc() as i64),
        (Value::Float(_), FieldType::Integer) => Value::Null,
        (Value::Float(f), FieldType::Text) => Value::Text(f.to_string()),
        (Value::Bool(b), FieldType::Integer) => Value::Int(i64::from(*b)),
        (Value::Bool(b), FieldType::Double) => Value::Float(if *b { 1.0 } else { 0.0 }),
        // Text that reads as an integer is one; text that reads as another number goes as that
        // float would, so one out of range (or not a number) is empty, not clamped.
        (Value::Text(t), FieldType::Integer) => match t.trim().parse::<i64>() {
            Ok(i) => Value::Int(i),
            Err(_) => t.trim().parse::<f64>().map_or(Value::Null, |f| converted(&Value::Float(f), ty)),
        },
        // Text that reads as a finite number goes into a Double field; "NaN", "inf" or "1e400" is
        // empty, as it is in an integer field.
        (Value::Text(t), FieldType::Double) => t.trim().parse::<f64>().ok().filter(|f| f.is_finite()).map_or(Value::Null, Value::Float),
        (Value::Text(t), _) => Value::parse(t, ty),
        (v, _) => Value::parse(&v.to_string(), ty),
    }
}

/// Update: each input polygon's part outside every update polygon, with its attributes, then each
/// update polygon as it is, its values copied into the input's fields of the same name (matched
/// exactly, else ignoring case; converted by [`converted`]), the input's other fields empty. The
/// output has the input's fields only. Input features without geometry, and those no update
/// polygon overlaps, are kept as they are; update features without geometry are left out, and
/// update polygons that overlap each other each come out whole. An input field that no update
/// field matches exactly, but two match ignoring case, is an error rather than a guess.
fn update(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let ds = input(inp, p, "input")?;
    if ds.geometry_type != GeometryType::Polygon {
        return Err(format!("'{}' is not a polygon layer", ds.name));
    }
    let (up, zones) = overlay_zones(inp, p, "update")?;
    // Each input field's field in the update layer, if it has one: an ambiguous one fails here,
    // before any geometry work.
    let from = ds
        .fields
        .iter()
        .map(|f| {
            if up.fields.iter().any(|u| u.name.eq_ignore_ascii_case(&f.name)) {
                find_field_case_insensitive(up, &f.name).map(Some)
            } else {
                Ok(None)
            }
        })
        .collect::<Result<Vec<Option<usize>>, String>>()?;
    let index = ExtentIndex::new(zones.iter().map(|z| z.ext));
    let mut out = copy_schema(ds, out_name(p, format!("{}_Update", ds.name)), GeometryType::Polygon);
    keep_source_crs(&mut out, ds, up);
    for r in rows(inp, p, "input", ds) {
        let Some(f) = ds.features.get(r) else { continue };
        let rest = match &f.geometry {
            None => {
                out.push_next(None, f.attrs.clone());
                continue;
            }
            Some(g) => outside(g, &zones, &index),
        };
        if let Some(rest) = rest {
            out.push_next(Some(rest), f.attrs.clone());
        }
    }
    let kept = out.features.len();
    for z in &zones {
        let Some(u) = up.features.get(z.row) else { continue };
        let attrs =
            ds.fields.iter().zip(&from).map(|(f, i)| i.and_then(|i| u.attrs.get(i)).map_or(Value::Null, |v| converted(v, f.ty))).collect();
        out.push_next(u.geometry.clone(), attrs);
    }
    let added = out.features.len() - kept;
    result(out, format!("Update: {kept} input features kept, {added} update features added"))
}

/// The most split areas (split polygons, or values of the split field) Split takes: more is an
/// error before anything is clipped, not thousands of layers. Areas that come out empty count too.
const MAX_SPLIT_AREAS: usize = 1000;
/// The most characters of a split value kept in a layer's name.
const MAX_SPLIT_NAME: usize = 64;

/// A split value as it goes into a layer's name: control characters dropped, then its first
/// [`MAX_SPLIT_NAME`] characters, trimmed.
fn split_name(value: &str) -> String {
    let name: String = value.chars().filter(|c| !c.is_control()).take(MAX_SPLIT_NAME).collect();
    name.trim().to_string()
}

/// A split value exactly, for grouping: a float as the shortest text that reads back as it (its
/// displayed text keeps six decimals, so 0.0000012 and 0.0000014 would read alike); any other
/// value as it displays.
fn split_key(v: &Value) -> String {
    match v {
        Value::Float(f) => format!("{f:?}"),
        v => v.to_string(),
    }
}

/// Split: the input clipped (as Clip clips) by each split polygon, one layer each, named by the
/// output name (else the input's), an underscore and the polygon's id. With a split field, split
/// polygons sharing its value make one layer together, named by the value (its first
/// [`MAX_SPLIT_NAME`] characters, control characters dropped); one with no value is named by its
/// id, as is one whose value is blank once its control characters are dropped. A name the map
/// already has gets a number when the layer is added. Layers left with no features are left out;
/// selections in both layers count.
fn split(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let ds = input(inp, p, "input")?;
    if ds.geometry_type == GeometryType::None {
        return Err(format!("'{}' has no geometry", ds.name));
    }
    let (sp, zones) = overlay_zones(inp, p, "split")?;
    // A field name, or none; anything else is an error, not one layer per polygon.
    let field = match p.get("field") {
        None | Some(Json::Null) => None,
        Some(Json::String(name)) if name.trim().is_empty() => None,
        Some(Json::String(name)) => Some(find_field_case_insensitive(sp, name.trim())?),
        Some(other) => return Err(format!("Split Field must be a field name, not {other}")),
    };
    // The split polygons of each layer, in the order their first polygon comes, keyed by the exact
    // value, so values that would be named alike stay apart. A layer named by a value and one named
    // by an id are kept apart even when the two read the same.
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    let mut by_key: HashMap<(bool, String), usize> = HashMap::new();
    for (zi, z) in zones.iter().enumerate() {
        let Some(f) = sp.features.get(z.row) else { continue };
        let value = field.and_then(|i| f.attrs.get(i)).filter(|v| !v.is_null());
        // -0 is 0, in the key and the name alike.
        let value = value.map(|v| if let Value::Float(f) = v { Value::Float(f + 0.0) } else { v.clone() });
        let named = value.map(|v| (split_key(&v), split_name(&v.to_string()))).filter(|(_, name)| !name.is_empty());
        let (key, name) = match named {
            Some((key, name)) => ((true, key), name),
            None => ((false, f.id.to_string()), f.id.to_string()),
        };
        if let Some(g) = by_key.get(&key).and_then(|g| groups.get_mut(*g)) {
            g.1.push(zi);
        } else {
            if groups.len() >= MAX_SPLIT_AREAS {
                return Err(format!("Split takes at most {MAX_SPLIT_AREAS} split areas"));
            }
            by_key.insert(key, groups.len());
            groups.push((name, vec![zi]));
        }
    }
    let prefix = out_name(p, ds.name.clone());
    let located: Vec<(usize, Extent)> =
        rows(inp, p, "input", ds).into_iter().filter_map(|r| Some((r, ds.features.get(r)?.geometry.as_ref()?.extent()))).collect();
    let index = ExtentIndex::new(located.iter().map(|(_, e)| *e));
    // Each input feature as the overlay takes it, made once, when a split polygon first meets it.
    let mut whole: Vec<Option<std::borrow::Cow<'_, Geometry>>> = vec![None; located.len()];
    let mut layers = Vec::new();
    let mut empty = 0usize;
    let mut cand_buf = Vec::new();
    for (name, members) in groups {
        let polys: Vec<&Zone> = members.iter().filter_map(|zi| zones.get(*zi)).collect();
        let mask = match polys.as_slice() {
            [only] => std::borrow::Cow::Borrowed(&only.poly),
            _ => std::borrow::Cow::Owned(union_all(polys.iter().flat_map(|z| z.poly.0.iter()))),
        };
        let ext = polys.iter().fold(Extent::EMPTY, |e, z| e.union(&z.ext));
        let mut out = copy_schema(ds, format!("{prefix}_{name}"), ds.geometry_type);
        keep_source_crs(&mut out, ds, sp);
        index.candidates_into(&ext, &mut cand_buf);
        for &li in &cand_buf {
            let Some((r, e)) = located.get(li) else { continue };
            let Some(f) = ds.features.get(*r) else { continue };
            let (Some(g), Some(slot)) = (f.geometry.as_ref().filter(|_| e.intersects(&ext)), whole.get_mut(li)) else { continue };
            if let Some(c) = part(slot.get_or_insert_with(|| dissolved(g)), &mask, true) {
                out.push_next(Some(c), f.attrs.clone());
            }
        }
        if out.features.is_empty() {
            empty += 1;
        } else {
            layers.push(OutputLayer::Vector(out));
        }
    }
    let mut msg = format!("Split: {} layers", layers.len());
    if empty > 0 {
        let areas = if empty == 1 { "split area" } else { "split areas" };
        msg = format!("{msg} ({empty} {areas} held no features)");
    }
    Ok(ToolResult { output: Output::Layers(layers), messages: vec![msg] })
}

/// Dissolve: one feature for each value of the dissolve field (one for all without it), the union
/// of the (selected) polygons holding it, with the value, `count` (the polygons merged) and the
/// statistics asked for, as `<STAT>_<field>`, over exactly the records merged: a record without a
/// polygon is left out of both.
fn dissolve(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let ds = input(inp, p, "input")?;
    if ds.geometry_type != GeometryType::Polygon {
        return Err("Dissolve needs a polygon layer".into());
    }
    let field = s(p, "field").trim();
    let fi = if field.is_empty() { None } else { Some(find_field_case_insensitive(ds, field)?) };
    let (requests, separator) = summary_requests(ds, p)?;
    // Each group's key, polygons and records.
    let mut groups: Vec<(Value, Vec<MultiPolygon<f64>>, Vec<usize>)> = Vec::new();
    for r in each_once(rows(inp, p, "input", ds)) {
        let Some(mp) = ds.features.get(r).and_then(|f| f.geometry.as_ref()).and_then(to_multipolygon) else { continue };
        let key = fi.map(|fi| ds.value(r, fi).clone()).unwrap_or(Value::Null);
        match groups.iter_mut().find(|(k, _, _)| *k == key) {
            Some(g) => {
                g.1.push(mp);
                g.2.push(r);
            }
            None => groups.push((key, vec![mp], vec![r])),
        }
    }
    let mut fields = match fi.and_then(|i| ds.fields.get(i)) {
        Some(f) => vec![f.clone(), Field::new("count", FieldType::Integer)],
        None => vec![Field::new("count", FieldType::Integer)],
    };
    let added = statistic_fields(ds, &requests, false, &fields)?;
    let first_added = fields.len();
    fields.extend(added);
    let mut out = Dataset::new(out_name(p, format!("{}_Dissolve", ds.name)), GeometryType::Polygon, fields);
    let mut rounded = Rounded::new(requests.len());
    for (key, parts, mut records) in groups {
        let n = parts.len() as i64;
        let u = union_all(parts.iter().flat_map(|m| m.0.iter()));
        let mut attrs = if fi.is_some() { vec![key, Value::Int(n)] } else { vec![Value::Int(n)] };
        if !requests.is_empty() {
            record_order(&mut records);
            // Each record is in one group: counted here, each is counted once.
            rounded.count_texts(ds, &records, &requests, false);
            attrs.extend(statistic_values(ds, &records, &requests, false, separator, &mut rounded));
        }
        out.push(from_multipolygon(&u), attrs);
    }
    let n = out.features.len();
    let mut messages = vec![format!("Dissolve: {n} features")];
    messages.extend(rounded_notice(&rounded, out.fields.get(first_added..).unwrap_or_default()));
    Ok(ToolResult { output: Output::Layer(out), messages })
}

fn convex_hull(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let ds = input(inp, p, "input")?;
    let each = s(p, "group") == "None";
    let mut out = if each {
        copy_schema(ds, out_name(p, format!("{}_Hull", ds.name)), GeometryType::Polygon)
    } else {
        Dataset::new(out_name(p, format!("{}_Hull", ds.name)), GeometryType::Polygon, vec![Field::new("count", FieldType::Integer)])
    };
    let mut pts: Vec<geo::Point<f64>> = Vec::new();
    for r in rows(inp, p, "input", ds) {
        let Some(f) = ds.features.get(r) else { continue };
        let Some(g) = &f.geometry else { continue };
        let mut these = Vec::new();
        g.for_each_coord(|c| {
            let m = rustgis_geom::lonlat_to_merc(c);
            these.push(geo::Point::new(m.x, m.y));
        });
        if each {
            let hull = geo::MultiPoint::new(these).convex_hull();
            out.push(from_multipolygon(&MultiPolygon::new(vec![hull])), f.attrs.clone());
        } else {
            pts.extend(these);
        }
    }
    if !each {
        let n = ds.features.len() as i64;
        let hull = geo::MultiPoint::new(pts).convex_hull();
        out.push(from_multipolygon(&MultiPolygon::new(vec![hull])), vec![Value::Int(n)]);
    }
    let n = out.features.len();
    result(out, format!("Minimum Bounding Geometry: {n} hulls"))
}

/// The point Feature To Point gives for a geometry: inside a polygon, else its label point.
fn interior_point(g: &Geometry) -> Option<Geometry> {
    match g {
        Geometry::Polygon(_) => to_multipolygon(g).and_then(|m| m.interior_point()).map(|p| from_point(&p)),
        g => g.label_point().map(Geometry::Point),
    }
}

fn feature_to_point(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let ds = input(inp, p, "input")?;
    let mut out = copy_schema(ds, out_name(p, format!("{}_Points", ds.name)), GeometryType::Point);
    for r in rows(inp, p, "input", ds) {
        let Some(f) = ds.features.get(r) else { continue };
        out.push(f.geometry.as_ref().and_then(interior_point), f.attrs.clone());
    }
    let n = out.features.len();
    result(out, format!("Feature To Point: {n} points"))
}

/// The fields Feature Vertices To Points adds: a point's feature's OID, linking the two as
/// ArcGIS's tool of the same name does, and where in the feature the vertex lies, named as QGIS's
/// Extract Vertices names them.
const VERTEX_FIELDS: [&str; 4] = ["orig_fid", "vertex_part", "vertex_part_ring", "vertex_part_index"];

/// An allocation's overhead in the allocator, as estimated: its header and rounding.
const ALLOCATION_OVERHEAD: u64 = 16;

/// The memory a point a tool writes takes once drawn, copying `own` into `width` attributes: the
/// row, its values, its copy of `own`'s text, each allocation's overhead, and the map's cache of a
/// drawn feature (its projected geometry and extent).
fn point_bytes(own: &[Value], width: usize) -> u64 {
    let u = |x: usize| u64::try_from(x).unwrap_or(u64::MAX);
    let (mut text, mut allocations) = (0u64, 1u64);
    for v in own {
        if let Value::Text(t) = v
            && !t.is_empty()
        {
            text += u(t.len());
            allocations += 1;
        }
    }
    let fixed = size_of::<rustgis_data::Feature>() + size_of::<Option<Geometry>>() + size_of::<Extent>();
    u(fixed)
        .saturating_add(u(width).saturating_mul(u(size_of::<Value>())))
        .saturating_add(text)
        .saturating_add(allocations.saturating_mul(ALLOCATION_OVERHEAD))
}

/// The points Feature Vertices To Points writes for a geometry: one a vertex.
fn vertex_count(g: &Geometry) -> usize {
    let mut k = 0usize;
    g.for_each_vertex(|_, _| k += 1);
    k
}

/// How many points a tool writes for `feats`, `count` of them for a feature's geometry, and, when
/// `estimate`, the memory they take (see [`point_bytes`]), each point copying `kept` of its
/// feature's attributes into `width`. The memory is `None` when it isn't estimated. Counting stops
/// once the memory is over `limit`, which the counts then are too, however many more follow.
fn size_output_points<'a>(
    feats: impl IntoIterator<Item = &'a rustgis_data::Feature>,
    kept: usize,
    width: usize,
    estimate: bool,
    limit: u64,
    mut count: impl FnMut(&Geometry) -> usize,
) -> (usize, Option<u64>) {
    let (mut n, mut need) = (0usize, 0u64);
    for f in feats {
        let Some(g) = &f.geometry else { continue };
        let k = count(g);
        // Saturating, as sizes from input are. A row repeated in the selection (a project can
        // restore one) is counted again, and its points are written twice.
        n = n.saturating_add(k);
        if estimate {
            let own = f.attrs.get(..kept).unwrap_or(&f.attrs);
            let k = u64::try_from(k).unwrap_or(u64::MAX);
            need = need.saturating_add(k.saturating_mul(point_bytes(own, width)));
            if need > limit {
                break;
            }
        }
    }
    (n, estimate.then_some(need))
}

/// Whether the browser can give the web app `need` bytes now, and a quarter more as a margin for
/// what the estimate leaves out, such as the renderer's buffers. The allocator is asked for them
/// in one block, freed at once, so the free space inside memory counts. On wasm32 a single block
/// can't exceed `isize::MAX` (2 GiB), so the probe can't confirm more than that: a larger estimate
/// is refused even where the browser could give more. Elsewhere the system manages memory, so this
/// is `true`.
fn web_has_room(need: u64) -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        let Ok(want) = usize::try_from(need.saturating_add(need / 4)) else { return false };
        let mut probe = Vec::<u8>::new();
        let room = probe.try_reserve_exact(want).is_ok();
        // Kept in sight of the optimizer, which could otherwise drop an allocation never used.
        std::hint::black_box(&probe);
        room
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = need;
        true
    }
}

/// Refuses `tool`'s `n` points needing `need` bytes when `has_room` says the memory isn't there,
/// with `advice` on making fewer.
fn check_room(tool: &str, n: usize, need: u64, advice: &str, has_room: impl FnOnce(u64) -> bool) -> Result<(), String> {
    if has_room(need) {
        return Ok(());
    }
    Err(format!("{tool}: {n} points would need about {} MiB, more than the browser can give now: {advice}", need.div_ceil(1 << 20)))
}

/// A copy of `v` whose allocation (a text's) fails into an error rather than aborting the process.
/// The variants are listed, so one that owns memory can't be added without deciding how.
pub(crate) fn try_clone_value(v: &Value) -> Result<Value, TryReserveError> {
    Ok(match v {
        Value::Text(t) => {
            let mut s = String::new();
            s.try_reserve_exact(t.len())?;
            s.push_str(t);
            Value::Text(s)
        }
        Value::Null | Value::Int(_) | Value::Float(_) | Value::Bool(_) => v.clone(),
    })
}

/// A point's attributes: its feature's `own` (padded to `kept`), then `extra`, each allocation
/// fallible.
fn point_row<const N: usize>(own: &[Value], kept: usize, extra: [Value; N]) -> Result<Vec<Value>, TryReserveError> {
    let mut attrs = Vec::new();
    attrs.try_reserve_exact(kept.saturating_add(extra.len()))?;
    for v in own {
        attrs.push(try_clone_value(v)?);
    }
    attrs.resize(kept, Value::Null);
    attrs.extend(extra);
    Ok(attrs)
}

/// The points a tool writes, allocated once for the `cap` it counted and filled without growing:
/// growing the list would copy it and briefly hold it twice. Each point's attributes are allocated
/// fallibly too, so running out of memory stops the tool with a message. (A desktop system that
/// overcommits memory may instead close the app when it runs out.)
struct PointList {
    points: Vec<rustgis_data::Feature>,
    cap: usize,
    stopped: Option<Stopped>,
}

/// Why a [`PointList`] stopped taking points.
#[derive(Debug)]
enum Stopped {
    /// The memory ran out.
    Memory(TryReserveError),
    /// A point came past the count it was allocated for: the count was wrong, and the points
    /// can't be all there.
    Uncounted,
}

impl Stopped {
    /// What a tool that counted `n` points says.
    fn message(self, tool: &str, n: usize) -> String {
        match self {
            Stopped::Memory(e) => format!("{tool}: not enough memory for {n} points: {e}"),
            Stopped::Uncounted => {
                format!("{tool}: made more than the {n} points it counted, so stopped rather than leave some out (please report this)")
            }
        }
    }
}

impl PointList {
    fn new(cap: usize) -> Result<Self, TryReserveError> {
        let mut points = Vec::new();
        points.try_reserve_exact(cap)?;
        Ok(PointList { points, cap, stopped: None })
    }

    /// Adds a point at `c` with its feature's `own` attributes (padded to `kept`), then `extra`,
    /// numbered from 1. Breaks, for good, once the memory has run out or a point comes past `cap`.
    fn push<const N: usize>(&mut self, c: Coord, own: &[Value], kept: usize, extra: [Value; N]) -> ControlFlow<()> {
        if self.stopped() {
            return ControlFlow::Break(());
        }
        if self.points.len() == self.cap {
            self.stopped = Some(Stopped::Uncounted);
            return ControlFlow::Break(());
        }
        match point_row(own, kept, extra) {
            Ok(attrs) => {
                let id = u64::try_from(self.points.len()).map_or(u64::MAX, |i| i.saturating_add(1));
                self.points.push(rustgis_data::Feature { id, geometry: Some(Geometry::Point(c)), attrs });
                ControlFlow::Continue(())
            }
            Err(e) => {
                self.stopped = Some(Stopped::Memory(e));
                ControlFlow::Break(())
            }
        }
    }

    /// Whether it has stopped taking points.
    fn stopped(&self) -> bool {
        self.stopped.is_some()
    }

    /// The points, or what stopped them (the points freed, so that there is memory to say so). A
    /// count from a bound can leave much of the list unused (a point or two a part): over a
    /// sixteenth of it, the points move into a list of their own size, when there's memory for one,
    /// so the layer doesn't keep the room for as long as it lives.
    fn finish(self) -> Result<Vec<rustgis_data::Feature>, Stopped> {
        if let Some(e) = self.stopped {
            return Err(e);
        }
        let mut points = self.points;
        if points.capacity() - points.len() > points.len() / 16 {
            let mut fitted = Vec::new();
            if fitted.try_reserve_exact(points.len()).is_ok() {
                fitted.append(&mut points);
                return Ok(fitted);
            }
        }
        Ok(points)
    }
}

/// A point at each vertex, with its feature's attributes and OID and where it lies: the part, the
/// ring (a polygon's; 0 otherwise) and the place along it. A closed ring's repeated last point is
/// not a vertex of its own; a point layer's points are their own vertices.
fn feature_vertices_to_points(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let ds = input(inp, p, "input")?;
    if ds.geometry_type == GeometryType::None {
        return Err("Feature Vertices To Points needs a layer with geometry".into());
    }
    let mut out = copy_schema(ds, out_name(p, format!("{}_Vertices", ds.name)), GeometryType::Point);
    // What the input lost on reading (features, Z and M values), its points have lost too.
    (out.crs, out.crs_incomplete, out.zm_dropped) = (ds.crs, ds.crs_incomplete, ds.zm_dropped);
    // A name the input already has keeps its field, and the new one gets a suffix, as Intersect's
    // and Spatial Join's do.
    for name in VERTEX_FIELDS {
        let field = Field::new(unique_field(&out, name), FieldType::Integer);
        out.fields.push(field);
    }
    let (kept, width) = (ds.fields.len(), out.fields.len());
    let sel = inp.selection(s(p, "input"));
    let feats = || selected_features(sel, ds);
    // Counted first, so the points are allocated once at their final size: growing the list would
    // copy it and briefly hold it twice. In the browser, whose memory has a ceiling, the points'
    // memory is also estimated and asked for before anything is built.
    let web = cfg!(target_arch = "wasm32");
    let (n, need) = size_output_points(feats(), kept, width, web, u64::MAX, vertex_count);
    if let Some(need) = need {
        check_room("Feature Vertices To Points", n, need, "select fewer features, or simplify ones with many vertices", web_has_room)?;
    }
    // The point list and each point's attributes are allocated fallibly, so running out of memory
    // while they're built stops the tool with a message. (A desktop system that overcommits memory
    // may instead close the app when it runs out.)
    let no_memory = |e: TryReserveError| Stopped::Memory(e).message("Feature Vertices To Points", n);
    let mut list = PointList::new(n).map_err(no_memory)?;
    let int = |k: usize| Value::Int(i64::try_from(k).unwrap_or(i64::MAX));
    for f in feats() {
        if list.stopped() {
            break;
        }
        let Some(g) = &f.geometry else { continue };
        let own = f.attrs.get(..kept).unwrap_or(&f.attrs);
        // An id a reader wrapped from a negative key (a GeoPackage's) is that key again.
        let fid = Value::Int(f.id.cast_signed());
        // A multipoint's points are its parts, as QGIS numbers them.
        let multipoint = matches!(g, Geometry::MultiPoint(_));
        g.for_each_vertex(|v, c| {
            let (part, index) = if multipoint { (v.index, 0) } else { (v.part, v.index) };
            let _ = list.push(c, own, kept, [fid.clone(), int(part), int(v.ring), int(index)]);
        });
    }
    out.features = list.finish().map_err(|e| e.message("Feature Vertices To Points", n))?.into();
    let written = out.features.len();
    result(out, format!("Feature Vertices To Points: {written} points"))
}

/// The fields Points Along Lines adds: a point's feature's OID, as Feature Vertices To Points
/// writes it, then how far along its part the point is and the line's direction there, named as
/// QGIS's Points along geometry names them.
const ALONG_FIELDS: [(&str, FieldType); 3] =
    [("orig_fid", FieldType::Integer), ("distance", FieldType::Double), ("angle", FieldType::Double)];

/// The most memory Points Along Lines' points may take, as estimated (about 20 million points with
/// a few fields). The distance alone sets how many there are, so one in the wrong units can ask for
/// billions, and a desktop system that hands out more memory than it has would close the app when
/// it ran out rather than refuse.
const ALONG_BUDGET: u64 = 4 << 30;

/// Where Points Along Lines puts points on a part, in meters along it: `start` along, then every
/// `step` up to `end` before the part's end, and with `with_end` at that end too.
#[derive(Clone, Copy, Debug)]
struct Spacing {
    step: f64,
    start: f64,
    end: f64,
    with_end: bool,
}

impl Spacing {
    /// How close two distances along a part `len` meters long are to be one place: as close as two
    /// vertices are to make a segment ([`MIN_SEGMENT`]), or a billionth of its length (a centimeter
    /// along 10,000 km) where that is more, well over the rounding in its segments' sum; and under a
    /// quarter of a step, so two points never take one place (a vertex) between them.
    fn tolerance(self, len: f64) -> f64 {
        (len * 1e-9).max(MIN_SEGMENT).min(self.step / 4.0)
    }

    /// Where the points go on a part `len` meters long, as its walk puts them. A part with no length
    /// gets none, as does one shorter than its offsets.
    fn places(self, len: f64, ring: bool) -> Places {
        self.placed(len, ring, self.tolerance(len))
    }

    /// How many points a part at most `len` meters long gets, at most: what the list is sized from,
    /// a part's length taken from a bound. It never gets smaller as `len` grows, nor is it below
    /// what [`Spacing::places`] makes for the same length, a ring's end merging into its start only
    /// within a tolerance that doesn't grow with `len` (and the walk's is never smaller).
    fn most(self, len: f64, ring: bool) -> usize {
        self.placed(len, ring, MIN_SEGMENT.min(self.step / 4.0)).n()
    }

    /// [`Spacing::places`], with a ring's end its start when the offsets are within `merge`.
    fn placed(self, len: f64, ring: bool, merge: f64) -> Places {
        let (last, tol) = (len - self.end, self.tolerance(len));
        let mut places = Places { spacing: self, regular: 0, with_end: false, last, tol };
        // Written to be false for NaN.
        if !(len > 0.0 && last >= self.start) {
            return places;
        }
        // A point within `tol` past `last` is at `last`. A float casts to an integer saturating, so a
        // step far too small for the part asks for `usize::MAX` points, which the memory budget refuses.
        places.regular = (((last - self.start + tol) / self.step).floor() as usize).saturating_add(1);
        // A ring with no offsets ends at its start, where its first point is: one is made there.
        let back_at_start = ring && self.start <= merge && self.end <= merge;
        if back_at_start && places.regular > 1 && places.at(places.regular - 1) >= len - tol {
            places.regular -= 1;
        }
        places.with_end = self.with_end && !back_at_start && last - places.at(places.regular - 1) > tol;
        places
    }
}

/// The points [`Spacing`] puts on one part: `regular` every step from its start, then with
/// `with_end` one at `last`, its end less the end offset; distances within `tol` are one place.
#[derive(Clone, Copy, Debug)]
struct Places {
    spacing: Spacing,
    regular: usize,
    with_end: bool,
    last: f64,
    tol: f64,
}

impl Places {
    /// How many points there are.
    fn n(self) -> usize {
        self.regular.saturating_add(usize::from(self.with_end))
    }

    /// How far along its part the `k`th point is.
    fn at(self, k: usize) -> f64 {
        let Spacing { start, step, .. } = self.spacing;
        if k < self.regular { (start + k as f64 * step).min(self.last) } else { self.last }
    }
}

/// How near the antimeridian, in degrees of longitude, a line's part is to end and the next to
/// begin for the two to be one line split there: a billionth of a degree, about 0.1 mm.
const SEAM: f64 = 1e-9;

/// Whether a line's part ends where the next begins, on the antimeridian: one line split there,
/// as RFC 7946 asks GeoJSON to split a line that crosses it. The two ends are within
/// [`MIN_SEGMENT`] of each other, so the walk passes over the step between them, and the parts lie
/// on opposite sides of the antimeridian; a part that touches it and turns back, or two that only
/// meet there, stay apart, as parts meeting anywhere else do.
fn meet_at_seam(part: &[Coord], next: &[Coord]) -> bool {
    fn on_seam(c: &Coord) -> bool {
        180.0 - rustgis_geom::fold_lon(c.x).abs() <= SEAM
    }
    // The side of the antimeridian a part lies on: its nearest vertex off it, from its end on. Not
    // its end's own sign, which splitters write either way (both ends at −180°, or one a rounding
    // past 180°, folded to just over −180°); that only when all of it is on the antimeridian.
    fn side<'a>(end: &'a Coord, mut rest: impl Iterator<Item = &'a Coord>) -> f64 {
        rustgis_geom::fold_lon(rest.find(|c| !on_seam(c)).unwrap_or(end).x).signum()
    }
    let mut before = part.iter().rev().filter(|c| c.is_finite());
    let mut after = next.iter().filter(|c| c.is_finite());
    let (Some(a), Some(b)) = (before.next(), after.next()) else { return false };
    on_seam(a) && on_seam(b) && side(a, before) != side(b, after) && rustgis_geom::wgs84_inverse(*a, *b).0 < MIN_SEGMENT
}

/// Calls `f` with each line Points Along Lines walks, as the run of parts that make it, and whether
/// it is a ring: each ring of a polygon on its own, and each part of a line together with the parts
/// after it that carry it on across the antimeridian ([`meet_at_seam`]), a ring when it ends where it
/// starts, within [`MIN_SEGMENT`] (a closed line, such as a contour, or one written along a parallel
/// from −180° to 180°, walked along its edges' geodesics). A point has none.
fn for_each_line(g: &Geometry, mut f: impl FnMut(&[Vec<Coord>], bool)) {
    let closed = |run: &[Vec<Coord>]| {
        let mut v = run.iter().flatten().filter(|c| c.is_finite());
        matches!((v.next(), v.next_back()), (Some(a), Some(b)) if a == b || rustgis_geom::wgs84_inverse(*a, *b).0 < MIN_SEGMENT)
    };
    match g {
        Geometry::Line(parts) => {
            let mut start = 0;
            for (i, pair) in parts.windows(2).enumerate() {
                if let [part, next] = pair
                    && !meet_at_seam(part, next)
                {
                    if let Some(run) = parts.get(start..=i) {
                        f(run, closed(run));
                    }
                    start = i + 1;
                }
            }
            if let Some(run) = parts.get(start..).filter(|r| !r.is_empty()) {
                f(run, closed(run));
            }
        }
        Geometry::Polygon(polys) => polys.iter().flatten().for_each(|r| f(std::slice::from_ref(r), true)),
        Geometry::Point(_) | Geometry::MultiPoint(_) => {}
    }
}

/// A segment of a part on the WGS 84 ellipsoid: its ends, its length in meters, and its azimuths
/// at either end (degrees clockwise from north).
#[derive(Clone, Copy, Debug)]
struct Segment {
    a: Coord,
    b: Coord,
    len: f64,
    azi_a: f64,
    azi_b: f64,
}

/// How far apart two vertices are to make a segment, in meters: the direction of a shorter one, such
/// as between a vertex and its near-duplicate, is noise.
const MIN_SEGMENT: f64 = 1e-3;

/// The segments of a line (its run of parts, see [`for_each_line`]), a ring's last one going back
/// to its first vertex. A vertex that isn't finite, or is within [`MIN_SEGMENT`] of the last one
/// kept, is passed over, its neighbors joined: so is the second end of a line split at the
/// antimeridian, the same place as the first.
fn segments(line: &[Vec<Coord>], ring: bool) -> impl Iterator<Item = Segment> + '_ {
    let mut verts = line.iter().flatten().copied().filter(|c| c.is_finite());
    let first = verts.next();
    verts
        .chain(first.filter(|_| ring))
        .scan(first, |kept, b| {
            let a = (*kept)?;
            let (len, azi_a, azi_b) = rustgis_geom::wgs84_inverse(a, b);
            // False for NaN; a vertex passed over leaves the next segment starting at `a`.
            let long_enough = len >= MIN_SEGMENT;
            if long_enough {
                *kept = Some(b);
            }
            Some(long_enough.then_some(Segment { a, b, len, azi_a, azi_b }))
        })
        .flatten()
}

/// How much longer a path can be on the WGS 84 ellipsoid than on the sphere
/// [`rustgis_geom::haversine_m`] measures, rounded up: the ellipsoid's radius of curvature is at
/// most a/√(1−e²) = 6,399,594 m (at the poles), 0.45 % over the sphere's 6,371,009 m, so a geodesic
/// is at most that much longer than the great circle between the same points.
const ELLIPSOID_STRETCH: f64 = 1.005;

/// At least the length of a part as [`segments`] measures it, in meters: its length on the
/// sphere, stretched by [`ELLIPSOID_STRETCH`]. A few operations a segment, against an iterative
/// solution for the exact length, it sizes the point list before each segment is solved, once.
/// (Leaving out a vertex only shortens a part.) Coordinates are folded and clamped as GeographicLib
/// takes them, so the bound holds for any finite ones.
fn length_bound(line: &[Vec<Coord>], ring: bool) -> f64 {
    let fold = |c: &Coord| Coord::new(rustgis_geom::fold_lon(c.x), c.y.clamp(-90.0, 90.0));
    let mut verts = line.iter().flatten().filter(|c| c.is_finite()).map(fold);
    let Some(first) = verts.next() else { return 0.0 };
    let (mut prev, mut sum) = (first, 0.0);
    for c in verts.chain(ring.then_some(first)) {
        sum += rustgis_geom::haversine_m(prev, c);
        prev = c;
    }
    sum * ELLIPSOID_STRETCH
}

/// The direction halfway between two azimuths (degrees), or `b` when they are opposite.
fn mean_azimuth(a: f64, b: f64) -> f64 {
    let ((sa, ca), (sb, cb)) = (a.to_radians().sin_cos(), b.to_radians().sin_cos());
    let (y, x) = (sa + sb, ca + cb);
    if y.hypot(x) < 1e-9 { b } else { y.atan2(x).to_degrees() }
}

/// An azimuth (degrees) from 0 up to 360: north 0, east 90.
fn bearing(azimuth: f64) -> f64 {
    let b = azimuth.rem_euclid(360.0);
    // Rounding takes a tiny negative angle to 360, and due north can come as -0 (written "-0").
    if b >= 360.0 || b == 0.0 { 0.0 } else { b }
}

/// Calls `emit` with each point `spacing` puts on a line, given its [`segments`], already solved:
/// where the point is, how far along, and the line's direction there. Each point takes one direct
/// solution, and one on a vertex none. A point on a vertex takes the mean of the directions either
/// side, as QGIS's tool does; a ring's first vertex lies between its last segment and its first.
/// Two points that would both take the same vertex (one just before it, one just after, within the
/// tolerance) are one: the same vertex of the walk, not the same place, which a line can pass
/// through twice. The walk stops when `emit` breaks.
fn walk_line(segs: &[Segment], ring: bool, spacing: Spacing, mut emit: impl FnMut(Coord, f64, f64) -> ControlFlow<()>) {
    let (Some(head), Some(tail)) = (segs.first(), segs.last()) else { return };
    let len: f64 = segs.iter().map(|s| s.len).sum();
    let places = spacing.places(len, ring);
    let (n, tol) = (places.n(), places.tol);
    let at = |k: usize| places.at(k);
    // The direction the walk arrives at its vertex in: none at a line's start; at a ring's, the one
    // its last segment comes back in.
    let mut arriving = ring.then_some(tail.azi_b);
    // The vertex the last point was put on, if it was: the start of that segment.
    let mut on_vertex: Option<usize> = None;
    let (mut k, mut walked) = (0usize, 0.0);
    for (i, seg) in segs.iter().enumerate() {
        // The points before this segment's end; one at its end is on the next one's start.
        while k < n && at(k) < walked + seg.len - tol {
            let along = at(k) - walked;
            let (c, azimuth) = if along <= tol {
                (seg.a, arriving.map_or(seg.azi_a, |inc| mean_azimuth(inc, seg.azi_a)))
            } else {
                rustgis_geom::wgs84_direct(seg.a, seg.azi_a, along)
            };
            let vertex = (along <= tol).then_some(i);
            if vertex.is_none() || vertex != on_vertex {
                if emit(c, at(k), bearing(azimuth)).is_break() {
                    return;
                }
                on_vertex = vertex;
            }
            k += 1;
        }
        if k == n {
            return;
        }
        walked += seg.len;
        arriving = Some(seg.azi_b);
    }
    // The rest are at the part's end, which on a ring is its first vertex again: one point there.
    // (`Spacing::places` already leaves out a ring's end that merges into its start; the check
    // keeps the walk from making that point twice should the two ever disagree.)
    let azimuth = if ring { mean_azimuth(tail.azi_b, head.azi_a) } else { tail.azi_b };
    if k < n && !(ring && on_vertex == Some(0)) {
        let _ = emit(tail.b, at(k), bearing(azimuth));
    }
}

/// Points at a set distance along each part of the input's lines, or each ring of its polygons,
/// placed by [`Spacing`] on the WGS 84 ellipsoid, with their feature's attributes and OID, the
/// distance along the part (in the distance's units) and the line's direction there.
fn points_along_lines(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    const TOOL: &str = "Points Along Lines";
    let ds = input(inp, p, "input")?;
    if !matches!(ds.geometry_type, GeometryType::Polyline | GeometryType::Polygon) {
        return Err(format!("{TOOL} needs a line or polygon layer"));
    }
    let unit = to_metres(1.0, s(p, "units"))?;
    // Finite in meters too: 1e308 miles is not a distance.
    let step = num(p, "distance")
        .map(|d| d * unit)
        .filter(|m| m.is_finite() && *m > 0.0)
        .ok_or_else(|| format!("{TOOL}: Distance must be a number above 0, and not too large"))?;
    // An offset left blank is none.
    let offset = |k: &str, label: &str| match p.get(k) {
        None | Some(Json::Null) => Ok(0.0),
        Some(Json::String(t)) if t.trim().is_empty() => Ok(0.0),
        Some(_) => num(p, k)
            .map(|d| d * unit)
            .filter(|m| m.is_finite() && *m >= 0.0)
            .ok_or_else(|| format!("{TOOL}: {label} must be a number, 0 or more, and not too large")),
    };
    let spacing = Spacing {
        step,
        start: offset("startOffset", "Start Offset")?,
        end: offset("endOffset", "End Offset")?,
        // On or off, as `resolve_params` checked it.
        with_end: p.get("includeEnd").and_then(Json::as_bool).unwrap_or(false),
    };
    let mut out = copy_schema(ds, out_name(p, format!("{}_PointsAlong", ds.name)), GeometryType::Point);
    // What the input lost on reading (features, Z and M values), its points have lost too.
    (out.crs, out.crs_incomplete, out.zm_dropped) = (ds.crs, ds.crs_incomplete, ds.zm_dropped);
    // A name the input already has keeps its field, and the new one gets a suffix.
    for (name, ty) in ALONG_FIELDS {
        let field = Field::new(unique_field(&out, name), ty);
        out.fields.push(field);
    }
    let (kept, width) = (ds.fields.len(), out.fields.len());
    let sel = inp.selection(s(p, "input"));
    let feats = || selected_features(sel, ds);
    // Counted first from each line's length bound, so the points are allocated once, and their
    // memory checked before anything is built; the bound counts about 0.5 % more
    // ([`ELLIPSOID_STRETCH`]), and up to a point or two more a part. The longest line sizes the one
    // list its segments are solved into, line by line. With `exact`, counted from the lengths the
    // walk measures instead, each segment solved for it, stopping once the memory is over `limit`.
    let count = |exact: bool, limit: u64, longest: &mut usize| {
        size_output_points(feats(), kept, width, true, limit, |g| {
            let mut k = 0usize;
            // An exact count, solving every segment, can take a while: a cancel stops it (it's
            // then reported below, before the count is used).
            if exact && crate::control::cancelled() {
                return k;
            }
            for_each_line(g, |line, ring| {
                *longest = (*longest).max(line.iter().map(Vec::len).fold(0, usize::saturating_add));
                // A cancel stops the solving within a line too (one can have millions of vertices).
                let len = if exact {
                    segments(line, ring).take_while(|_| !crate::control::cancelled()).map(|s| s.len).sum::<f64>()
                } else {
                    length_bound(line, ring)
                };
                k = k.saturating_add(spacing.most(len, ring));
            });
            k
        })
    };
    let mut longest = 0usize;
    let (mut n, bound_need) = count(false, u64::MAX, &mut longest);
    let u = |x: usize| u64::try_from(x).unwrap_or(u64::MAX);
    let segments_bytes = u(longest).saturating_mul(u(size_of::<Segment>()));
    // Always estimated (`true` above), as the budget holds on every platform; a missing estimate
    // would count as over it, never as needing nothing.
    let total = |need: Option<u64>| need.unwrap_or(u64::MAX).saturating_add(segments_bytes);
    let mut need = total(bound_need);
    let mut room = web_has_room(need);
    if (need > ALONG_BUDGET || !room) && segments_bytes <= ALONG_BUDGET {
        // Before refusing for what the bound counts, count the points exactly: only a run that
        // doesn't fit by its bound (in the browser, by the room it has too) solves its segments
        // twice, and the count stops once it's sure not to fit the budget either, so one that
        // can't costs no more.
        let (en, eneed) = count(true, ALONG_BUDGET - segments_bytes, &mut longest);
        crate::control::check()?;
        let eneed = total(eneed);
        if eneed <= ALONG_BUDGET {
            (n, need) = (en, eneed);
            room = web_has_room(need);
        }
    }
    // One line with tens of millions of vertices can be what doesn't fit.
    let advice = if segments_bytes > need / 2 {
        "simplify the lines with the most vertices, or select fewer features"
    } else {
        "use a larger distance, or select fewer features"
    };
    // Over the budget, the count is still the bound's: an exact one that fits replaced it.
    if need > ALONG_BUDGET {
        let points = if n == usize::MAX { "more points than it can count".to_string() } else { format!("up to {n} points") };
        let memory =
            if need == u64::MAX { "more memory than it can count".to_string() } else { format!("about {} MiB", need.div_ceil(1 << 20)) };
        return Err(format!("{TOOL}: {points} would need {memory}, more than the {} GiB it may use: {advice}", ALONG_BUDGET >> 30));
    }
    check_room(TOOL, n, need, advice, |_| room)?;
    let no_memory = |e: TryReserveError| Stopped::Memory(e).message(TOOL, n);
    let mut list = PointList::new(n).map_err(no_memory)?;
    let mut segs = Vec::new();
    segs.try_reserve_exact(longest).map_err(no_memory)?;
    // Vertices passed over for a coordinate missing or infinite, their neighbors joined.
    let mut missing = 0usize;
    // Vertices with a latitude past a pole (bad data), taken as that pole.
    let mut past_pole = 0usize;
    let (mut without_geometry, mut point_features, mut without_segments) = (0usize, 0usize, 0usize);
    // The progress counts features; a cancel stops the walk at the next point, line or feature.
    crate::control::begin(feats().count());
    for (i, f) in feats().enumerate() {
        crate::control::check()?;
        crate::control::reached(i);
        if list.stopped() {
            break;
        }
        let Some(g) = &f.geometry else {
            without_geometry = without_geometry.saturating_add(1);
            continue;
        };
        if matches!(g, Geometry::Point(_) | Geometry::MultiPoint(_)) {
            point_features = point_features.saturating_add(1);
            continue;
        }
        let own = f.attrs.get(..kept).unwrap_or(&f.attrs);
        // An id a reader wrapped from a negative key (a GeoPackage's) is that key again.
        let fid = Value::Int(f.id.cast_signed());
        let mut has_segments = false;
        for_each_line(g, |line, ring| {
            if list.stopped() || crate::control::cancelled() {
                return;
            }
            missing = missing.saturating_add(line.iter().flatten().filter(|c| !c.is_finite()).count());
            past_pole = past_pole.saturating_add(line.iter().flatten().filter(|c| c.is_finite() && c.y.abs() > 90.0).count());
            // Each segment solved once. A line has no more segments than vertices, so the list
            // stays within the room reserved for the longest.
            segs.clear();
            // A cancel stops the solving within a line too; the walk below then stops at once.
            segs.extend(segments(line, ring).take_while(|_| !crate::control::cancelled()));
            has_segments |= !segs.is_empty();
            walk_line(&segs, ring, spacing, |c, along, angle| {
                if crate::control::cancelled() {
                    return ControlFlow::Break(());
                }
                // Folded into −180..180: a point past the antimeridian is on the far side of the map.
                // A vertex's latitude past a pole (bad data) is the pole, where the walk measured it.
                let c = Coord::new(rustgis_geom::fold_lon(c.x), c.y.clamp(-90.0, 90.0));
                list.push(c, own, kept, [fid.clone(), Value::Float(along / unit), Value::Float(angle)])
            });
        });
        if !has_segments {
            without_segments = without_segments.saturating_add(1);
        }
    }
    // A walk a cancel stopped partway made only some of the points.
    crate::control::check()?;
    crate::control::finish();
    // Freed first, so its room is there if the points move into a list of their own size.
    drop(segs);
    out.features = list.finish().map_err(|e| e.message(TOOL, n))?.into();
    let written = out.features.len();
    let mut r = result(out, format!("{TOOL}: {written} points"))?;
    if missing > 0 {
        r.messages.push(format!("{TOOL}: {missing} vertex(es) with a coordinate missing or infinite passed over"));
    }
    if past_pole > 0 {
        r.messages.push(format!(
            "{TOOL}: {past_pole} vertex(es) with a latitude past a pole found (latitudes limited to the poles when measuring segments)"
        ));
    }
    if without_geometry > 0 {
        r.messages.push(format!("{TOOL}: {without_geometry} feature(s) without geometry skipped"));
    }
    if point_features > 0 {
        r.messages.push(format!("{TOOL}: {point_features} point feature(s) skipped; only lines and polygon rings produce points"));
    }
    if without_segments > 0 {
        r.messages.push(format!("{TOOL}: {without_segments} line or polygon feature(s) without measurable segments skipped"));
    }
    Ok(r)
}

/// A coordinate from a table cell: a number, or text holding one, written with a decimal point
/// or (when it has no point) a single decimal comma, as spreadsheets in many languages do. A comma
/// never separates thousands: `1,234` is 1.234 (refusing it would refuse `1234,567` too), and
/// `1,234.5` is not a number.
fn coordinate(v: &Value) -> Option<f64> {
    let n = match v {
        Value::Int(i) => *i as f64,
        Value::Float(f) => *f,
        Value::Text(t) => {
            let t = t.trim();
            match t.parse::<f64>() {
                Ok(n) => n,
                Err(_) if !t.contains('.') && t.matches(',').count() == 1 => t.replace(',', ".").parse().ok()?,
                Err(_) => return None,
            }
        }
        Value::Null | Value::Bool(_) => return None,
    };
    n.is_finite().then_some(n)
}

/// The rows looked at to tell whether a text field holds numbers.
const COORDINATE_SAMPLE: usize = 1000;

/// Whether a field can hold coordinates: a numeric field, or a text field where one of the
/// first rows reads as a number, as [`coordinate`] reads it. The Tools pane lists these fields
/// for a coordinate parameter, so a table without any offers none.
pub fn holds_coordinates(ds: &Dataset, field: usize) -> bool {
    match ds.fields.get(field).map(|f| f.ty) {
        Some(FieldType::Text) => (0..ds.features.len().min(COORDINATE_SAMPLE)).any(|r| coordinate(ds.value(r, field)).is_some()),
        Some(ty) => ty.is_numeric(),
        None => false,
    }
}

fn xy_table_to_point(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let ds = input(inp, p, "input")?;
    let field = |k: &str| {
        let name = s(p, k).trim();
        find_field_case_insensitive(ds, name)
    };
    let (xi, yi) = (field("x")?, field("y")?);
    let crs = match s(p, "crs").trim() {
        "" => Crs::LonLat,
        t => Crs::parse(t)?,
    };
    let to = crs.transformer()?;
    let mut out = copy_schema(ds, out_name(p, format!("{}_Points", ds.name)), GeometryType::Point);
    // Rows with numbers in both fields, and those of them that land on the globe.
    let (mut read, mut placed, mut total) = (0usize, 0usize, 0usize);
    for r in rows(inp, p, "input", ds) {
        let Some(f) = ds.features.get(r) else { continue };
        total += 1;
        let xy = coordinate(ds.value(r, xi)).zip(coordinate(ds.value(r, yi)));
        read += usize::from(xy.is_some());
        let pt = xy.and_then(|(x, y)| to.to_lonlat(Coord::new(x, y))).filter(|c| c.is_finite() && c.x.abs() <= 180.0 && c.y.abs() <= 90.0);
        placed += usize::from(pt.is_some());
        // A row without a usable position stays in the layer's table, without a point.
        out.push_next(pt.map(Geometry::Point), f.attrs.clone());
    }
    if placed == 0 {
        return Err(if read == 0 {
            format!("{}: no row has numbers in both '{}' and '{}'", ds.name, s(p, "x").trim(), s(p, "y").trim())
        } else {
            format!(
                "{}: the coordinates aren't in {}: give their coordinate system (\"crs\", such as EPSG:2263) to place them",
                ds.name,
                crs.label()
            )
        });
    }
    let mut messages = vec![format!("XY Table To Point: {placed} points")];
    if placed < total {
        messages.push(format!("{} of {total} rows have no usable coordinates; they are in the table without a point", total - placed));
    }
    Ok(ToolResult { output: Output::Layer(out), messages })
}

// Three fixed non-collinear anchors prevent ring collapse and keep vertex counts monotonic.
fn simplify_ring(ring: &geo::LineString<f64>, eps: f64) -> geo::LineString<f64> {
    use geo::{Distance, Euclidean};
    if ring.0.len() <= 4 || ring.0.iter().any(|c| !c.x.is_finite() || !c.y.is_finite()) {
        return ring.clone();
    }
    let Some(&first) = ring.0.first() else { return ring.clone() };
    let Some((b, _)) = ring.0.iter().enumerate().map(|(i, c)| (i, Euclidean.distance(first, *c))).max_by(|a, b| a.1.total_cmp(&b.1)) else {
        return ring.clone();
    };
    let Some(&second) = ring.0.get(b) else { return ring.clone() };
    let Some((c, _)) = ring
        .0
        .iter()
        .enumerate()
        .map(|(i, c)| (i, ((second.x - first.x) * (c.y - first.y) - (second.y - first.y) * (c.x - first.x)).abs()))
        .filter(|(_, area)| area.is_finite() && *area > 0.0)
        .max_by(|a, b| a.1.total_cmp(&b.1))
    else {
        return ring.clone();
    };
    let mut stops = [0, b, c, ring.0.len() - 1];
    stops.sort_unstable();
    let mut corners = Vec::new();
    for bounds in stops.windows(2) {
        let [start, end] = bounds else { return ring.clone() };
        let Some(arc) = ring.0.get(*start..=*end) else { return ring.clone() };
        let simple = geo::LineString::new(arc.to_vec()).simplify(eps);
        corners.extend(simple.0.into_iter().skip(usize::from(!corners.is_empty())));
    }
    geo::LineString::new(corners)
}

/// A geometry simplified (Douglas–Peucker) to a tolerance in metres.
pub fn simplify_geometry(g: &Geometry, tol_m: f64) -> Geometry {
    let eps = tol_m * merc_scale(feature_lat(g));
    let sg = match g {
        Geometry::Polygon(_) => to_multipolygon(g).and_then(|mut m| {
            for p in &mut m.0 {
                p.exterior_mut(|r| *r = simplify_ring(r, eps));
                p.interiors_mut(|rings| {
                    for r in rings {
                        *r = simplify_ring(r, eps);
                    }
                });
            }
            from_multipolygon(&m)
        }),
        Geometry::Line(_) => to_multiline(g).and_then(|m| from_line(&m.simplify(eps))),
        _ => None,
    };
    sg.unwrap_or_else(|| g.clone())
}

fn simplify(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let ds = input(inp, p, "input")?;
    let tol = num(p, "tolerance").ok_or("Tolerance must be a number")?.abs() * 1000.0;
    let mut out = copy_schema(ds, out_name(p, format!("{}_Simplified", ds.name)), ds.geometry_type);
    let (mut before, mut after) = (0usize, 0usize);
    for r in rows(inp, p, "input", ds) {
        let Some(f) = ds.features.get(r) else { continue };
        let Some(g) = &f.geometry else { continue };
        before += g.coord_count();
        let sg = simplify_geometry(g, tol);
        after += sg.coord_count();
        out.push(Some(sg), f.attrs.clone());
    }
    result(out, format!("Simplify: {before} → {after} vertices"))
}

/// Conservative heap accounting for the geometry variants produced by `to_geo`. Coordinate
/// capacities are exact; private polygon-interior arrays use an upper allocation allowance.
fn selector_heap_bytes(g: &geo::Geometry<f64>) -> usize {
    let line = |l: &geo::LineString<f64>| l.0.capacity().saturating_mul(std::mem::size_of::<geo::Coord<f64>>());
    match g {
        geo::Geometry::Point(_) => 0,
        geo::Geometry::MultiPoint(p) => p.0.capacity().saturating_mul(std::mem::size_of::<geo::Point<f64>>()),
        geo::Geometry::MultiLineString(p) => {
            p.0.iter().fold(p.0.capacity().saturating_mul(std::mem::size_of::<geo::LineString<f64>>()), |n, l| n.saturating_add(line(l)))
        }
        geo::Geometry::MultiPolygon(p) => {
            p.0.iter().fold(p.0.capacity().saturating_mul(std::mem::size_of::<geo::Polygon<f64>>()), |n, p| {
                let interiors = if p.interiors().is_empty() { 0 } else { p.interiors().len().max(4).saturating_mul(2) };
                p.interiors().iter().fold(
                    n.saturating_add(line(p.exterior()))
                        .saturating_add(interiors.saturating_mul(std::mem::size_of::<geo::LineString<f64>>())),
                    |n, l| n.saturating_add(line(l)),
                )
            })
        }
        // `to_geo` emits only the four variants above.
        _ => 0,
    }
}

/// Select against nearby selectors, indexed in the projection used by the predicate and after
/// any distance buffer. Inversion and input row order keep the exhaustive selection's semantics.
fn select_by_location(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let bytes = max_buffer_bytes();
    select_by_location_with_limits(inp, p, MAX_BUFFER_VERTICES, bytes).map_err(|e| with_budget(e, bytes))
}

/// Bounds each distance-buffered selector's complexity and their combined retained storage.
/// Zero-distance selection uses original geometries without a buffer budget.
fn select_by_location_with_limits(inp: &dyn Inputs, p: &Json, limit: usize, bytes: usize) -> Result<ToolResult, String> {
    select_by_location_with_work_limit(inp, p, limit, bytes, MAX_SELECTOR_CHECKS)
}

/// Injects the large-selector candidate allowance for deterministic work-exhaustion regressions.
fn select_by_location_with_work_limit(inp: &dyn Inputs, p: &Json, limit: usize, bytes: usize, checks: usize) -> Result<ToolResult, String> {
    let ds = input(inp, p, "input")?;
    let sel_ds = input(inp, p, "select")?;
    let dist = to_metres(num(p, "distance").unwrap_or(0.0).max(0.0), s(p, "units"))?;
    if dist > 5_000_000.0 {
        return Err("Distance is too large (limit 5000 km)".into());
    }
    let chosen = rows(inp, p, "select", sel_ds);
    let picked: Vec<&Geometry> = chosen.iter().filter_map(|r| sel_ds.features.get(*r)?.geometry.as_ref()).collect();
    // Each selector has its own complexity limit. Retained selectors share a separate byte
    // budget; zero-distance selection keeps its existing unbuffered behavior.
    let storage = BufferStorage::new(if dist > 0.0 { bytes } else { usize::MAX });
    let mut selectors = Vec::new();
    crate::control::begin(picked.len());
    for batch in picked.chunks(BUFFER_BATCH) {
        let made = crate::parallel::try_map(
            batch,
            PARALLEL_FEATURES,
            || (),
            |_, _, g| {
                let b = if dist > 0.0 { geodesic_buffer_with_limit(g, dist, limit)?.geometry } else { None };
                let gg = to_geo(b.as_ref().unwrap_or(g));
                if dist > 0.0 {
                    storage.charge(selector_heap_bytes(&gg))?;
                }
                let ext = merc_extent(&gg);
                Ok::<_, &'static str>((gg, ext))
            },
        )?;
        storage.reserve(&mut selectors, made.len())?;
        selectors.extend(made);
    }
    // The index also has an independent one-million-reference construction limit. Charge its
    // temporary extent array before building and its retained allocations before selection.
    if dist > 0.0 {
        storage.charge(selectors.len().saturating_mul(std::mem::size_of::<Extent>()))?;
    }
    let index = ExtentIndex::new(selectors.iter().map(|(_, ext)| *ext));
    if dist > 0.0 {
        storage.charge(index.storage_bytes())?;
    }
    let invert = s(p, "invert") == "Yes";
    let bounded_checks = dist > 0.0 && selectors.len() > ExtentIndex::MAX_LINEAR_FEATURES;
    let mut remaining_checks = checks;
    let mut hits = Vec::new();
    crate::control::begin(ds.features.len());
    for (r, feature) in ds.features.iter().enumerate() {
        crate::control::check()?;
        crate::control::reached(r);
        let Some(g) = feature.geometry.as_ref() else { continue };
        let gg = to_geo(g);
        let ext = merc_extent(&gg);
        let mut exhausted = false;
        let hit = index.any_candidate(&ext, |i| {
            if bounded_checks {
                if remaining_checks == 0 {
                    exhausted = true;
                    // Stop the candidate traversal; this sentinel is never used as a match.
                    return true;
                }
                remaining_checks -= 1;
            }
            selectors.get(i).is_some_and(|(s, e)| e.intersects(&ext) && s.intersects(&gg))
        });
        if exhausted {
            return Err(format!("Distance selection exceeds its limit of {checks} selector checks; select fewer features or selectors"));
        }
        if hit != invert {
            hits.push(r);
        }
    }
    let n = hits.len();
    let mut messages = vec![format!("Selected {n} features in {}", ds.name)];
    let without = ds.features.iter().filter(|f| f.geometry.is_none()).count();
    if without > 0 {
        messages.push(format!("{without} of {} features lack geometry, so they are not selected", ds.features.len()));
    }
    if picked.len() < chosen.len() {
        messages.push(format!(
            "{} of {} selecting features lack geometry, so they select nothing",
            chosen.len() - picked.len(),
            chosen.len()
        ));
    }
    Ok(ToolResult { output: Output::Selection { layer: ds.name.clone(), rows: hits }, messages })
}

fn select_by_attributes(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let ds = input(inp, p, "input")?;
    let hits = rustgis_data::query::select(s(p, "where"), ds)?;
    let n = hits.len();
    Ok(ToolResult {
        output: Output::Selection { layer: ds.name.clone(), rows: hits },
        messages: vec![format!("Selected {n} features in {}", ds.name)],
    })
}

/// The QGIS IDW interpolation: every point weighs on every cell.
fn idw_interpolation(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let ds = input(inp, p, "input")?;
    let field = s(p, "field");
    let fi = find_field_case_insensitive(ds, field)?;
    let mut pts: Vec<(Coord, f64)> = Vec::new();
    for r in rows(inp, p, "input", ds) {
        let Some(g) = ds.features.get(r).and_then(|f| f.geometry.as_ref()) else { continue };
        let Some(v) = ds.value(r, fi).as_f64().filter(|v| v.is_finite()) else { continue };
        g.for_each_coord(|c| {
            if c.is_finite() {
                pts.push((c, v));
            }
        });
    }
    let extent = match s(p, "extent").trim() {
        "" => {
            let mut e = Extent::EMPTY;
            pts.iter().for_each(|(c, _)| e.expand(*c));
            // Points in a row span no area: give the grid some.
            if e.width() > 0.0 && e.height() > 0.0 { e } else { e.padded(0.05, 0.01) }
        }
        t => {
            // Every component must parse: dropping a bad one would shift the rest.
            let v: Option<Vec<f64>> = t.split(',').map(|x| x.trim().parse().ok().filter(|x: &f64| x.is_finite())).collect();
            let Some([x0, y0, x1, y1]) = v.as_deref().and_then(|v| <[f64; 4]>::try_from(v).ok()) else {
                return Err("the extent is xmin,ymin,xmax,ymax".into());
            };
            Extent::new(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1))
        }
    };
    // Defaults only when a value is not given: a bad one is refused, not replaced.
    let given = |k: &str| p.get(k).is_some_and(|v| !v.is_null() && v.as_str().is_none_or(|t| !t.trim().is_empty()));
    let cell = match num(p, "pixelSize") {
        Some(c) if c > 0.0 => c,
        _ if given("pixelSize") => return Err("the pixel size must be more than zero".into()),
        _ => idw_auto_cell(&extent, pts.len()),
    };
    let power = match num(p, "power") {
        Some(v) => v,
        None if given("power") => return Err("the distance coefficient must be a number".into()),
        None => 2.0,
    };
    let name = out_name(p, format!("{}_{field}_idw", ds.name));
    let r = crate::rasterops::idw(&pts, &extent, cell, power, 0, &name)?;
    let bytes = rustgis_data::raster::geotiff::write(&r, 0).map_err(|e| e.to_string())?;
    let msg = format!("IDW Interpolation: {} × {} cells from {} points", r.width, r.height, pts.len());
    Ok(ToolResult {
        output: Output::Layers(vec![OutputLayer::Raster { file_name: format!("{name}.tif"), name, bytes }]),
        messages: vec![msg],
    })
}

/// The automatic IDW cell size: 300 cells across the extent, coarsened until every cell weighing
/// every one of `n` points stays within the interpolation's work limit.
fn idw_auto_cell(extent: &Extent, n: usize) -> f64 {
    let mut cell = (extent.width().max(extent.height()) / 300.0).max(1e-5);
    let cells = |c: f64| ((extent.width() / c).ceil().max(1.0) * (extent.height() / c).ceil().max(1.0)) as usize;
    for _ in 0..200 {
        if cells(cell).saturating_mul(n) <= crate::rasterops::MAX_IDW_WORK {
            break;
        }
        cell *= 1.1;
    }
    cell
}

/// Ordinary Kriging interpolation: builds prediction and error surfaces from spatial autocorrelation.
fn kriging_interpolation(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let ds = input(inp, p, "input")?;
    let field = s(p, "field");
    let fi = find_field_case_insensitive(ds, field)?;
    let mut raw_pts: Vec<(Coord, f64)> = Vec::new();
    for r in rows(inp, p, "input", ds) {
        let Some(g) = ds.features.get(r).and_then(|f| f.geometry.as_ref()) else { continue };
        let Some(v) = ds.value(r, fi).as_f64().filter(|v| v.is_finite()) else { continue };
        match g {
            Geometry::Point(c) => {
                if c.is_finite() {
                    raw_pts.push((*c, v));
                }
            }
            Geometry::MultiPoint(cs) => {
                for c in cs {
                    if c.is_finite() {
                        raw_pts.push((*c, v));
                    }
                }
            }
            Geometry::Line(_) | Geometry::Polygon(_) => {
                let ext = g.extent();
                if !ext.is_empty() {
                    let center = Coord::new((ext.min_x + ext.max_x) * 0.5, (ext.min_y + ext.max_y) * 0.5);
                    if center.is_finite() {
                        raw_pts.push((center, v));
                    }
                }
            }
        }
    }
    // Deduplicate sample points by coordinate to prevent singular kriging matrix
    raw_pts.sort_by(|a, b| a.0.x.total_cmp(&b.0.x).then_with(|| a.0.y.total_cmp(&b.0.y)));
    let mut accum: Vec<(Coord, f64, usize)> = Vec::with_capacity(raw_pts.len());
    for (c, v) in raw_pts {
        let mut duplicate_idx = None;
        for (i, (last_c, _, _)) in accum.iter().enumerate().rev() {
            if c.x - last_c.x >= 1e-9 {
                break;
            }
            if (c.y - last_c.y).abs() < 1e-9 {
                duplicate_idx = Some(i);
                break;
            }
        }
        if let Some(i) = duplicate_idx {
            if let Some((_, sum, count)) = accum.get_mut(i) {
                *sum += v;
                *count += 1;
            }
            continue;
        }
        accum.push((c, v, 1));
    }
    let pts: Vec<(Coord, f64)> = accum.into_iter().map(|(c, sum, count)| (c, sum / count as f64)).collect();
    if pts.len() < 2 {
        return Err("kriging needs at least two points with values".into());
    }

    let extent = match s(p, "extent").trim() {
        "" => {
            let mut e = Extent::EMPTY;
            pts.iter().for_each(|(c, _)| e.expand(*c));
            if e.width() > 0.0 && e.height() > 0.0 { e } else { e.padded(0.05, 0.01) }
        }
        t => {
            let v: Option<Vec<f64>> = t.split(',').map(|x| x.trim().parse().ok().filter(|x: &f64| x.is_finite())).collect();
            let Some([x0, y0, x1, y1]) = v.as_deref().and_then(|v| <[f64; 4]>::try_from(v).ok()) else {
                return Err("the extent is xmin,ymin,xmax,ymax".into());
            };
            Extent::new(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1))
        }
    };
    let given = |k: &str| p.get(k).is_some_and(|v| !v.is_null() && v.as_str().is_none_or(|t| !t.trim().is_empty()));
    let neighbours = match num(p, "neighbours") {
        Some(n) if n >= 2.0 => (n as usize).min(32),
        Some(_) => return Err("neighbours must be at least 2".into()),
        None if given("neighbours") => return Err("neighbours must be a number".into()),
        None => 12,
    };
    let cell = match num(p, "pixelSize") {
        Some(c) if c > 0.0 => c,
        _ if given("pixelSize") => return Err("the pixel size must be more than zero".into()),
        _ => kriging_auto_cell(&extent, pts.len(), neighbours),
    };
    let model = match s(p, "model") {
        "" => crate::kriging::VariogramModel::Spherical,
        m => m.parse::<crate::kriging::VariogramModel>().map_err(|_| format!("unknown variogram model '{m}'"))?,
    };
    let mut params = crate::kriging::auto_variogram(&pts, &extent, model);
    match num(p, "nugget") {
        Some(v) if v >= 0.0 => params.nugget = v,
        Some(_) => return Err("nugget must be a non-negative number".into()),
        None if given("nugget") => return Err("nugget must be a number".into()),
        None => {}
    }
    match num(p, "sill") {
        Some(v) if v > 0.0 => params.sill = v,
        Some(_) => return Err("sill must be a positive number".into()),
        None if given("sill") => return Err("sill must be a number".into()),
        None => {}
    }
    match num(p, "range") {
        Some(v) if v > 0.0 => params.range = v,
        Some(_) => return Err("range must be a positive number".into()),
        None if given("range") => return Err("range must be a number".into()),
        None => {}
    }

    let name = out_name(p, format!("{}_{field}_kriging", ds.name));
    let r = crate::kriging::kriging(&pts, &extent, cell, &params, neighbours, &name)?;
    let pred_bytes = rustgis_data::raster::geotiff::write(&r, 0).map_err(|e| e.to_string())?;
    let var_bytes = rustgis_data::raster::geotiff::write(&r, 1).map_err(|e| e.to_string())?;
    let var_name = format!("{name}_variance");
    let msg = format!("Ordinary Kriging ({}): {} × {} cells from {} points", model.as_str(), r.width, r.height, pts.len());
    Ok(ToolResult {
        output: Output::Layers(vec![
            OutputLayer::Raster { file_name: format!("{name}.tif"), name, bytes: pred_bytes },
            OutputLayer::Raster { file_name: format!("{var_name}.tif"), name: var_name, bytes: var_bytes },
        ]),
        messages: vec![msg],
    })
}

fn kriging_auto_cell(extent: &Extent, n: usize, neighbours: usize) -> f64 {
    let k = if neighbours == 0 { 12.min(n) } else { neighbours.clamp(2, n.clamp(2, 32)) };
    let work_per_cell = n.saturating_add((k * k * k) / 3);
    let mut cell = (extent.width().max(extent.height()) / 250.0).max(1e-5);
    let cells = |c: f64| ((extent.width() / c).ceil().max(1.0) * (extent.height() / c).ceil().max(1.0)) as usize;
    for _ in 0..200 {
        if cells(cell).saturating_mul(work_per_cell) <= crate::kriging::MAX_KRIGING_WORK {
            break;
        }
        cell *= 1.1;
    }
    cell
}

fn parse_optional_extent(p: &Json) -> Result<Option<Extent>, String> {
    let raw = match p.get("extent") {
        None | Some(serde_json::Value::Null) => return Ok(None),
        Some(serde_json::Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                return Ok(None);
            }
            let v: Option<Vec<f64>> = t.split(',').map(|x| x.trim().parse().ok().filter(|x: &f64| x.is_finite())).collect();
            v.as_deref().and_then(|v| <[f64; 4]>::try_from(v).ok()).ok_or("the extent is xmin,ymin,xmax,ymax")?
        }
        Some(serde_json::Value::Array(arr)) => {
            if arr.len() != 4 {
                return Err("the extent is xmin,ymin,xmax,ymax".into());
            }
            let coords: Option<Vec<f64>> = arr.iter().map(|v| v.as_f64().filter(|x| x.is_finite())).collect();
            coords.as_deref().and_then(|v| <[f64; 4]>::try_from(v).ok()).ok_or("the extent is xmin,ymin,xmax,ymax")?
        }
        Some(_) => return Err("the extent is xmin,ymin,xmax,ymax".into()),
    };

    let (min_x, max_x) = (raw[0].min(raw[2]), raw[0].max(raw[2]));
    let (min_y, max_y) = (raw[1].min(raw[3]), raw[1].max(raw[3]));
    let (w, h) = (max_x - min_x, max_y - min_y);
    if !(max_x > min_x && max_y > min_y && w.is_finite() && h.is_finite()) {
        return Err("the extent must have a positive width and height".into());
    }
    Ok(Some(Extent::new(min_x, min_y, max_x, max_y)))
}

fn kernel_density_tool(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let ds = input(inp, p, "input")?;
    if ds.geometry_type != GeometryType::Point {
        return Err("Input Points must be a point layer".into());
    }

    if p.get("crs").is_some_and(|v| !v.is_null() && !v.is_string()) {
        return Err("crs must be text".into());
    }

    let crs_text = s(p, "crs").trim();
    let crs = if !crs_text.is_empty() {
        Crs::parse(crs_text)?
    } else if let Some(c) = ds.crs {
        c
    } else {
        return Err(format!(
            "kernel density requires a projected coordinate system with linear units (such as UTM or State Plane); '{}' has no coordinate system. Provide a projected system under 'crs' (e.g. EPSG:32618)",
            ds.name
        ));
    };

    if crs.is_geographic() {
        if !crs_text.is_empty() {
            return Err("crs must be a projected coordinate system with linear units (such as UTM or State Plane, e.g. EPSG:32618), not geographic coordinates (degrees)".into());
        }
        return Err(format!(
            "kernel density requires a projected coordinate system with linear units (such as UTM or State Plane); '{}' is in geographic coordinates (degrees). Provide a projected system under 'crs' (e.g. EPSG:32618)",
            ds.name
        ));
    }

    let transformer = crs.transformer()?;

    if p.get("field").is_some_and(|v| !v.is_null() && !v.is_string()) {
        return Err("field must be text".into());
    }

    let field_name = s(p, "field").trim();
    let fi = if !field_name.is_empty() {
        let idx = find_field_case_insensitive(ds, field_name)?;
        let f = ds.fields.get(idx).ok_or_else(|| format!("no field named '{field_name}'"))?;
        if !f.ty.is_numeric() {
            return Err(format!("field '{field_name}' must be numeric (is {})", f.ty.label()));
        }
        Some(idx)
    } else {
        None
    };

    let mut pts: Vec<(Coord, f64)> = Vec::new();
    let mut skipped: usize = 0;
    for r in rows(inp, p, "input", ds) {
        let Some(g) = ds.features.get(r).and_then(|f| f.geometry.as_ref()) else {
            skipped += 1;
            continue;
        };
        let w = match fi {
            Some(idx) => match ds.value(r, idx).as_f64() {
                Some(v) if v.is_finite() && v > 0.0 => v,
                _ => {
                    skipped += 1;
                    continue;
                }
            },
            None => 1.0,
        };

        match g {
            Geometry::Point(c) => {
                if c.is_finite()
                    && let Some(p_proj) = transformer.from_lonlat(*c)
                    && p_proj.is_finite()
                {
                    pts.push((p_proj, w));
                } else {
                    skipped += 1;
                }
            }
            Geometry::MultiPoint(cs) => {
                let mut any = false;
                for c in cs {
                    if c.is_finite()
                        && let Some(p_proj) = transformer.from_lonlat(*c)
                        && p_proj.is_finite()
                    {
                        pts.push((p_proj, w));
                        any = true;
                    }
                }
                if !any {
                    skipped += 1;
                }
            }
            _ => {
                skipped += 1;
            }
        }
    }

    if pts.is_empty() {
        return Err("input layer has no valid points with positive weights to analyze".into());
    }

    let pt_extent = crate::density::points_extent(&pts).ok_or("input layer has no valid points with positive weights to analyze")?;

    let given = |k: &str| p.get(k).is_some_and(|v| !v.is_null() && v.as_str().is_none_or(|t| !t.trim().is_empty()));
    let (mut bandwidth, user_radius) = match num(p, "radius") {
        Some(h) if h > 0.0 => (h, true),
        Some(0.0) => (crate::density::silverman_bandwidth(&pts, &pt_extent), false),
        Some(_) => return Err("search radius must be a positive number".into()),
        None if given("radius") => return Err("search radius must be a number".into()),
        None => (crate::density::silverman_bandwidth(&pts, &pt_extent), false),
    };

    let custom_extent = parse_optional_extent(p)?;
    let mut grid_ext = crate::density::resolve_grid_extent(&pt_extent, bandwidth, custom_extent.as_ref())?;

    let cell_size = match num(p, "pixelSize") {
        Some(c) if c > 0.0 => {
            if user_radius && c > bandwidth {
                return Err("pixel size must not be larger than the search radius (bandwidth); choose a smaller pixel size or a larger search radius".into());
            }
            c
        }
        Some(_) => return Err("pixel size must be a positive number".into()),
        None if given("pixelSize") => return Err("pixel size must be a number".into()),
        None => {
            let auto = crate::density::auto_cell_size(&grid_ext);
            if user_radius { auto.min(bandwidth) } else { auto }
        }
    };

    if !user_radius && bandwidth < cell_size {
        bandwidth = cell_size;
        if custom_extent.is_none() {
            grid_ext = crate::density::resolve_grid_extent(&pt_extent, bandwidth, None)?;
        }
    }

    let kernel = crate::density::KernelType::parse(s(p, "kernel"))?;

    if p.get("output").is_some_and(|v| !v.is_null() && !v.is_string()) {
        return Err("output must be text".into());
    }

    let name = out_name(p, format!("{}_density", ds.name));
    let r = crate::density::kernel_density(&pts, Some(&grid_ext), cell_size, bandwidth, kernel, &name, crs)?;
    let bytes = rustgis_data::raster::geotiff::write(&r, 0).map_err(|e| e.to_string())?;

    let msg = if skipped > 0 {
        format!(
            "Kernel Density: {} × {} cells from {} points ({} skipped, bandwidth: {:.2})",
            r.width,
            r.height,
            pts.len(),
            skipped,
            bandwidth
        )
    } else {
        format!("Kernel Density: {} × {} cells from {} points (bandwidth: {:.2})", r.width, r.height, pts.len(), bandwidth)
    };
    Ok(ToolResult {
        output: Output::Layers(vec![OutputLayer::Raster { file_name: format!("{name}.tif"), name, bytes }]),
        messages: vec![msg],
    })
}

/// Project coordinates directly into an extent, without allocating the full geometry. Use the
/// same transform and non-finite-coordinate filtering as [`to_geo`] followed by [`merc_extent`].
fn projected_extent(g: &Geometry) -> Extent {
    let mut ext = Extent::EMPTY;
    g.for_each_coord(|c| ext.expand(rustgis_geom::lonlat_to_merc(c)));
    ext
}

/// The extent of a geometry in Web Mercator, as [`to_geo`] gives it, leaving out non-finite
/// coordinates as [`Extent::expand`] does: one NaN doesn't hide the rest.
fn merc_extent(g: &impl geo::CoordsIter<Scalar = f64>) -> Extent {
    g.coords_iter().fold(Extent::EMPTY, |mut e, c| {
        e.expand(Coord::new(c.x, c.y));
        e
    })
}

/// Summarize Within: each polygon's count of the (selected) point features that meet it, inside it
/// or on its edge; a feature of several points counts once in each polygon it meets. Each point is
/// tested only against the polygons whose extents hold it, through an [`ExtentIndex`]. The extents
/// are in Web Mercator, where the test runs, so a point beyond the map's latitude limit still
/// meets a polygon reaching that limit. The statistics asked for, of the point layer's fields,
/// are over the same points, as `<STAT>_<field>` after `point_count`.
fn count_points(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let polys = input(inp, p, "polygons")?;
    let pts = input(inp, p, "points")?;
    let (requests, separator) = summary_requests(pts, p)?;
    let mut out = polys.clone();
    out.name = out_name(p, format!("{}_Summarize", polys.name));
    out.source = None;
    let fi = out.ensure_field("point_count", FieldType::Integer);
    let added = statistic_fields(pts, &requests, false, &out.fields)?;
    // Each polygon's row, shape and extent; a feature without polygons meets no point.
    let zones: Vec<(usize, MultiPolygon<f64>, Extent)> = out
        .features
        .iter()
        .enumerate()
        .filter_map(|(row, f)| {
            let mp = f.geometry.as_ref().and_then(to_multipolygon)?;
            let ext = merc_extent(&mp);
            Some((row, mp, ext))
        })
        .collect();
    let index = ExtentIndex::new(zones.iter().map(|(_, _, ext)| *ext));
    let mut counts = vec![0i64; out.features.len()];
    // Each polygon's points, kept only for statistics.
    let mut members: Vec<Vec<usize>> = if requests.is_empty() { Vec::new() } else { vec![Vec::new(); out.features.len()] };
    // The points in at least one polygon, each once: the ones the statistics read.
    let mut summarised: Vec<usize> = Vec::new();
    let mut cand_buf = Vec::new();
    for r in each_once(rows(inp, p, "points", pts)) {
        let Some(g) = pts.features.get(r).and_then(|f| f.geometry.as_ref()) else { continue };
        let pg = to_geo(g);
        let ext = merc_extent(&pg);
        let mut met = false;
        index.candidates_into(&ext, &mut cand_buf);
        for &zi in &cand_buf {
            let Some((row, mp, z_ext)) = zones.get(zi) else { continue };
            if z_ext.intersects(&ext)
                && mp.intersects(&pg)
                && let Some(c) = counts.get_mut(*row)
            {
                *c += 1;
                met = true;
                if let Some(m) = members.get_mut(*row) {
                    // One entry for each point in each polygon it meets: many overlapping polygons
                    // can make that more than memory holds, an error rather than an abort. The
                    // statistics then take each polygon's list as it is, one polygon at a time.
                    m.try_reserve(1).map_err(|e| {
                        format!("Summarize Within: not enough memory to keep each polygon's points for its statistics: {e}")
                    })?;
                    m.push(r);
                }
            }
        }
        if met && !requests.is_empty() {
            summarised.push(r);
        }
    }
    let total: i64 = counts.iter().sum();
    for (f, n) in out.features.iter_mut().zip(counts) {
        if let Some(v) = f.attrs.get_mut(fi) {
            *v = Value::Int(n);
        }
    }
    let mut rounded = Rounded::new(requests.len());
    let first = out.fields.len();
    if !requests.is_empty() {
        out.fields.extend(added);
        // A point in several overlapping polygons is read for each of them: counted over the
        // points, not the polygons' lists, each is counted once.
        rounded.count_texts(pts, &summarised, &requests, false);
        let width = out.fields.len();
        for (f, mut records) in out.features.iter_mut().zip(members) {
            if f.attrs.len() < width {
                f.attrs.resize(width, Value::Null);
            }
            record_order(&mut records);
            let values = statistic_values(pts, &records, &requests, false, separator, &mut rounded);
            for (i, v) in values.into_iter().enumerate() {
                if let Some(slot) = f.attrs.get_mut(first + i) {
                    *slot = v;
                }
            }
        }
    }
    let mut messages = vec![format!("Summarize Within: {total} points counted")];
    messages.extend(rounded_notice(&rounded, out.fields.get(first..).unwrap_or_default()));
    Ok(ToolResult { output: Output::Layer(out), messages })
}

/// A lon/lat line or ring as a `geo` line string, in degrees.
fn lonlat_line(coords: &[Coord]) -> geo::LineString<f64> {
    geo::LineString::new(coords.iter().map(|c| geo::Coord { x: c.x, y: c.y }).collect())
}

/// The ground area (square metres, holes taken out) and perimeter (metres, holes included) of a
/// lon/lat polygon geometry on the WGS 84 ellipsoid, as Calculate Geometry Attributes gives them;
/// zero for any other geometry.
fn ground_area_perimeter(g: &Geometry) -> (f64, f64) {
    use geo::GeodesicArea;
    let (mut a, mut per) = (0.0, 0.0);
    let Geometry::Polygon(polys) = g else { return (a, per) };
    for rings in polys {
        for (i, r) in rings.iter().enumerate() {
            let mut ring = lonlat_line(r);
            ring.close();
            // Orient in continuous longitude space, including small dateline crossings.
            // A step of a full turn (-180° to 180°, an edge along a parallel or the pole) is kept as
            // written: unwrapped the short way it would be no edge at all.
            let mut longitude = ring.0.first().map_or(0.0, |c| c.x);
            let mut full_turn = false;
            let mut previous = longitude;
            for c in &mut ring.0 {
                let step = c.x - previous;
                previous = c.x;
                if step.abs() >= 360.0 && step.is_finite() {
                    longitude += step;
                    full_turn = true;
                } else {
                    longitude += (c.x - longitude + 180.0).rem_euclid(360.0) - 180.0;
                }
                c.x = longitude;
            }
            let polar = ring.0.first().zip(ring.0.last()).is_some_and(|(a, b)| (a.x - b.x).abs() > 180.0);
            let orientable = !polar && ring.0.iter().all(|c| c.x.is_finite() && (-90.0..=90.0).contains(&c.y));
            let polygon = geo::Polygon::new(ring, vec![]);
            let (rp, ra) = if orientable {
                polygon.orient(Direction::Default).geodesic_perimeter_area_unsigned()
            } else {
                // Keep smaller polar caps; invalid coordinates bypass planar orientation.
                let (perimeter, area) = polygon.geodesic_perimeter_area_signed();
                (perimeter, area.abs())
            };
            // A ring with a full-turn edge is measured on the ellipsoid along its edges' parallels and
            // meridians, as the Measure Area panel does; the geodesic area above has no such edge.
            let ra =
                if full_turn { Some(rustgis_geom::wgs84_ring_perimeter_area_m2(r).1).filter(|m| m.is_finite()).unwrap_or(ra) } else { ra };
            a += if i == 0 { ra } else { -ra };
            per += rp;
        }
    }
    (a, per)
}

/// The area of a ring in square degrees of longitude and latitude, by the shoelace formula: zero
/// for a ring that has collapsed onto a line or point. Coordinates are taken from the first
/// vertex, so a ring far from the origin keeps the precision of its own size.
fn lonlat_ring_area(ring: &[Coord]) -> f64 {
    let Some(o) = ring.first() else { return 0.0 };
    let d = |c: &Coord| (c.x - o.x, c.y - o.y);
    (ring.iter().zip(ring.iter().cycle().skip(1)).map(|(a, b)| d(a).0 * d(b).1 - d(b).0 * d(a).1).sum::<f64>() / 2.0).abs()
}

/// The area (square metres, holes taken out) of a lon/lat polygon geometry on a sphere of the
/// Earth's radius, as the map draws it: each edge a straight line in Web Mercator, which is a rhumb
/// line, with longitudes as they are (not folded across the antimeridian) and latitudes clamped to
/// the map's limit, as the overlay takes them. A polygon cut along any straight line on the map
/// splits this area exactly, so the parts' areas add up to the whole's, up to the overlay's
/// rounding of the cut ([`OverlayGrid`]). Zero for any other geometry.
fn map_area_m2(g: &Geometry) -> f64 {
    let Geometry::Polygon(polys) = g else { return 0.0 };
    let r2 = rustgis_geom::EARTH_RADIUS_M * rustgis_geom::EARTH_RADIUS_M;
    polys.iter().flat_map(|rings| rings.iter().enumerate()).map(|(i, r)| if i == 0 { 1.0 } else { -1.0 } * rhumb_ring_area(r) * r2).sum()
}

/// The area of a ring of rhumb lines on the unit sphere, |∮ sin φ dλ|, for [`map_area_m2`]. The
/// integral is the area element cos φ dφ dλ taken round the ring by Green's theorem, the one
/// Chamberlain & Duquette (JPL, 2007) evaluate with edges straight in longitude and latitude
/// ([`rustgis_geom::geodesic_ring_area_m2`]). Along a rhumb line the isometric latitude
/// ψ = ln tan(π/4 + φ/2), the Mercator y over the radius (Snyder, *Map Projections: A Working
/// Manual*, USGS Professional Paper 1395, 1987, on the Mercator projection), changes linearly with
/// the longitude λ, and sin φ = tanh ψ, so an edge's integral is Δλ/Δψ × (ln cosh ψ₂ − ln cosh ψ₁);
/// for a short Δψ, where that difference cancels, its Taylor series about the middle. Longitudes
/// are not folded, as on the map, so the ring's Δλ add up to 0 and no ring winds around a pole: a
/// ring stored around one with a jump across the antimeridian is the flat shape the map draws.
fn rhumb_ring_area(ring: &[Coord]) -> f64 {
    let iso = |c: &Coord| c.y.clamp(-rustgis_geom::MAX_LAT, rustgis_geom::MAX_LAT).to_radians().tan().asinh();
    let ln_cosh = |x: f64| x.abs() + (-2.0 * x.abs()).exp().ln_1p() - std::f64::consts::LN_2;
    let mut total = 0.0;
    for (a, b) in ring.iter().zip(ring.iter().cycle().skip(1)) {
        let dlon = b.x - a.x;
        let (p1, p2) = (iso(a), iso(b));
        let dpsi = p2 - p1;
        let mean_sin = if dpsi.abs() < 1e-4 {
            // tanh over the edge, averaged: tanh m + Δψ²/24 × tanh″ m, with tanh″ = −2 tanh sech².
            let t = ((p1 + p2) / 2.0).tanh();
            t - dpsi * dpsi / 12.0 * t * (1.0 - t * t)
        } else {
            (ln_cosh(p2) - ln_cosh(p1)) / dpsi
        };
        total += dlon.to_radians() * mean_sin;
    }
    total.abs()
}

/// The overlay's grid for a feature cut by masks within a Web Mercator extent. The overlay keeps 29
/// integer bits about the centre of its operands' extent (see [`clip`]), a grid step of the extent
/// over 2^30, so it moves each vertex by less than a step: the area by less than the perimeter times
/// a step, and a sliver it leaves along an edge is no wider than a step.
struct OverlayGrid {
    /// The grid step, in Web Mercator metres, taken over the feature and all its masks.
    step: f64,
    /// How far rounding can move the feature's area, relative to it: twice the perimeter times the
    /// step over the area, from 1e-9 to 1. A part closer than this to the whole may be the whole.
    rounding: f64,
}

impl OverlayGrid {
    /// The grid for the polygon feature `g` cut by masks within `domain`.
    fn new(g: &Geometry, domain: &Extent) -> Self {
        let mp = to_multipolygon(g).unwrap_or_else(|| MultiPolygon::new(vec![]));
        let ext = domain.union(&merc_extent(&mp));
        let step = ext.width().max(ext.height()) / f64::from(1_u32 << 30);
        let (area, perimeter) = merc_area_perimeter(&mp);
        let rounding = if area > 0.0 && step.is_finite() { (2.0 * perimeter * step / area).clamp(1e-9, 1.0) } else { 1e-9 };
        OverlayGrid { step: if step.is_finite() { step } else { 0.0 }, rounding }
    }

    /// Whether the grid measures the feature finely: rounding can move its area by at most 1%.
    fn resolves(&self) -> bool {
        self.rounding <= 0.01
    }

    /// Whether a polygon part is wider on average than the grid step: more than the slivers
    /// rounding leaves along an edge, which are less than a step wide. A part's average width is
    /// twice its area over its perimeter (a long strip's perimeter is twice its length).
    fn wider(&self, g: &Geometry) -> bool {
        to_multipolygon(g).is_some_and(|mp| {
            let (area, perimeter) = merc_area_perimeter(&mp);
            2.0 * area > perimeter * self.step
        })
    }
}

/// The planar area and perimeter (holes included) of polygons in Web Mercator metres.
fn merc_area_perimeter(mp: &MultiPolygon<f64>) -> (f64, f64) {
    use geo::{Area, Euclidean, Length};
    let perimeter = mp.0.iter().flat_map(|p| std::iter::once(p.exterior()).chain(p.interiors())).map(|r| Euclidean.length(r)).sum();
    (mp.unsigned_area(), perimeter)
}

/// Calculate Geometry Attributes: adds `area_km2` and `perimeter_km` to a polygon layer (see
/// [`ground_area_perimeter`]), `length_km` to a line layer, and `point_x`/`point_y` (a label point)
/// to any other, updating the layer in place.
fn calculate_geometry(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    use geo::{Geodesic, Length};
    let ds = input(inp, p, "input")?;
    let mut out = ds.clone();
    match ds.geometry_type {
        GeometryType::Polygon => {
            let fa = out.ensure_field("area_km2", FieldType::Double);
            let fp = out.ensure_field("perimeter_km", FieldType::Double);
            for f in &mut out.features {
                let (a, per) = f.geometry.as_ref().map_or((0.0, 0.0), ground_area_perimeter);
                if let Some(v) = f.attrs.get_mut(fa) {
                    *v = Value::Float(a / 1e6);
                }
                if let Some(v) = f.attrs.get_mut(fp) {
                    *v = Value::Float(per / 1e3);
                }
            }
        }
        GeometryType::Polyline => {
            let fl = out.ensure_field("length_km", FieldType::Double);
            for f in &mut out.features {
                let len: f64 = match &f.geometry {
                    Some(Geometry::Line(parts)) => parts.iter().map(|p| Geodesic.length(&lonlat_line(p))).sum(),
                    _ => 0.0,
                };
                if let Some(v) = f.attrs.get_mut(fl) {
                    *v = Value::Float(len / 1e3);
                }
            }
        }
        _ => {
            let fx = out.ensure_field("point_x", FieldType::Double);
            let fy = out.ensure_field("point_y", FieldType::Double);
            for f in &mut out.features {
                let c = f.geometry.as_ref().and_then(|g| g.label_point());
                if let Some(v) = f.attrs.get_mut(fx) {
                    *v = c.map(|c| Value::Float(c.x)).unwrap_or(Value::Null);
                }
                if let Some(v) = f.attrs.get_mut(fy) {
                    *v = c.map(|c| Value::Float(c.y)).unwrap_or(Value::Null);
                }
            }
        }
    }
    Ok(ToolResult {
        output: Output::Update { layer: ds.name.clone(), dataset: out },
        messages: vec![format!("Calculated geometry attributes for {}", ds.name)],
    })
}

/// The longest separator CONCATENATE joins values with, in bytes: each value adds one, so a long
/// one would multiply the output's size.
const MAX_SEPARATOR: usize = 256;

/// Summary Statistics: a row for each group of records sharing the case fields' values, holding
/// the case fields, `FREQUENCY` (the group's records) and each requested statistic as
/// `<STAT>_<field>`. Without `statistics`, `field` takes COUNT, SUM, MEAN, MIN, MAX and STD, its
/// values read as numbers whatever its type, as before the statistics list.
fn summary_statistics(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    use crate::stats::{self, Request, Stat};
    let ds = input(inp, p, "input")?;
    let mut requests = stats::parse_requests(ds, p.get("statistics"))?;
    let field_only = requests.is_empty();
    if field_only {
        let field = s(p, "field").trim();
        if field.is_empty() {
            return Err("give the statistics to compute, or a Statistics Field".into());
        }
        let fi = find_field_case_insensitive(ds, field)?;
        requests = [Stat::Count, Stat::Sum, Stat::Mean, Stat::Min, Stat::Max, Stat::Std].map(|stat| Request { field: fi, stat }).to_vec();
    }
    let cases = key_fields(ds, p.get("case"), "Case Field")?;
    let separator = concatenate_separator(p)?;
    let mut fields: Vec<Field> = cases.iter().filter_map(|&c| ds.fields.get(c).cloned()).collect();
    fields.push(Field::new("FREQUENCY", FieldType::Integer));
    let added = statistic_fields(ds, &requests, field_only, &fields)?;
    let first_added = fields.len();
    fields.extend(added);
    if let Some(c) = cases.iter().filter_map(|&c| ds.fields.get(c)).find(|f| f.name.eq_ignore_ascii_case("FREQUENCY")) {
        return Err(format!("the case field '{}' has the name of the output's FREQUENCY field", c.name));
    }
    let mut out = Dataset::new(format!("{}_Statistics", ds.name), GeometryType::None, fields);
    // Record order, each record once, whatever order the selection lists them in: FIRST, LAST and
    // CONCATENATE follow it.
    let mut rs = rows(inp, p, "input", ds);
    rs.sort_unstable();
    rs.dedup();
    let mut rounded = Rounded::new(requests.len());
    rounded.count_texts(ds, &rs, &requests, field_only);
    for group in stats::group_rows(ds, &rs, &cases) {
        let mut row = group.key;
        row.push(stats::count(group.rows.len()));
        row.extend(statistic_values(ds, &group.rows, &requests, field_only, separator, &mut rounded));
        // One past the last id: `push` would search them all for each group.
        out.push_next(None, row);
    }
    let n = out.features.len();
    let mut messages = vec![format!("Summary Statistics: {n} rows")];
    messages.extend(rounded_notice(&rounded, out.fields.get(first_added..).unwrap_or_default()));
    Ok(ToolResult { output: Output::Table(out), messages })
}

/// The output fields of a statistics list over `ds`: `<STAT>_<field>` for each request, typed by
/// the statistic and the field (numeric when `as_numbers`). A name that differs only in case from
/// one in `taken` (the tool's other output fields) or from another statistic's is an error, not a
/// renamed field.
fn statistic_fields(ds: &Dataset, requests: &[crate::stats::Request], as_numbers: bool, taken: &[Field]) -> Result<Vec<Field>, String> {
    let mut out: Vec<Field> = Vec::with_capacity(requests.len());
    for r in requests {
        let (name, numeric) = ds.fields.get(r.field).map(|f| (f.name.as_str(), f.ty.is_numeric())).unwrap_or_default();
        let field = Field::new(format!("{}_{name}", r.stat.name()), r.stat.output_type(numeric || as_numbers));
        if taken.iter().chain(&out).any(|f| f.name.eq_ignore_ascii_case(&field.name)) {
            return Err(format!("the output would have two fields named '{}'", field.name));
        }
        out.push(field);
    }
    Ok(out)
}

/// What a statistics list lost to decimals, counted over every group for [`rounded_notice`]. Pivot
/// Table counts its statistic's results by pivot field, as one request each.
struct Rounded {
    /// For each request, how many of its results came out rounded from a whole number past 2^53.
    results: Vec<usize>,
    /// For each input field read as numbers that holds some, by name, how many of its values were
    /// whole numbers written as text past the i64 range, read as decimals.
    texts: Vec<(String, usize)>,
}

impl Rounded {
    /// Nothing counted yet, for `requests` requests.
    fn new(requests: usize) -> Rounded {
        Rounded { results: vec![0; requests], texts: Vec::new() }
    }

    /// Adds, for each field `requests` read as numbers from `ds`, how many of its values among the
    /// records `rows` are whole numbers written as text past the i64 range, read as decimals.
    /// Called once over the records a tool reads, each once, so a value is counted once however
    /// many groups (Summarize Within's polygons) it is summarised in.
    fn count_texts(&mut self, ds: &Dataset, rows: &[usize], requests: &[crate::stats::Request], as_numbers: bool) {
        // Each field once, in the requests' order, which names them in the notice.
        let mut fields: Vec<usize> = Vec::new();
        for r in requests {
            if !fields.contains(&r.field) {
                fields.push(r.field);
            }
        }
        for field in fields {
            let n = crate::stats::column(ds, rows, field, as_numbers).rounded_texts();
            if n == 0 {
                continue;
            }
            let name = ds.fields.get(field).map(|f| f.name.as_str()).unwrap_or_default();
            match self.texts.iter_mut().find(|(f, _)| f == name) {
                Some((_, count)) => *count += n,
                None => self.texts.push((name.to_string(), n)),
            }
        }
    }
}

/// The values of a statistics list over the records `rows` of `ds`, one per request, in the
/// requests' order. `rows` must be in record order, each once: FIRST, LAST and CONCATENATE follow
/// it. Each field's values are read once, however many of its statistics are asked for. Each
/// value that comes out rounded from whole numbers past 2^53 adds one to its request's count in
/// `rounded`, for [`rounded_notice`]; the whole numbers written as text that reading a field as
/// numbers rounds are counted by [`Rounded::count_texts`], per record rather than per group.
fn statistic_values(
    ds: &Dataset,
    rows: &[usize],
    requests: &[crate::stats::Request],
    as_numbers: bool,
    separator: &str,
    rounded: &mut Rounded,
) -> Vec<Value> {
    use crate::stats;
    let mut read: Vec<(usize, stats::Column)> = Vec::new();
    let mut out = Vec::with_capacity(requests.len());
    for r in requests {
        let i = match read.iter().position(|(f, _)| *f == r.field) {
            Some(i) => i,
            None => {
                read.push((r.field, stats::column(ds, rows, r.field, as_numbers)));
                read.len() - 1
            }
        };
        let value = read.get(i).map_or(Value::Null, |(_, c)| {
            let v = stats::compute(r.stat, c, separator);
            if stats::rounded(r.stat, c, &v)
                && let Some(n) = rounded.results.get_mut(out.len())
            {
                *n += 1;
            }
            v
        });
        out.push(value);
    }
    out
}

/// The notices for what a statistics list lost to decimals: the statistics that came out rounded,
/// naming their output fields `fields` (one per request, as `rounded` counts them), as a decimal
/// field holds whole numbers exactly only up to 2^53; and the whole numbers written as text past
/// the i64 range that were read as decimals, whose statistics aren't checked, naming their input
/// fields. Empty when nothing was rounded.
fn rounded_notice(rounded: &Rounded, fields: &[Field]) -> Vec<String> {
    let mut notices = Vec::new();
    let total: usize = rounded.results.iter().sum();
    if total > 0 {
        let names: Vec<&str> = rounded.results.iter().zip(fields).filter(|(n, _)| **n > 0).map(|(_, f)| f.name.as_str()).collect();
        let results = if total == 1 { "result" } else { "results" };
        notices.push(format!(
            "{total} {results} rounded in {}: a decimal field holds whole numbers exactly only up to 2^53 (9007199254740992)",
            names.join(", ")
        ));
    }
    let total: usize = rounded.texts.iter().map(|(_, n)| n).sum();
    if total > 0 {
        let names: Vec<&str> = rounded.texts.iter().map(|(f, _)| f.as_str()).collect();
        let (numbers, are, were, decimals, them) =
            if total == 1 { ("number", "is", "was", "a decimal", "it") } else { ("numbers", "are", "were", "decimals", "them") };
        notices.push(format!(
            "{total} whole {numbers} written as text in {} {are} past the 64-bit integer range and {were} read as {decimals}, so statistics of {them} may be rounded",
            names.join(", ")
        ));
    }
    notices
}

/// The statistics list of a tool that adds statistics to its own output (Dissolve, Summarize
/// Within), read from `statistics` over the fields of `ds`, with the separator CONCATENATE joins
/// values with. The separator is read only with a list: a run without one ignores it.
fn summary_requests<'a>(ds: &Dataset, p: &'a Json) -> Result<(Vec<crate::stats::Request>, &'a str), String> {
    let requests = crate::stats::parse_requests(ds, p.get("statistics"))?;
    let separator = if requests.is_empty() { DEFAULT_SEPARATOR } else { concatenate_separator(p)? };
    Ok((requests, separator))
}

/// `rows`, as [`rows`] gives them, with each record once, at its first place: a selection that
/// lists a record twice still counts it once. Rows in increasing order (all of a layer's, or a
/// selection made in the app) hold each record once already and are returned as they are; any
/// other selection is deduplicated through a set its own size, not the layer's.
fn each_once(rows: Vec<usize>) -> Vec<usize> {
    if rows.windows(2).all(|w| matches!(w, [a, b] if a < b)) {
        return rows;
    }
    let mut seen = HashSet::with_capacity(rows.len());
    rows.into_iter().filter(|&r| seen.insert(r)).collect()
}

/// Puts `rows` in record order, each once, as [`statistic_values`] takes them, in place.
fn record_order(rows: &mut Vec<usize>) {
    rows.sort_unstable();
    rows.dedup();
}

/// The key fields a table tool groups by, such as Summary Statistics' case fields: one field's name,
/// several as a list, or none. `label` is the parameter's label, singular, for messages. A field
/// named twice is an error.
fn key_fields(ds: &Dataset, v: Option<&Json>, label: &str) -> Result<Vec<usize>, String> {
    let lower = label.to_lowercase();
    let names: Vec<&str> = match v {
        None | Some(Json::Null) => Vec::new(),
        Some(Json::String(t)) => vec![t.trim()],
        Some(Json::Array(items)) => items
            .iter()
            .map(|i| i.as_str().map(str::trim).ok_or_else(|| format!("{label}s must be field names")))
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(format!("{label} must be a field name or a list of them")),
    };
    let mut out = Vec::with_capacity(names.len());
    for name in names.into_iter().filter(|n| !n.is_empty()) {
        let c = find_field_case_insensitive(ds, name)?;
        if out.contains(&c) {
            return Err(format!("{lower} '{name}' is listed twice"));
        }
        // Output names must differ in more than case, for lookups that ignore it.
        let same = |o: &usize| ds.fields.get(*o).zip(ds.fields.get(c)).is_some_and(|(a, b)| a.name.eq_ignore_ascii_case(&b.name));
        if out.iter().any(same) {
            return Err(format!("{lower}s can't differ only in case ('{name}')"));
        }
        out.push(c);
    }
    Ok(out)
}

/// The separator CONCATENATE joins values with unless one is given.
const DEFAULT_SEPARATOR: &str = ", ";

/// The separator CONCATENATE joins values with: [`DEFAULT_SEPARATOR`] unless given. A bad one is
/// refused whatever the statistics (Dissolve and Summarize Within read it only with a statistics
/// list); the Tools pane leaves it out of runs without CONCATENATE.
fn concatenate_separator(p: &Json) -> Result<&str, String> {
    match p.get("separator") {
        None | Some(Json::Null) => Ok(DEFAULT_SEPARATOR),
        Some(Json::String(t)) if t.len() <= MAX_SEPARATOR => Ok(t.as_str()),
        Some(Json::String(_)) => Err(format!("Separator can be at most {MAX_SEPARATOR} bytes")),
        Some(_) => Err("Separator must be text".into()),
    }
}

/// The most columns Pivot Table makes from the pivot field's values: each is an output field.
const MAX_PIVOT_COLUMNS: usize = 1000;

/// The most cells (rows times output fields: the row fields and the pivot fields) Pivot Table
/// makes: its output is held in memory.
const MAX_PIVOT_CELLS: usize = 10_000_000;

/// The most characters of a pivot value Pivot Table keeps in a field name, besides the pivot
/// field's name and any suffix that tells two names apart.
const MAX_PIVOT_NAME: usize = 64;

/// Pivot Table: a row for each combination of the row fields' values (one row without them), then
/// a field for each value of the pivot field, sorted by value, holding the value field's values in
/// that row. Without a statistic, a row and column can hold one record at most, and its value is
/// copied with the value field's type; with one, the statistic combines that cell's records, as in
/// Summary Statistics, which says how many cells came out rounded from whole numbers past 2^53. A
/// cell with no records is null. A record without a pivot value is an error.
fn pivot_table(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    use crate::stats::{self, Request, Stat};
    let ds = input(inp, p, "input")?;
    let keys = key_fields(ds, p.get("fields"), "Row Field")?;
    let named = |k: &str, label: &str| -> Result<usize, String> {
        if p.get(k).is_some_and(|v| !v.is_string() && !v.is_null()) {
            return Err(format!("'{label}' must be a field name"));
        }
        let name = s(p, k).trim();
        if name.is_empty() {
            return Err(format!("'{label}' is required"));
        }
        find_field_case_insensitive(ds, name)
    };
    let pivot = named("pivot", "Pivot Field")?;
    let value = named("value", "Value Field")?;
    let field_name = |i: usize| ds.fields.get(i).map_or("", |f| f.name.as_str());
    if keys.contains(&pivot) {
        return Err(format!("the pivot field '{}' can't also be a row field", field_name(pivot)));
    }
    let value_type = ds.fields.get(value).map_or(FieldType::Text, |f| f.ty);
    if p.get("statistic").is_some_and(|v| !v.is_string() && !v.is_null()) {
        return Err("Statistic must be text".into());
    }
    let stat = match s(p, "statistic").trim() {
        "" => None,
        name => {
            let stat = Stat::parse(name).ok_or_else(|| format!("Statistic must be one of {} (not '{name}')", stats::STAT_NAMES))?;
            if !value_type.is_numeric() && !stat.fits_text() {
                return Err(format!("{} needs a numeric value field ('{}' is not)", stat.name(), field_name(value)));
            }
            Some(stat)
        }
    };
    let separator = concatenate_separator(p)?;
    // Record order, each record once, as FIRST, LAST and CONCATENATE follow it.
    let mut rs = rows(inp, p, "input", ds);
    rs.sort_unstable();
    rs.dedup();
    let columns = stats::group_rows(ds, &rs, &[pivot]);
    let missing: usize = columns.iter().filter(|c| c.key.first().is_none_or(no_pivot_value)).map(|c| c.rows.len()).sum();
    if missing > 0 {
        let records = if missing == 1 { "1 record".to_string() } else { format!("{missing} records") };
        return Err(format!("the pivot field '{}' has no value in {records}: fill them in, or select the others", field_name(pivot)));
    }
    if columns.len() > MAX_PIVOT_COLUMNS {
        return Err(format!(
            "the pivot field '{}' has {} values, which would make more than {MAX_PIVOT_COLUMNS} fields",
            field_name(pivot),
            columns.len()
        ));
    }
    let groups = stats::group_rows(ds, &rs, &keys);
    let width = keys.len() + columns.len();
    if groups.len().saturating_mul(width) > MAX_PIVOT_CELLS {
        return Err(format!("{} rows by {width} fields would make more than {MAX_PIVOT_CELLS} values; select fewer records", groups.len()));
    }
    let mut fields: Vec<Field> = keys.iter().filter_map(|&k| ds.fields.get(k).cloned()).collect();
    let ty = stat.map_or(value_type, |st| st.output_type(value_type.is_numeric()));
    let values = columns.iter().map(|c| c.key.first().unwrap_or(&Value::Null));
    let names = pivot_field_names(field_name(pivot), values, &fields);
    let first_pivot_field = fields.len();
    fields.extend(names.into_iter().map(|name| Field::new(name, ty)));
    // Cells rounded from whole numbers past 2^53, counted for each pivot field, and the whole
    // numbers written as text past the i64 range that a statistic read as decimals, each once.
    let mut rounded = Rounded::new(columns.len());
    if let Some(stat) = stat {
        rounded.count_texts(ds, &rs, &[Request { field: value, stat }], false);
    }
    let mut column_of: HashMap<usize, usize> = HashMap::with_capacity(rs.len());
    for (c, column) in columns.iter().enumerate() {
        column_of.extend(column.rows.iter().map(|&r| (r, c)));
    }
    let mut out = Dataset::new(out_name(p, format!("{}_Pivot", ds.name)), GeometryType::None, fields);
    for group in groups {
        // The group's records by column; the sort is stable, so each cell keeps record order.
        let mut cells: Vec<(usize, usize)> = group.rows.iter().filter_map(|&r| column_of.get(&r).map(|&c| (c, r))).collect();
        cells.sort_by_key(|&(c, _)| c);
        let mut row = group.key;
        // Where the pivot fields start, after the row fields.
        let pivot_start = row.len();
        for cell in cells.chunk_by(|a, b| a.0 == b.0) {
            let Some(&(c, r)) = cell.first() else { continue };
            let v = match stat {
                None if cell.len() > 1 => {
                    let mut cell_key: Vec<String> = keys.iter().zip(&row).map(|(&k, v)| format!("{} = {v}", field_name(k))).collect();
                    cell_key.push(format!("{} = {}", field_name(pivot), ds.value(r, pivot)));
                    return Err(format!(
                        "{} records have {}: give a Statistic to combine them, or choose Row Fields that tell them apart",
                        cell.len(),
                        cell_key.join(", ")
                    ));
                }
                None => ds.value(r, value).clone(),
                Some(st) => {
                    let records: Vec<usize> = cell.iter().map(|&(_, r)| r).collect();
                    let column = stats::column(ds, &records, value, false);
                    let v = stats::compute(st, &column, separator);
                    if stats::rounded(st, &column, &v)
                        && let Some(n) = rounded.results.get_mut(c)
                    {
                        *n += 1;
                    }
                    v
                }
            };
            row.resize(pivot_start + c, Value::Null);
            row.push(v);
        }
        row.resize(pivot_start + columns.len(), Value::Null);
        // One past the last id: `push` would search them all for each row.
        out.push_next(None, row);
    }
    let n = out.features.len();
    let mut messages = vec![format!("Pivot Table: {n} rows, {} pivot fields", columns.len())];
    messages.extend(rounded_notice(&rounded, out.fields.get(first_pivot_field..).unwrap_or_default()));
    Ok(ToolResult { output: Output::Table(out), messages })
}

/// Whether a pivot value is missing: null, or a number that isn't one.
fn no_pivot_value(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Float(f) => f.is_nan(),
        _ => false,
    }
}

/// The names of the fields Pivot Table makes for the pivot field `field`'s `values`, in order,
/// after the fields `before` it: each value's [`pivot_column_name`], with a suffix (`_2`, `_3`, …)
/// for one already taken, ignoring case, in the order of the values.
fn pivot_field_names<'a>(field: &str, values: impl Iterator<Item = &'a Value>, before: &[Field]) -> Vec<String> {
    let mut taken: HashSet<String> = before.iter().map(|f| f.name.to_ascii_lowercase()).collect();
    let mut out = Vec::new();
    for v in values {
        let base = pivot_column_name(field, v);
        let mut name = base.clone();
        let mut n = 2;
        while !taken.insert(name.to_ascii_lowercase()) {
            name = format!("{base}_{n}");
            n += 1;
        }
        out.push(name);
    }
    out
}

/// The name of the field Pivot Table makes for a value of the pivot field `field`. Letters, digits
/// and underscores are kept, and each run of other characters becomes one underscore, or none right
/// after an underscore (`North_-east` gives `North_east`); text also loses underscores at its ends.
/// At most [`MAX_PIVOT_NAME`] characters of the value are kept. A number (in full) or true/false
/// follows the field's name (`year2020`, `rate2_5`, `temp_3` for −3); so does text that would start
/// with a digit or have nothing left.
fn pivot_column_name(field: &str, v: &Value) -> String {
    // A decimal in full (the shortest text that reads back as it), not rounded as values are shown.
    let shown = match v {
        Value::Float(f) => f.to_string(),
        _ => v.to_string(),
    };
    let mut name = String::new();
    for ch in shown.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            name.push(ch);
        } else if !name.ends_with('_') {
            name.push('_');
        }
    }
    let text = matches!(v, Value::Text(_));
    // Cut, then trimmed, so a cut at a run of other characters leaves no underscore at the end.
    let kept: String = name.chars().take(MAX_PIVOT_NAME).collect();
    let name = if text { kept.trim_matches('_').to_string() } else { kept };
    if !text || name.is_empty() || name.starts_with(char::is_numeric) { format!("{field}{name}") } else { name }
}

/// A zone of Tabulate Intersection: the zone features sharing the zone fields' values, as one mask.
struct TabulateZone {
    /// The zone fields' values.
    key: Vec<Value>,
    /// The lon/lat extent of its features, for the index.
    ext: Extent,
    /// Its features' polygons in Web Mercator, unioned when there are several, as [`mask_of`] does.
    mask: MultiPolygon<f64>,
    /// The Web Mercator extent of its mask, for [`OverlayGrid`].
    merc_ext: Extent,
    /// Its area in square metres as the map draws it ([`map_area_m2`]), overlaps between its
    /// features counted once.
    map_area_m2: f64,
}

/// What one class covers of one zone: its class features' parts in the zone, and the shares of
/// their sum fields' values, in the order of the sum fields.
struct TabulateCell {
    parts: Vec<Geometry>,
    sums: Vec<f64>,
}

/// Tabulate Intersection: a row for each zone and class that overlap, holding the zone fields, the
/// class fields, `area_km2` (the ground area of the class in the zone, class features overlapping
/// each other counted once), `percentage` (that area as a percentage of the zone's) and each sum
/// field's share: the sum, over the class's features, of the field's value times the part of the
/// feature's area inside the zone. Rows are sorted by the zone fields' values, then the class
/// fields', as Summary Statistics sorts its groups; pairs that only touch get none. Only selected
/// features are used when a layer has a selection. The messages count features without a polygon
/// (left out: no geometry, an empty one, a line or a point), zones with no rows, class features
/// with no value in a sum field and, with sum fields, class features not wholly inside the zones,
/// whose values are not wholly shared out.
///
/// `area_km2` is measured on the ellipsoid, as Calculate Geometry Attributes measures it. The
/// percentages and shares compare areas measured as the map draws the polygons instead
/// ([`map_area_m2`]): the overlay cuts along straight lines on the map, which that measure splits
/// exactly and the ellipsoid's geodesic edges don't, so a feature's shares in zones that cover it
/// without overlapping each other add up to its whole value, up to the overlay's rounding: a part
/// that leaves nothing outside its zone wider than the overlay's grid is the whole ([`OverlayGrid`]).
fn tabulate_intersection(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let zones = input(inp, p, "zones")?;
    let classes = input(inp, p, "classes")?;
    for ds in [zones, classes] {
        if ds.geometry_type != GeometryType::Polygon {
            return Err(format!("'{}' is not a polygon layer", ds.name));
        }
    }
    let zone_keys = key_fields(zones, p.get("zoneFields"), "Zone Field")?;
    if zone_keys.is_empty() {
        return Err("'Zone Fields' is required".into());
    }
    let class_keys = key_fields(classes, p.get("classFields"), "Class Field")?;
    let sum_fields = key_fields(classes, p.get("sumFields"), "Sum Field")?;
    for &f in &sum_fields {
        let Some(field) = classes.fields.get(f) else { continue };
        if !field.ty.is_numeric() {
            return Err(format!("the sum field '{}' is not numeric", field.name));
        }
        if class_keys.contains(&f) {
            return Err(format!("the sum field '{}' can't also be a class field", field.name));
        }
    }
    // Record order, each record once, whatever order a selection lists them in.
    let sorted_rows = |k: &str, ds: &Dataset| {
        let mut rs = rows(inp, p, k, ds);
        rs.sort_unstable();
        rs.dedup();
        rs
    };
    // A feature without geometry, with an empty one (`"coordinates": []`), with a line or point in
    // a polygon layer, or whose polygons have no area in its own coordinates (a collapsed ring) has
    // no polygon, so no area to tabulate. The area is taken in longitude and latitude, so a polygon
    // the map's latitude limit flattens still counts as one.
    let has_shape = |g: &Geometry| matches!(g, Geometry::Polygon(polys) if polys.iter().any(|rings| rings.first().is_some_and(|r| lonlat_ring_area(r) > 0.0)));
    let shapeless = |ds: &Dataset, rs: &[usize]| {
        rs.iter().filter(|&&r| ds.features.get(r).and_then(|f| f.geometry.as_ref()).is_none_or(|g| !has_shape(g))).count()
    };
    let zone_rows = sorted_rows("zones", zones);
    let class_rows = sorted_rows("classes", classes);
    let (zones_shapeless, classes_shapeless) = (shapeless(zones, &zone_rows), shapeless(classes, &class_rows));
    // The map ends at the latitude limit: the overlay takes parts beyond it as lying along it.
    let beyond_limit = |ds: &Dataset, rs: &[usize]| {
        let beyond = |g: &Geometry| {
            let e = g.extent();
            has_shape(g) && (e.max_y > rustgis_geom::MAX_LAT || e.min_y < -rustgis_geom::MAX_LAT)
        };
        rs.iter().filter(|&&r| ds.features.get(r).and_then(|f| f.geometry.as_ref()).is_some_and(beyond)).count()
    };
    let (zones_beyond, classes_beyond) = (beyond_limit(zones, &zone_rows), beyond_limit(classes, &class_rows));
    // Features with a ring that jumps across the antimeridian unsplit: the overlay, like the map,
    // takes such a ring the long way round, while `area_km2` takes its ground shape.
    let unsplit = |ds: &Dataset, rs: &[usize]| {
        let jumps = |g: &Geometry| {
            has_shape(g)
                && matches!(g, Geometry::Polygon(polys) if polys.iter().flatten().any(|r| {
                    r.iter().zip(r.iter().cycle().skip(1)).any(|(a, b)| (b.x - a.x).abs() > 180.0)
                }))
        };
        rs.iter().filter(|&&r| ds.features.get(r).and_then(|f| f.geometry.as_ref()).is_some_and(jumps)).count()
    };
    let (zones_unsplit, classes_unsplit) = (unsplit(zones, &zone_rows), unsplit(classes, &class_rows));
    let zone_list: Vec<TabulateZone> = crate::stats::group_rows(zones, &zone_rows, &zone_keys)
        .into_iter()
        .map(|group| {
            let shapes: Vec<&Geometry> =
                group.rows.iter().filter_map(|&r| zones.features.get(r)?.geometry.as_ref()).filter(|g| has_shape(g)).collect();
            let ext = shapes.iter().fold(Extent::EMPTY, |e, g| e.union(&g.extent()));
            let mask = mask_of(MultiPolygon::new(shapes.iter().filter_map(|g| to_multipolygon(g)).flat_map(|m| m.0).collect()));
            let map_area = from_multipolygon(&mask).map_or(0.0, |g| map_area_m2(&g));
            let merc_ext = merc_extent(&mask);
            TabulateZone { key: group.key, ext, mask, merc_ext, map_area_m2: map_area }
        })
        .collect();
    let class_groups = crate::stats::group_rows(classes, &class_rows, &class_keys);
    let index = ExtentIndex::new(zone_list.iter().map(|z| z.ext));
    let mut cells: std::collections::BTreeMap<(usize, usize), TabulateCell> = std::collections::BTreeMap::new();
    // For each sum field, the class features in some zone whose value isn't a number.
    let mut missing = vec![0usize; sum_fields.len()];
    // With sum fields, the class features whose shares come to less than their whole value.
    let mut outside = 0usize;
    // The class features meeting a zone, not wholly inside one, whose area the overlay's rounding
    // can move by more than 1%: their parts are measured only roughly.
    let mut unresolved = 0usize;
    // The unions of the masks of the zones near a feature, for the outside check, by the zones'
    // indices: neighbouring features near the same zones reuse them, as Clip reuses its masks.
    const MAX_CACHED_UNIONS: usize = 8;
    let mut near_unions: std::collections::VecDeque<(Vec<usize>, MultiPolygon<f64>)> = std::collections::VecDeque::new();
    for (ci, group) in class_groups.iter().enumerate() {
        for &r in &group.rows {
            let Some(g) = classes.features.get(r).and_then(|f| f.geometry.as_ref()).filter(|g| has_shape(g)) else { continue };
            let ext = g.extent();
            let near: Vec<usize> =
                index.candidates(&ext).into_iter().filter(|&zi| zone_list.get(zi).is_some_and(|z| z.ext.intersects(&ext))).collect();
            if near.is_empty() {
                outside += usize::from(!sum_fields.is_empty());
                continue;
            }
            // Overlapping parts of one feature are unioned once, so they count once.
            let whole = dissolved(g);
            let values: Vec<Option<f64>> = sum_fields.iter().map(|&f| classes.value(r, f).as_f64()).collect();
            let mut whole_area: Option<f64> = None;
            // The overlay's grid for this feature and all the zones near it, for the outside check,
            // which cuts it by their union: see [`OverlayGrid`].
            let near_merc = near.iter().filter_map(|&zi| zone_list.get(zi)).fold(Extent::EMPTY, |e, z| e.union(&z.merc_ext));
            let mut grid: Option<OverlayGrid> = None;
            let resolved = grid.get_or_insert_with(|| OverlayGrid::new(&whole, &near_merc)).resolves();
            let mut inside = false;
            // Whether one zone holds the whole feature, so none of it lies outside them all; and
            // whether a zone got a part of it as cut, rounded, not the whole.
            let mut held = false;
            let mut cut_part = false;
            for &zi in &near {
                let Some(zone) = zone_list.get(zi) else { continue };
                // A cut that leaves nothing, or nothing with area, when the feature's interior meets
                // the zone's lost that part to rounding. Only a feature the grid doesn't resolve can
                // lose more than that rounding allows, so only for one is the zone asked, by the
                // exact predicate: if it covers the feature, the part is the whole feature; if it
                // only meets it, the feature is measured roughly, as for a part as cut.
                let total = *whole_area.get_or_insert_with(|| map_area_m2(&whole));
                let lost = |cut_part: &mut bool| -> Option<Geometry> {
                    if resolved {
                        return None;
                    }
                    let m = to_multipolygon(&whole).map(|w| zone.mask.relate(&w))?;
                    if m.is_covers() && total > 0.0 {
                        return Some(whole.as_ref().clone());
                    }
                    *cut_part |= m.is_intersects() && !m.is_touches();
                    None
                };
                let cut = part(&whole, &zone.mask, true).map(|p| (map_area_m2(&p), p)).filter(|(a, _)| *a > 0.0 && a.is_finite());
                let (mut piece, mut piece_area) = match cut {
                    Some((a, p)) => (p, a),
                    None => match lost(&mut cut_part) {
                        Some(w) => (w, total),
                        None => continue,
                    },
                };
                inside = true;
                // A part within the overlay's rounding of the whole may be the whole feature, the cut
                // only moving its vertices onto the grid: it is when nothing lies outside the zone,
                // or, for a feature the grid resolves, nothing wider than the grid. (For one it
                // doesn't, what lies outside can be as narrow as the feature itself.) Then the part
                // is the whole, unrounded. The grid is the one this cut used, over the feature and
                // this zone alone, so a wide zone nearby doesn't coarsen it.
                let zone_grid = OverlayGrid::new(&whole, &zone.merc_ext);
                if total > 0.0
                    && piece_area >= total * (1.0 - zone_grid.rounding)
                    && part(&whole, &zone.mask, false).is_none_or(|rest| zone_grid.resolves() && !zone_grid.wider(&rest))
                {
                    piece = whole.as_ref().clone();
                    piece_area = total;
                    held = true;
                } else {
                    cut_part = true;
                }
                let cell = cells.entry((zi, ci)).or_insert_with(|| TabulateCell { parts: Vec::new(), sums: vec![0.0; sum_fields.len()] });
                if !sum_fields.is_empty() {
                    // The part inside can come out a rounding error larger than the whole.
                    let share = if total > 0.0 { (piece_area / total).min(1.0) } else { 0.0 };
                    for (sum, v) in cell.sums.iter_mut().zip(&values) {
                        if let Some(v) = v {
                            *sum += v * share;
                        }
                    }
                }
                cell.parts.push(piece);
            }
            if inside {
                for (m, v) in missing.iter_mut().zip(&values) {
                    *m += usize::from(v.is_none());
                }
            }
            // With sum fields, a feature none of whose value is shared out, or with a part outside
            // every zone wider than the overlay's grid, isn't wholly shared out: what is left of it
            // once the zones near it, unioned, are taken away. Zones that overlap each other can't
            // hide such a part, as shares adding up to the whole could.
            // Whether the feature's interior meets a zone's, by the exact predicate rather than the
            // overlay: one near a zone only by its extent meets none, and one too narrow for the
            // grid can meet a zone with no part surviving the cut.
            let meets = inside
                || to_multipolygon(&whole).is_some_and(|w| {
                    near.iter().filter_map(|&zi| zone_list.get(zi)).any(|z| {
                        let m = z.mask.relate(&w);
                        m.is_intersects() && !m.is_touches()
                    })
                });
            // Rough when a zone got a part of it as cut, or lost one to rounding, even if another
            // zone holds it whole, or when it meets a zone but no part of it survived the cut.
            unresolved += usize::from(!resolved && (cut_part || (meets && !inside)));
            if !sum_fields.is_empty() && !held {
                // A feature meeting no zone lies wholly outside them; one whose parts didn't
                // survive the cut is reported as roughly measured, above.
                let lies_outside = !meets || {
                    let grid = grid.get_or_insert_with(|| OverlayGrid::new(&whole, &near_merc));
                    let rest = if let [zi] = near.as_slice() {
                        zone_list.get(*zi).and_then(|z| part(&whole, &z.mask, false))
                    } else {
                        if let Some(pos) = near_unions.iter().position(|(ids, _)| *ids == near) {
                            if let Some(entry) = near_unions.remove(pos) {
                                near_unions.push_back(entry);
                            }
                        } else {
                            if near_unions.len() == MAX_CACHED_UNIONS {
                                near_unions.pop_front();
                            }
                            let union = union_all(near.iter().filter_map(|&zi| zone_list.get(zi)).flat_map(|z| z.mask.0.iter()));
                            near_unions.push_back((near.clone(), union));
                        }
                        near_unions.back().and_then(|(_, union)| part(&whole, union, false))
                    };
                    rest.is_some_and(|r| grid.wider(&r))
                };
                outside += usize::from(lies_outside);
            }
        }
    }
    let mut out = Dataset::new(
        out_name(p, format!("{}_Tabulate", zones.name)),
        GeometryType::None,
        zone_keys.iter().filter_map(|&k| zones.fields.get(k).cloned()).collect(),
    );
    out.crs_incomplete = zones.crs_incomplete || classes.crs_incomplete;
    // A class field, or an output field, whose name is taken gets a suffix (`name_2`).
    for f in class_keys.iter().filter_map(|&k| classes.fields.get(k)) {
        let name = unique_field(&out, &f.name);
        out.fields.push(Field { name, ..f.clone() });
    }
    for base in
        ["area_km2", "percentage"].into_iter().chain(sum_fields.iter().filter_map(|&f| classes.fields.get(f).map(|f| f.name.as_str())))
    {
        let name = unique_field(&out, base);
        out.fields.push(Field::new(name, FieldType::Double));
    }
    let mut zones_hit: HashSet<usize> = HashSet::new();
    for ((zi, ci), cell) in cells {
        let (Some(zone), Some(class)) = (zone_list.get(zi), class_groups.get(ci)) else { continue };
        zones_hit.insert(zi);
        // Parts of a class's features that overlap each other are unioned, so they count once.
        let union;
        let covered = match cell.parts.as_slice() {
            [one] => one,
            parts => {
                let polys: Vec<MultiPolygon<f64>> = parts.iter().filter_map(to_multipolygon).collect();
                let Some(g) = from_multipolygon(&union_all(polys.iter().flat_map(|m| m.0.iter()))) else {
                    let key: Vec<String> = zone.key.iter().map(ToString::to_string).collect();
                    return Err(format!(
                        "Tabulate Intersection: the parts of a class in the zone '{}' could not be combined",
                        key.join(", ")
                    ));
                };
                union = g;
                &union
            }
        };
        let mut row = zone.key.clone();
        row.extend(class.key.iter().cloned());
        row.push(Value::Float(ground_area_perimeter(covered).0 / 1e6));
        let covered_area = map_area_m2(covered);
        row.push(if zone.map_area_m2 > 0.0 { Value::Float(covered_area / zone.map_area_m2 * 100.0) } else { Value::Null });
        row.extend(cell.sums.into_iter().map(Value::Float));
        // One past the last id: `push` would search them all for each row.
        out.push_next(None, row);
    }
    let mut messages = vec![match out.features.len() {
        1 => "Tabulate Intersection: 1 row".to_string(),
        n => format!("Tabulate Intersection: {n} rows"),
    }];
    for (n, layer) in [(zones_shapeless, "zone"), (classes_shapeless, "class")] {
        match n {
            0 => {}
            1 => messages.push(format!("1 {layer} feature has no polygon and was left out")),
            n => messages.push(format!("{n} {layer} features have no polygon and were left out")),
        }
    }
    // Zones without geometry are counted above, not again here.
    match zone_list.iter().enumerate().filter(|(zi, z)| !z.ext.is_empty() && !zones_hit.contains(zi)).count() {
        0 => {}
        1 => messages.push("1 zone overlaps no class feature, so it has no rows".into()),
        n => messages.push(format!("{n} zones overlap no class feature, so they have no rows")),
    }
    let features = |n: usize, layer: &str| if n == 1 { format!("1 {layer} feature") } else { format!("{n} {layer} features") };
    // One notice for the sum fields' missing values, naming each field that has some.
    let missing: Vec<(usize, &str)> = sum_fields
        .iter()
        .zip(missing)
        .filter(|(_, n)| *n > 0)
        .map(|(&f, n)| (n, classes.fields.get(f).map_or("", |f| f.name.as_str())))
        .collect();
    match missing.as_slice() {
        [] => {}
        [(1, name)] => messages.push(format!("1 class feature has no value in '{name}', left out of its shares")),
        [(n, name)] => messages.push(format!("{n} class features have no value in '{name}', left out of its shares")),
        several => {
            let list: Vec<String> = several.iter().map(|(n, name)| format!("{} in '{name}'", features(*n, "class"))).collect();
            messages.push(format!("class features with no value in a sum field are left out of its shares: {}", list.join(", ")));
        }
    }
    let beyond: Vec<String> = [(zones_beyond, "zone"), (classes_beyond, "class")]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, layer)| features(n, layer))
        .collect();
    if !beyond.is_empty() {
        let verb = if zones_beyond + classes_beyond == 1 { "reaches" } else { "reach" };
        messages.push(format!(
            "{} {verb} beyond {:.2}° latitude, where the map ends: parts there are taken as lying along it, with no area",
            beyond.join(" and "),
            rustgis_geom::MAX_LAT
        ));
    }
    let unsplit: Vec<String> = [(zones_unsplit, "zone"), (classes_unsplit, "class")]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, layer)| features(n, layer))
        .collect();
    if !unsplit.is_empty() {
        let verb = if zones_unsplit + classes_unsplit == 1 { "crosses" } else { "cross" };
        messages.push(format!(
            "{} {verb} the antimeridian without being split there: the map draws them the long way round, and their percentages and shares follow the map, while area_km2 follows the ground",
            unsplit.join(" and ")
        ));
    }
    match unresolved {
        0 => {}
        1 => messages.push(
            "1 class feature is so narrow next to its zones that the overlay's rounding can move its area by more than 1%: its parts are measured only roughly"
                .into(),
        ),
        n => messages.push(format!(
            "{n} class features are so narrow next to their zones that the overlay's rounding can move their areas by more than 1%: their parts are measured only roughly"
        )),
    }
    match outside {
        0 => {}
        1 => messages.push("1 class feature lies partly or wholly outside the zones: its value outside isn't shared out".into()),
        n => messages.push(format!("{n} class features lie partly or wholly outside the zones: their values outside aren't shared out")),
    }
    Ok(ToolResult { output: Output::Table(out), messages })
}

fn export_features(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let ds = input(inp, p, "input")?;
    let mut rs = rows(inp, p, "input", ds);
    let w = s(p, "where").trim();
    if !w.is_empty() {
        let e = rustgis_data::query::parse(w, ds)?;
        rs.retain(|r| rustgis_data::query::eval(&e, ds, *r));
    }
    let mut out = copy_schema(ds, out_name(p, format!("{}_Export", ds.name)), ds.geometry_type);
    for r in rs {
        if let Some(f) = ds.features.get(r) {
            out.push(f.geometry.clone(), f.attrs.clone());
        }
    }
    let n = out.features.len();
    result(out, format!("Export Features: {n} features"))
}

fn extent_contains(a: &Extent, b: &Extent) -> bool {
    !a.is_empty() && !b.is_empty() && a.min_x <= b.min_x && a.max_x >= b.max_x && a.min_y <= b.min_y && a.max_y >= b.max_y
}

/// Whether a tool's parameter applies, given the values of the others (`value` gives one's
/// value as text, empty when unset, which stands for its default). The Tools pane hides and leaves
/// out one that doesn't. Spatial Join's search distance, units and distance field apply only to the
/// match options that measure distances, its merge rules only to one-to-one joins. Summary
/// Statistics' field applies only without a statistics list; its separator, and Dissolve's and
/// Summarize Within's, only to a list with CONCATENATE; Pivot Table's separator only to the
/// CONCATENATE statistic.
pub fn param_applies(tool: &str, param: &str, value: &dyn Fn(&str) -> String) -> bool {
    match (tool, param) {
        ("spatialJoin", "distance" | "units" | "distanceField") => {
            let m = value("match");
            DISTANCE_MATCHES.iter().any(|d| d.eq_ignore_ascii_case(m.trim()))
        }
        ("spatialJoin", "merge") => !value("operation").trim().eq_ignore_ascii_case("Join one to many"),
        ("summaryStatistics", "field") => {
            // Shown until the list holds a pair, so an empty list doesn't hide the field it falls back to.
            let list = serde_json::from_str::<Json>(&value("statistics")).unwrap_or(Json::Null);
            list.as_array().is_none_or(|items| items.is_empty())
        }
        ("pivotTable", "separator") => value("statistic").trim().eq_ignore_ascii_case("CONCATENATE"),
        ("summaryStatistics" | "dissolve" | "countPoints", "separator") => {
            let list = serde_json::from_str::<Json>(&value("statistics")).unwrap_or(Json::Null);
            let concatenates =
                |i: &Json| i.get("statistic").and_then(Json::as_str).is_some_and(|s| s.trim().eq_ignore_ascii_case("CONCATENATE"));
            list.as_array().is_some_and(|items| items.iter().any(concatenates))
        }
        _ => true,
    }
}

/// How Spatial Join matches a target feature to join features.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JoinMatch {
    Intersects,
    Contains,
    Within,
    /// The join features the target's interior point falls in.
    CentreIn,
    /// The one polygon covering most of the target.
    LargestOverlap,
    /// The one nearest join feature, within an optional distance.
    Closest,
    /// Every join feature within a distance, nearest first.
    WithinDistance,
}

impl JoinMatch {
    /// A match option by name, in any case (as `param_applies` reads it); Intersects otherwise.
    fn parse(s: &str) -> Self {
        const NAMED: &[(&str, JoinMatch)] = &[
            ("Contains", JoinMatch::Contains),
            ("Within", JoinMatch::Within),
            ("Have their center in", JoinMatch::CentreIn),
            ("Largest overlap", JoinMatch::LargestOverlap),
            ("Closest", JoinMatch::Closest),
            ("Within a distance", JoinMatch::WithinDistance),
        ];
        NAMED.iter().find(|(n, _)| n.eq_ignore_ascii_case(s.trim())).map_or(JoinMatch::Intersects, |(_, m)| *m)
    }
}

/// The least distance Closest's first search reaches, in metres.
const MIN_REACH_M: f64 = 10.0;

/// The ground distance between two features in metres: zero when they intersect.
fn ground_distance(t: &Geometry, t_geo: &geo::Geometry<f64>, j: &Geometry, j_geo: &geo::Geometry<f64>) -> Option<f64> {
    if t_geo.intersects(j_geo) { Some(0.0) } else { crate::join::distance_m(t, j) }
}

/// How much of a target lies in a join polygon: the ground area of a polygon's overlap, the ground
/// length of a line's. The geometries are in Web Mercator, polygons oriented for the overlay; the
/// overlap found is measured on the ground, in longitude/latitude.
fn overlap(t_geo: &geo::Geometry<f64>, j_geo: &geo::Geometry<f64>) -> f64 {
    match (t_geo, j_geo) {
        (geo::Geometry::MultiPolygon(t), geo::Geometry::MultiPolygon(j)) => {
            from_multipolygon(&t.intersection(j)).map_or(0.0, |g| crate::join::area_m2(&g))
        }
        (geo::Geometry::MultiLineString(t), geo::Geometry::MultiPolygon(j)) => {
            from_line(&j.clip(t, false)).map_or(0.0, |g| crate::join::length_m(&g))
        }
        _ => 0.0,
    }
}

/// A geometry in Web Mercator, polygons oriented as the overlay needs.
fn oriented(g: &Geometry) -> geo::Geometry<f64> {
    match to_geo(g) {
        geo::Geometry::MultiPolygon(m) => geo::Geometry::MultiPolygon(m.orient(Direction::Default)),
        other => other,
    }
}

/// A unique field name in `out`: `base`, else `base_2`, `base_3`, ….
pub(crate) fn unique_field(out: &Dataset, base: &str) -> String {
    let mut name = base.to_string();
    let mut k = 1usize;
    while out.field_index(&name).is_some() {
        k += 1;
        name = format!("{base}_{k}");
    }
    name
}

/// Features' extents on a grid, to find those whose extents may meet another extent without
/// testing every one. Up to 16 features, or when the grid would hold more than a million
/// references (large features on a fine grid), there is no grid and every feature is a candidate.
struct ExtentIndex {
    /// The union of the extents.
    total: Extent,
    cell_w: f64,
    cell_h: f64,
    /// The features each cell meets, in order; `None` without a grid.
    grid: Option<std::collections::HashMap<(i64, i64), Vec<usize>>>,
    /// How many features there are.
    len: usize,
}

impl ExtentIndex {
    /// Small sets deliberately use a bounded-width linear scan instead of a grid.
    const MAX_LINEAR_FEATURES: usize = 16;
    /// The most cell references the grid may hold.
    const MAX_REFERENCES: usize = 1_000_000;

    /// An index of these extents, numbered in order from 0.
    fn new(extents: impl Iterator<Item = Extent>) -> Self {
        let extents: Vec<Extent> = extents.collect();
        let total = extents.iter().fold(Extent::EMPTY, |t, e| t.union(e));
        let n_cells = (extents.len() as f64).sqrt().clamp(4.0, 256.0);
        let mut index = ExtentIndex {
            total,
            cell_w: (total.width() / n_cells).max(1e-9),
            cell_h: (total.height() / n_cells).max(1e-9),
            grid: None,
            len: extents.len(),
        };
        if extents.len() <= Self::MAX_LINEAR_FEATURES || total.is_empty() {
            return index;
        }
        let mut references = 0usize;
        for e in &extents {
            let ((x0, y0), (x1, y1)) = (index.key(e.min_x, e.min_y), index.key(e.max_x, e.max_y));
            references = references.saturating_add(((x1 - x0 + 1) as usize).saturating_mul((y1 - y0 + 1) as usize));
            if references > Self::MAX_REFERENCES {
                return index;
            }
        }
        let mut grid: std::collections::HashMap<(i64, i64), Vec<usize>> = std::collections::HashMap::new();
        for (i, e) in extents.iter().enumerate() {
            let ((x0, y0), (x1, y1)) = (index.key(e.min_x, e.min_y), index.key(e.max_x, e.max_y));
            for x in x0..=x1 {
                for y in y0..=y1 {
                    grid.entry((x, y)).or_default().push(i);
                }
            }
        }
        index.grid = Some(grid);
        index
    }

    /// Conservative retained grid allocation estimate: spare reference capacity plus table
    /// buckets/control bytes, allowing twice the advertised usable hash capacity.
    fn storage_bytes(&self) -> usize {
        self.grid.as_ref().map_or(0, |grid| {
            let bucket = std::mem::size_of::<((i64, i64), Vec<usize>)>().saturating_add(1);
            grid.values().fold(grid.capacity().saturating_mul(2).saturating_mul(bucket), |n, rows| {
                n.saturating_add(rows.capacity().saturating_mul(std::mem::size_of::<usize>()))
            })
        })
    }

    /// The cell holding a position, clamped to the grid (a non-finite one goes in the first).
    fn key(&self, x: f64, y: f64) -> (i64, i64) {
        let kx = if x.is_finite() { ((x - self.total.min_x) / self.cell_w).floor() } else { 0.0 };
        let ky = if y.is_finite() { ((y - self.total.min_y) / self.cell_h).floor() } else { 0.0 };
        (kx.clamp(0.0, 512.0) as i64, ky.clamp(0.0, 512.0) as i64)
    }

    /// The larger side of a cell in degrees, when there is a grid.
    fn cell_size(&self) -> Option<f64> {
        self.grid.as_ref().map(|_| self.cell_w.max(self.cell_h))
    }

    /// The cell bounds of the query's overlap with the grid, excluding empty or disjoint queries.
    fn query_cells(&self, ext: &Extent) -> Option<((i64, i64), (i64, i64))> {
        if !ext.intersects(&self.total) {
            return None;
        }
        // Clamp to the grid's own bounds, even for a query far larger than the layer.
        let t = &self.total;
        Some((self.key(ext.min_x.max(t.min_x), ext.min_y.max(t.min_y)), self.key(ext.max_x.min(t.max_x), ext.max_y.min(t.max_y))))
    }

    /// Whether any candidate passes `test`, stopping immediately at the first match. Test each
    /// candidate at most once even when it spans several cells; candidate order is unspecified.
    fn any_candidate(&self, ext: &Extent, mut test: impl FnMut(usize) -> bool) -> bool {
        let Some(grid) = &self.grid else { return (0..self.len).any(test) };
        let Some(((x0, y0), (x1, y1))) = self.query_cells(ext) else { return false };
        let mut seen = std::collections::HashSet::new();
        for x in x0..=x1 {
            for y in y0..=y1 {
                if let Some(indices) = grid.get(&(x, y)) {
                    for &i in indices {
                        if seen.insert(i) && test(i) {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    /// Replaces the contents of `out` with the candidate features meeting `ext`, in order and once each.
    /// Without a grid that is every feature, whether or not it meets `ext`.
    fn candidates_into(&self, ext: &Extent, out: &mut Vec<usize>) {
        out.clear();
        let Some(grid) = &self.grid else {
            out.extend(0..self.len);
            return;
        };
        let Some(((x0, y0), (x1, y1))) = self.query_cells(ext) else { return };
        for x in x0..=x1 {
            for y in y0..=y1 {
                if let Some(indices) = grid.get(&(x, y)) {
                    out.extend_from_slice(indices);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
    }

    /// The features whose extents may meet `ext`, in order and once each: every feature without a
    /// grid. Each still needs its own test.
    fn candidates(&self, ext: &Extent) -> Vec<usize> {
        let mut out = Vec::new();
        self.candidates_into(ext, &mut out);
        out
    }
}

/// A feature to join or measure to: its row in its layer, its extent, and its geometry in Web
/// Mercator (`geo`) and in lon/lat (`geom`).
struct Located<'a> {
    row: usize,
    ext: Extent,
    geo: geo::Geometry<f64>,
    geom: &'a Geometry,
}

/// Features to measure ground distances to (Spatial Join's join features, Near's near features),
/// looked up through an index of their extents. Features are named by their place in `feats`.
struct Nearby<'a, 'b> {
    feats: &'b [Located<'a>],
    index: &'b ExtentIndex,
}

impl Nearby<'_, '_> {
    /// The features within `reach` metres of a target (`t`, with its extent, and its geometry in Web
    /// Mercator), in order, with their distances. With `nearest`, only the nearest is wanted: a
    /// feature that can't be as near as the nearest so far isn't measured, so some farther ones
    /// may be missing. The feature in row `exclude` is left out (the target itself, when both are
    /// one layer). `measured` keeps each feature's distance, measured once per target.
    fn within(
        &self,
        t: &Geometry,
        t_ext: &Extent,
        t_geo: &geo::Geometry<f64>,
        reach: f64,
        nearest: bool,
        exclude: Option<usize>,
        measured: &mut std::collections::HashMap<usize, Option<f64>>,
    ) -> Vec<(usize, f64)> {
        let looked = match crate::join::search_box(t_ext, reach) {
            Some(b) => self.index.candidates(&b),
            None => (0..self.feats.len()).collect(),
        };
        let mut near = Vec::new();
        let mut bound = reach;
        for idx in looked {
            let Some(j) = self.feats.get(idx).filter(|j| Some(j.row) != exclude) else { continue };
            if !crate::join::may_be_within(t_ext, &j.ext, bound) {
                continue;
            }
            let d = *measured.entry(idx).or_insert_with(|| ground_distance(t, t_geo, j.geom, &j.geo));
            if let Some(d) = d.filter(|d| *d <= reach * (1.0 + 1e-9) + 1e-6) {
                near.push((idx, d));
                if nearest {
                    bound = bound.min(d);
                }
            }
        }
        near
    }

    /// Every feature within `reach` metres of a target, nearest first; equal distances keep the
    /// features' order.
    fn all_within(&self, t: &Geometry, t_ext: &Extent, t_geo: &geo::Geometry<f64>, reach: f64) -> Vec<(usize, f64)> {
        let mut near = self.within(t, t_ext, t_geo, reach, false, None, &mut std::collections::HashMap::new());
        near.sort_by(|a, b| a.1.total_cmp(&b.1));
        near
    }

    /// The feature nearest a target, within `limit` metres when there is one, and its distance;
    /// of equally near features, the first. The feature in row `exclude` is left out.
    ///
    /// Searches widen from about a grid cell, doubling until one finds a feature within its
    /// distance (no nearer one can lie outside it), up to the limit. Without an index, one search
    /// of everything. (A grid of coincident features has cells of no size: the first search reaches
    /// at least `MIN_REACH_M`.)
    fn closest(
        &self,
        t: &Geometry,
        t_ext: &Extent,
        t_geo: &geo::Geometry<f64>,
        limit: Option<f64>,
        exclude: Option<usize>,
    ) -> Option<(usize, f64)> {
        let mut measured = std::collections::HashMap::new();
        let mut reach = match self.index.cell_size() {
            Some(cell) => (cell.to_radians() * rustgis_geom::EARTH_RADIUS_M).max(MIN_REACH_M),
            None => f64::INFINITY,
        };
        let mut near = loop {
            let r = limit.map_or(reach, |l| reach.min(l));
            // A search reaching a pole or across the antimeridian looks at every feature anyway: it
            // measures them out to the limit.
            if crate::join::search_box(t_ext, r).is_none() {
                break self.within(t, t_ext, t_geo, limit.unwrap_or(f64::INFINITY), true, exclude, &mut measured);
            }
            let found = self.within(t, t_ext, t_geo, r, true, exclude, &mut measured);
            if !found.is_empty() || limit.is_some_and(|l| r >= l) {
                break found;
            }
            reach *= 2.0;
        };
        near.sort_by(|a, b| a.1.total_cmp(&b.1));
        near.into_iter().next()
    }
}

/// A point (lon/lat) two intersecting features share: one of the points of either on the other;
/// else a point of their overlap, when one is polygons; else a point where their edges meet. The
/// geometries come with their Web Mercator forms, where the test and the overlay work.
fn shared_point(a: &Geometry, a_geo: &geo::Geometry<f64>, b: &Geometry, b_geo: &geo::Geometry<f64>) -> Option<Coord> {
    use geo::LinesIter;
    let back = |c: geo::Coord<f64>| rustgis_geom::merc_to_lonlat(Coord::new(c.x, c.y));
    // Points answer with their own coordinates, not ones back from Web Mercator.
    let point_on = |pts: &Geometry, other: &geo::Geometry<f64>| -> Option<Option<Coord>> {
        let pts: &[Coord] = match pts {
            Geometry::Point(c) => std::slice::from_ref(c),
            Geometry::MultiPoint(v) => v,
            _ => return None,
        };
        Some(pts.iter().copied().find(|c| {
            let m = rustgis_geom::lonlat_to_merc(*c);
            other.intersects(&geo::Point::new(m.x, m.y))
        }))
    };
    if let Some(found) = point_on(a, b_geo).or_else(|| point_on(b, a_geo)) {
        return found;
    }
    let overlap = match (a_geo, b_geo) {
        (_, geo::Geometry::MultiPolygon(m)) => part(&dissolved(a), &mask_of(m.clone()), true),
        (geo::Geometry::MultiPolygon(m), _) => part(&dissolved(b), &mask_of(m.clone()), true),
        _ => None,
    };
    if let Some(c) = overlap.as_ref().and_then(crate::join::first_coord) {
        return Some(c);
    }
    // Lines, or polygons that only touch: the first point where an edge of `a` meets one of `b`. `b`'s
    // edges are sorted by their west end, so the search for each of `a`'s stops at the first that
    // starts east of it, and a pair whose boxes don't meet is skipped before the exact test; pairs
    // within that window are still each tried, so two long touching borders cost about the product
    // of their edges. This runs once per output row, for the nearest feature only.
    let edges = |g: &geo::Geometry<f64>| -> Vec<geo::Line<f64>> {
        let mut v: Vec<geo::Line<f64>> = match g {
            geo::Geometry::MultiLineString(m) => m.lines_iter().collect(),
            geo::Geometry::MultiPolygon(m) => m.lines_iter().collect(),
            _ => Vec::new(),
        };
        v.sort_by(|x, y| x.start.x.min(x.end.x).total_cmp(&y.start.x.min(y.end.x)));
        v
    };
    let (ea, eb) = (edges(a_geo), edges(b_geo));
    for la in &ea {
        let (x0, x1) = (la.start.x.min(la.end.x), la.start.x.max(la.end.x));
        let (y0, y1) = (la.start.y.min(la.end.y), la.start.y.max(la.end.y));
        for lb in &eb {
            if lb.start.x.min(lb.end.x) > x1 {
                break;
            }
            if lb.start.x.max(lb.end.x) < x0 || lb.start.y.max(lb.end.y) < y0 || lb.start.y.min(lb.end.y) > y1 {
                continue;
            }
            match geo::line_intersection::line_intersection(*la, *lb) {
                Some(geo::LineIntersection::SinglePoint { intersection, .. }) => return Some(back(intersection)),
                Some(geo::LineIntersection::Collinear { intersection }) => return Some(back(intersection.start)),
                None => {}
            }
        }
    }
    None
}

/// Near: a copy of the input features with the nearest near feature's OID (`near_fid`), the ground
/// distance to it (`near_dist`, in `units`) and the point on it nearest the input feature
/// (`near_x`, `near_y`, lon/lat; a point they share when they intersect). Within a search distance
/// when one is given; features with nothing that near, or without geometry, get empty values.
/// With one layer as both, a feature's nearest is another feature.
fn near(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    near_on(inp, p, None)
}

/// [`near`] on `threads` threads (none: as many as pay off).
fn near_on(inp: &dyn Inputs, p: &Json, threads: Option<usize>) -> Result<ToolResult, String> {
    let ds = input(inp, p, "input")?;
    let near_ds = input(inp, p, "near")?;
    if ds.geometry_type == GeometryType::None || near_ds.geometry_type == GeometryType::None {
        return Err(format!("'{}' has no geometry", if ds.geometry_type == GeometryType::None { &ds.name } else { &near_ds.name }));
    }
    let given = p.get("distance").filter(|v| !v.is_null() && v.as_str().is_none_or(|t| !t.trim().is_empty()));
    let limit = match given {
        None => None,
        Some(_) => {
            let d = num(p, "distance").ok_or("Search Distance must be a number")?;
            if d < 0.0 {
                return Err("Search Distance can't be negative".into());
            }
            Some(to_metres(d, s(p, "units"))?)
        }
    };
    let unit_m = to_metres(1.0, s(p, "units"))?;
    // The same layer as both: leave each feature out of its own search.
    let same = std::ptr::eq(ds, near_ds);

    // In layer order, whatever order the selection was made in: of equally near features, the
    // first in the layer wins.
    let mut near_rows = rows(inp, p, "near", near_ds);
    near_rows.sort_unstable();
    let feats: Vec<Located<'_>> = near_rows
        .into_iter()
        .filter_map(|row| {
            let g = near_ds.features.get(row)?.geometry.as_ref()?;
            Some(Located { row, ext: g.extent(), geo: to_geo(g), geom: g })
        })
        .collect();
    let index = ExtentIndex::new(feats.iter().map(|f| f.ext));
    let nearby = Nearby { feats: &feats, index: &index };

    let mut out = copy_schema(ds, out_name(p, format!("{}_Near", ds.name)), ds.geometry_type);
    out.crs = ds.crs;
    for (name, ty) in
        [("near_fid", FieldType::Integer), ("near_dist", FieldType::Double), ("near_x", FieldType::Double), ("near_y", FieldType::Double)]
    {
        let name = unique_field(&out, name);
        out.fields.push(Field::new(name, ty));
    }
    let (mut found, mut total) = (0usize, 0usize);
    // Each feature's nearest one is found on its own, on several threads when there are many
    // (#1252); the rows are then made in input order.
    let targets = rows(inp, p, "input", ds);
    crate::control::begin(targets.len());
    let threads = threads.unwrap_or_else(|| crate::parallel::threads_for(targets.len(), PARALLEL_FEATURES));
    let hits = crate::parallel::try_map_on(
        &targets,
        threads,
        || (),
        |_, _, &r| {
            let hit = ds.features.get(r).and_then(|f| f.geometry.as_ref()).and_then(|g| {
                let (t_ext, t_geo) = (g.extent(), to_geo(g));
                let (idx, d) = nearby.closest(g, &t_ext, &t_geo, limit, same.then_some(r))?;
                let j = feats.get(idx)?;
                // Where they touch, a point they share; else, or when none is found in the map's plane (the
                // same place at −180° and 180°), the nearest points' pair.
                let shared = if d > 0.0 { None } else { shared_point(g, &t_geo, j.geom, &j.geo) };
                let at = shared.or_else(|| crate::join::nearest_points(g, j.geom).map(|(_, q)| q));
                // The plane's longitudes run on past ±180°, and so can a shared point's: back within
                // [−180°, 180°).
                let at = at.map(|c| Coord::new((c.x + 180.0).rem_euclid(360.0) - 180.0, c.y));
                Some((near_ds.features.get(j.row)?.id, d, at))
            });
            Ok::<_, String>(hit)
        },
    )?;
    for (&r, hit) in targets.iter().zip(hits) {
        let Some(f) = ds.features.get(r) else { continue };
        total += 1;
        let mut attrs: Vec<Value> = (0..ds.fields.len()).map(|i| f.attrs.get(i).cloned().unwrap_or(Value::Null)).collect();
        match hit {
            Some((fid, d, at)) => {
                found += 1;
                let xy = |v: Option<f64>| v.map_or(Value::Null, Value::Float);
                attrs.extend([
                    i64::try_from(fid).map_or(Value::Null, Value::Int),
                    Value::Float(d / unit_m),
                    xy(at.map(|c| c.x)),
                    xy(at.map(|c| c.y)),
                ]);
            }
            None => attrs.extend([Value::Null, Value::Null, Value::Null, Value::Null]),
        }
        out.push_next(f.geometry.clone(), attrs);
    }
    result(out, format!("Near: {found} of {total} features have a near feature"))
}

/// Joins normalized lon/lat geometries, retaining the target's source CRS for export.
fn spatial_join(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let target = input(inp, p, "target")?;
    let join = input(inp, p, "join")?;

    let mode = JoinMatch::parse(s(p, "match"));
    let by_distance = matches!(mode, JoinMatch::Closest | JoinMatch::WithinDistance);

    let op_many = s(p, "operation").eq_ignore_ascii_case("Join one to many");
    let keep_all = !s(p, "keepAll").eq_ignore_ascii_case("Keep only matching");

    let prefix_raw = s(p, "prefix");
    let prefix = if prefix_raw.is_empty() && p.get("prefix").is_none() { "join_" } else { prefix_raw };

    // The search distance, in metres, and how a distance is written in the chosen units.
    let given = p.get("distance").filter(|v| !v.is_null() && v.as_str().is_none_or(|t| !t.trim().is_empty()));
    let limit = match given {
        None => None,
        Some(_) if !by_distance => return Err("Search Distance applies only to Closest and Within a distance".into()),
        Some(_) => {
            let d = num(p, "distance").ok_or("Search Distance must be a number")?;
            if d < 0.0 {
                return Err("Search Distance can't be negative".into());
            }
            Some(to_metres(d, s(p, "units"))?)
        }
    };
    if mode == JoinMatch::WithinDistance && limit.is_none() {
        return Err("Within a distance needs a Search Distance".into());
    }
    let unit_m = if by_distance { to_metres(1.0, s(p, "units"))? } else { 1.0 };
    let distance_field = s(p, "distanceField").trim();
    if !distance_field.is_empty() && !by_distance {
        return Err("Distance Field applies only to Closest and Within a distance".into());
    }

    let merges = crate::join::parse_merge(join, p.get("merge"))?;
    if op_many && !merges.is_empty() {
        return Err("Merge rules apply to Join one to one (one to many keeps each match)".into());
    }
    // For each merge rule, the targets whose sum went past the 64-bit range, the matched numbers it
    // left out, and the targets whose result came out rounded from a whole number past 2^53.
    let mut overflowed = vec![0usize; merges.len()];
    let mut left_out = vec![0usize; merges.len()];
    let mut rounded = Rounded::new(merges.len());

    if mode == JoinMatch::LargestOverlap
        && (join.geometry_type != GeometryType::Polygon || !matches!(target.geometry_type, GeometryType::Polygon | GeometryType::Polyline))
    {
        return Err("Largest overlap joins polygons to polygon or line target features".into());
    }

    let target_rows = rows(inp, p, "target", target);
    let join_rows = rows(inp, p, "join", join);

    let mut join_geoms: Vec<Located<'_>> = Vec::with_capacity(join_rows.len());
    for &jr in &join_rows {
        if let Some(f) = join.features.get(jr)
            && let Some(g) = f.geometry.as_ref()
        {
            let geo = if mode == JoinMatch::LargestOverlap { oriented(g) } else { to_geo(g) };
            join_geoms.push(Located { row: jr, ext: g.extent(), geo, geom: g });
        }
    }

    let mut out = copy_schema(target, out_name(p, format!("{}_SpatialJoin", target.name)), target.geometry_type);
    out.source = None;
    out.crs = target.crs;

    let count_field_name = unique_field(&out, "join_count");
    out.fields.push(Field::new(count_field_name, FieldType::Integer));

    let mut join_field_indices = Vec::with_capacity(join.fields.len());
    for (orig_idx, jf) in join.fields.iter().enumerate() {
        let raw = if prefix.is_empty() { jf.name.clone() } else { format!("{prefix}{}", jf.name) };
        let name = unique_field(&out, &raw);
        let ty = merges.iter().find(|m| m.field == orig_idx).map_or(jf.ty, |m| m.field_type(jf.ty));
        out.fields.push(Field::new(name, ty));
        join_field_indices.push(orig_idx);
    }
    if !distance_field.is_empty() {
        let name = unique_field(&out, distance_field);
        out.fields.push(Field::new(name, FieldType::Double));
    }

    let target_fields_len = target.fields.len();
    let join_fields_len = join.fields.len();

    let index = ExtentIndex::new(join_geoms.iter().map(|j| j.ext));
    let nearby = Nearby { feats: &join_geoms, index: &index };

    let mut total_matches = 0usize;
    let mut written_features = 0usize;
    let mut cand_buf = Vec::new();

    for tr in target_rows {
        let Some(tf) = target.features.get(tr) else { continue };
        // The matches, in the order they're joined, with their distances when measured.
        let mut matches: Vec<(usize, Option<f64>)> = Vec::new();

        if let Some(tg) = tf.geometry.as_ref() {
            let t_ext = tg.extent();
            let t_geo = if mode == JoinMatch::LargestOverlap { oriented(tg) } else { to_geo(tg) };
            let at = |idx: usize| join_geoms.get(idx);

            match mode {
                JoinMatch::Intersects | JoinMatch::Contains | JoinMatch::Within => {
                    index.candidates_into(&t_ext, &mut cand_buf);
                    for j in cand_buf.iter().copied().filter_map(at) {
                        let hit = match mode {
                            JoinMatch::Contains => extent_contains(&t_ext, &j.ext) && Contains::contains(&t_geo, &j.geo),
                            JoinMatch::Within => extent_contains(&j.ext, &t_ext) && Contains::contains(&j.geo, &t_geo),
                            _ => t_ext.intersects(&j.ext) && t_geo.intersects(&j.geo),
                        };
                        if hit {
                            matches.push((j.row, None));
                        }
                    }
                }
                JoinMatch::CentreIn => {
                    if let Some(pt) = interior_point(tg) {
                        let (p_ext, p_geo) = (pt.extent(), to_geo(&pt));
                        index.candidates_into(&p_ext, &mut cand_buf);
                        for j in cand_buf.iter().copied().filter_map(at) {
                            if j.ext.intersects(&p_ext) && j.geo.intersects(&p_geo) {
                                matches.push((j.row, None));
                            }
                        }
                    }
                }
                JoinMatch::LargestOverlap => {
                    let mut best: Option<(usize, f64)> = None;
                    index.candidates_into(&t_ext, &mut cand_buf);
                    for j in cand_buf.iter().copied().filter_map(at) {
                        if !t_ext.intersects(&j.ext) {
                            continue;
                        }
                        let o = overlap(&t_geo, &j.geo);
                        if o > 0.0 && best.is_none_or(|(_, b)| o > b) {
                            best = Some((j.row, o));
                        }
                    }
                    matches.extend(best.map(|(row, _)| (row, None)));
                }
                JoinMatch::Closest | JoinMatch::WithinDistance => {
                    let near = match (mode, limit) {
                        (JoinMatch::WithinDistance, Some(l)) => nearby.all_within(tg, &t_ext, &t_geo, l),
                        (JoinMatch::WithinDistance, None) => Vec::new(),
                        _ => nearby.closest(tg, &t_ext, &t_geo, limit, None).into_iter().collect(),
                    };
                    matches.extend(near.into_iter().filter_map(|(idx, d)| Some((at(idx)?.row, Some(d)))));
                }
            }
        }

        let m_count = matches.len();
        total_matches += m_count;

        let mut push_row = |count: i64, joined: Option<Vec<Value>>, metres: Option<f64>| {
            let mut attrs = Vec::with_capacity(out.fields.len());
            for i in 0..target_fields_len {
                attrs.push(tf.attrs.get(i).cloned().unwrap_or(Value::Null));
            }
            attrs.push(Value::Int(count));
            match joined {
                Some(values) => attrs.extend(values),
                None => attrs.resize(attrs.len() + join_fields_len, Value::Null),
            }
            if !distance_field.is_empty() {
                attrs.push(metres.map_or(Value::Null, |m| Value::Float(m / unit_m)));
            }
            out.push_next(tf.geometry.clone(), attrs);
            written_features += 1;
        };
        let values_of = |jr: usize| -> Vec<Value> {
            let f = join.features.get(jr);
            join_field_indices.iter().map(|&ji| f.and_then(|f| f.attrs.get(ji)).cloned().unwrap_or(Value::Null)).collect()
        };

        if m_count == 0 {
            if keep_all {
                // A count of no matches is 0; every other value is null.
                let counted = |ji: &usize| merges.iter().any(|m| m.field == *ji && m.rule == crate::join::Rule::Count);
                let none = merges
                    .iter()
                    .any(|m| m.rule == crate::join::Rule::Count)
                    .then(|| join_field_indices.iter().map(|ji| if counted(ji) { Value::Int(0) } else { Value::Null }).collect());
                push_row(0, none, None);
            }
        } else if op_many {
            for &(jr, d) in &matches {
                push_row(m_count as i64, Some(values_of(jr)), d);
            }
        } else {
            let Some(&(first, d)) = matches.first() else { continue };
            let mut values = values_of(first);
            for (k, m) in merges.iter().enumerate() {
                let column: Vec<&Value> = matches
                    .iter()
                    .map(|(jr, _)| join.features.get(*jr).and_then(|f| f.attrs.get(m.field)).unwrap_or(&Value::Null))
                    .collect();
                let ty = join.fields.get(m.field).map_or(FieldType::Text, |f| f.ty);
                let (merged, lost) = m.apply(ty, &column);
                if let Some(slot) = join_field_indices.iter().position(|&ji| ji == m.field).and_then(|k| values.get_mut(k)) {
                    *slot = merged;
                }
                if let Some(n) = overflowed.get_mut(k) {
                    *n += usize::from(lost.overflowed);
                }
                if let Some(n) = left_out.get_mut(k) {
                    *n += lost.left_out;
                }
                if let Some(n) = rounded.results.get_mut(k) {
                    *n += usize::from(lost.rounded);
                }
            }
            push_row(m_count as i64, Some(values), d);
        }
    }

    // The output field of each merge rule, for the notices.
    let merged_fields: Vec<Field> = merges
        .iter()
        .map(|m| {
            let at = join_field_indices.iter().position(|&ji| ji == m.field).map(|k| target_fields_len + 1 + k);
            at.and_then(|i| out.fields.get(i)).cloned().unwrap_or_else(|| Field::new("", FieldType::Text))
        })
        .collect();
    let mut messages = vec![format!("Spatial Join: {written_features} features joined ({total_matches} matches)")];
    // The total of `counts` and the output fields with some, or None without any.
    let counted = |counts: &[usize]| -> Option<(usize, String)> {
        let total: usize = counts.iter().sum();
        let names: Vec<&str> = counts.iter().zip(&merged_fields).filter(|(n, _)| **n > 0).map(|(_, f)| f.name.as_str()).collect();
        (total > 0).then(|| (total, names.join(", ")))
    };
    if let Some((total, names)) = counted(&overflowed) {
        let (sums, are, were) = if total == 1 { ("sum", "is", "was") } else { ("sums", "are", "were") };
        messages.push(format!("{total} {sums} in {names} {are} past the 64-bit integer range and {were} left empty"));
    }
    if let Some((total, names)) = counted(&left_out) {
        let (numbers, were) = if total == 1 { ("number", "was") } else { ("numbers", "were") };
        messages.push(format!(
            "{total} matched {numbers} in {names} {were} left out: a whole-number sum takes whole numbers in the 64-bit range, the other rules numbers a decimal holds"
        ));
    }
    messages.extend(rounded_notice(&rounded, &merged_fields));
    Ok(ToolResult { output: Output::Layer(out), messages })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatOp {
    Mean,
    Min,
    Max,
    Sum,
    Count,
    StdDev,
}

impl StatOp {
    pub const ALL: [StatOp; 6] = [StatOp::Mean, StatOp::Min, StatOp::Max, StatOp::Sum, StatOp::Count, StatOp::StdDev];

    fn name(self) -> &'static str {
        match self {
            StatOp::Mean => "mean",
            StatOp::Min => "min",
            StatOp::Max => "max",
            StatOp::Sum => "sum",
            StatOp::Count => "count",
            StatOp::StdDev => "stddev",
        }
    }

    fn field_type(self) -> FieldType {
        match self {
            StatOp::Count => FieldType::Integer,
            _ => FieldType::Double,
        }
    }

    fn parse(raw: &str) -> Result<Vec<StatOp>, String> {
        let clean = raw.trim().to_ascii_lowercase();
        match clean.as_str() {
            "all" => Ok(Self::ALL.to_vec()),
            "mean" => Ok(vec![StatOp::Mean]),
            "min" => Ok(vec![StatOp::Min]),
            "max" => Ok(vec![StatOp::Max]),
            "sum" => Ok(vec![StatOp::Sum]),
            "count" => Ok(vec![StatOp::Count]),
            "stddev" | "std" | "std_dev" => Ok(vec![StatOp::StdDev]),
            "" => Err("empty statistic name: choose from mean, min, max, sum, count, stddev, or all".into()),
            other => Err(format!("unknown statistic '{other}': choose from mean, min, max, sum, count, stddev, or all")),
        }
    }
}

/// Parses the `bands` parameter for zonal statistics.
/// Null and an empty string default to band 1, matching the workflow form.
pub fn zonal_bands(value: &Json, total_bands: usize) -> Result<Vec<usize>, String> {
    if total_bands == 0 {
        return Err("raster has no bands".into());
    }
    match value {
        Json::Null => Ok(vec![0]),
        Json::String(s) => crate::rasterops::parse_bands_str(s, total_bands),
        Json::Number(n) => {
            Ok(vec![crate::rasterops::band_from_f64(n.as_f64().ok_or_else(|| "invalid band number".to_string())?, total_bands)?])
        }
        Json::Array(arr) => {
            if arr.is_empty() {
                return Err("bands array cannot be empty".into());
            }
            let mut out = Vec::new();
            for item in arr {
                if !matches!(item, Json::Number(_) | Json::String(_)) {
                    return Err("band items must be numbers or strings".into());
                }
                if item.as_str().is_some_and(|s| s.trim().is_empty()) {
                    return Err("band items cannot be empty strings".into());
                }
                for band in zonal_bands(item, total_bands)? {
                    if !out.contains(&band) {
                        out.push(band);
                    }
                }
            }
            Ok(out)
        }
        _ => Err("bands must be a number, string, or array of numbers/strings".into()),
    }
}

fn parse_bands(p: &Json, total_bands: usize) -> Result<Vec<usize>, String> {
    zonal_bands(p.get("bands").unwrap_or(&Json::Null), total_bands)
}

fn parse_stats(p: &Json) -> Result<Vec<StatOp>, String> {
    match p.get("stats") {
        None | Some(Json::Null) => Ok(StatOp::ALL.to_vec()),
        Some(Json::Array(arr)) => {
            if arr.is_empty() {
                return Err("stats array cannot be empty".into());
            }
            let mut out = Vec::new();
            for item in arr {
                let txt = item.as_str().ok_or_else(|| "statistic names must be strings".to_string())?;
                if txt.trim().is_empty() {
                    return Err("statistic name cannot be empty".into());
                }
                let ops = StatOp::parse(txt)?;
                for op in ops {
                    if !out.contains(&op) {
                        out.push(op);
                    }
                }
            }
            Ok(out)
        }
        Some(Json::String(txt)) => {
            let trimmed = txt.trim();
            if trimmed.is_empty() {
                return Err("stats parameter cannot be empty string".into());
            }
            let clean = trimmed.trim_matches(|c| c == '[' || c == ']');
            if clean.trim().is_empty() {
                return Err("stats parameter cannot be empty brackets".into());
            }
            let mut out = Vec::new();
            for part in clean.split([',', ';']) {
                let part = part.trim().trim_matches('"').trim();
                if part.is_empty() {
                    return Err("stats selection contains an empty statistic entry".into());
                }
                let ops = StatOp::parse(part)?;
                for op in ops {
                    if !out.contains(&op) {
                        out.push(op);
                    }
                }
            }
            Ok(out)
        }
        _ => Err("stats parameter must be a string or array of strings".into()),
    }
}

fn zonal_stats(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    zonal_stats_on(inp, p, None)
}

/// [`zonal_stats`] on `threads` threads (none: as many as pay off and the masks' memory allows).
fn zonal_stats_on(inp: &dyn Inputs, p: &Json, threads: Option<usize>) -> Result<ToolResult, String> {
    let r_name = {
        let n = s(p, "raster");
        if n.is_empty() { s(p, "layer") } else { n }
    };
    if r_name.is_empty() {
        return Err("parameter 'raster' is required".into());
    }
    let r_input = inp.raster(r_name).ok_or_else(|| format!("no raster layer named '{r_name}'"))?;
    let r = r_input.raster;
    r.require_coordinates()?;

    let poly_param = if !s(p, "polygons").is_empty() { "polygons" } else { "input" };
    let poly_ds = input(inp, p, poly_param)?;
    if poly_ds.geometry_type != GeometryType::Polygon {
        return Err(format!(
            "layer '{}' is {} data; zonal statistics needs polygon geometries",
            poly_ds.name,
            poly_ds.geometry_type.label()
        ));
    }

    let bands = parse_bands(p, r.band_count())?;
    let stats = parse_stats(p)?;

    let mut out = copy_schema(poly_ds, out_name(p, format!("{}_ZonalStats", poly_ds.name)), poly_ds.geometry_type);
    out.source = None;
    out.crs = poly_ds.crs;

    let user_prefix = s(p, "prefix").trim();
    let multi_bands = bands.len() > 1;
    let mut out_fields = Vec::new();
    let mut new_field_names: Vec<String> = Vec::new();

    for (b_idx, &band) in bands.iter().enumerate() {
        let band_num = band + 1;
        let band_name = r.bands.get(band).map(|b| b.name.as_str()).unwrap_or("");
        let has_named_band = !band_name.is_empty() && !band_name.starts_with("Band ") && band_name != "Gray";
        let prefix = if !user_prefix.is_empty() {
            if multi_bands { format!("{user_prefix}_b{band_num}") } else { user_prefix.to_string() }
        } else if multi_bands {
            if has_named_band { format!("{}_{}", r.name, band_name) } else { format!("{}_b{band_num}", r.name) }
        } else if has_named_band {
            band_name.to_string()
        } else {
            r.name.to_string()
        };

        for &stat in &stats {
            let base_name = format!("{prefix}_{}", stat.name());
            let mut final_name = base_name.clone();
            let mut counter = 1usize;
            while out.field_index(&final_name).is_some() || new_field_names.iter().any(|n| n.eq_ignore_ascii_case(&final_name)) {
                counter += 1;
                final_name = format!("{base_name}_{counter}");
            }
            new_field_names.push(final_name.clone());
            out_fields.push((b_idx, band, stat, final_name, stat.field_type()));
        }
    }

    for (_, _, _, name, ty) in &out_fields {
        out.fields.push(Field::new(name.clone(), *ty));
    }

    let target_rows: Vec<usize> = rows(inp, p, poly_param, poly_ds).into_iter().filter(|&r| r < poly_ds.features.len()).collect();
    // Each feature's statistics are its own, its cells summed in the same order on any thread, so
    // the features are worked out on several threads when there are many (#1252). A thread holds
    // a feature's mask, up to the raster's size, and briefly its trimmed copy too, so their memory
    // bounds the threads too.
    let mask_bytes = (r.width as usize).saturating_mul(r.height as usize).saturating_mul(2);
    let threads = threads.unwrap_or_else(|| crate::parallel::threads_for_scratch(target_rows.len(), PARALLEL_FEATURES, mask_bytes));
    crate::control::begin(target_rows.len());
    // A feature's statistics by band, whether its geometry was read, its parts that couldn't be
    // projected and what went wrong.
    let worked = crate::parallel::try_map_on(
        &target_rows,
        threads,
        || (),
        |_, _, &r_idx| {
            let none = || vec![crate::rasterops::ZoneStats::default(); bands.len()];
            let Some(f) = poly_ds.features.get(r_idx) else { return Ok((none(), false, 0, None)) };
            Ok::<_, String>(match &f.geometry {
                Some(Geometry::Polygon(polys)) if !polys.is_empty() => {
                    // Dataset geometry is already lon/lat; crs records the source system.
                    match crate::rasterops::transform_feature_polygons_to_lonlat(polys, None) {
                        Ok(polys_lonlat) => {
                            let mut failed_parts = Vec::new();
                            let st = crate::rasterops::zonal_statistics_with_failures(r, &bands, &polys_lonlat, &mut failed_parts)?;
                            let detail = (!failed_parts.is_empty())
                                .then(|| format!("feature {}: raster projection failed for polygon part(s) {failed_parts:?}", f.id));
                            (st, true, failed_parts.len(), detail)
                        }
                        Err(e) => (none(), false, 0, Some(format!("feature {}: {e}", f.id))),
                    }
                }
                _ => (none(), false, 0, Some(format!("feature {}: missing or empty geometry", f.id))),
            })
        },
    )?;
    let mut skipped_features = 0usize;
    let mut skipped_parts = 0usize;
    let mut failure_details = Vec::new();
    for (&r_idx, (stats_by_band, feature_valid, failed_parts, detail)) in target_rows.iter().zip(worked) {
        let Some(f) = poly_ds.features.get(r_idx) else { continue };
        skipped_parts += failed_parts;
        failure_details.extend(detail);
        if !feature_valid {
            skipped_features += 1;
        }

        let mut attrs = f.attrs.clone();
        attrs.resize(poly_ds.fields.len(), Value::Null);
        for (b_idx, _, stat, _, _) in &out_fields {
            let b_stats = stats_by_band.get(*b_idx);
            let val = match (b_stats, stat) {
                (Some(s), StatOp::Count) => Value::Int(s.count as i64),
                (Some(s), StatOp::Mean) => s.mean.map(Value::Float).unwrap_or(Value::Null),
                (Some(s), StatOp::Min) => s.min.map(Value::Float).unwrap_or(Value::Null),
                (Some(s), StatOp::Max) => s.max.map(Value::Float).unwrap_or(Value::Null),
                (Some(s), StatOp::Sum) => s.sum.map(Value::Float).unwrap_or(Value::Null),
                (Some(s), StatOp::StdDev) => s.stddev.map(Value::Float).unwrap_or(Value::Null),
                _ => Value::Null,
            };
            attrs.push(val);
        }
        out.features.push(rustgis_data::Feature { id: f.id, geometry: f.geometry.clone(), attrs });
    }

    let n_features = out.features.len();
    let mut messages = vec![format!("Calculated zonal statistics for {n_features} feature(s)")];
    if skipped_features > 0 {
        messages.push(format!("{skipped_features} feature(s) had invalid geometry"));
    }
    if skipped_parts > 0 {
        messages.push(format!("{skipped_parts} polygon part(s) could not be projected onto the raster grid; valid parts were retained"));
    }
    if !failure_details.is_empty() {
        for detail in failure_details.iter().take(5) {
            messages.push(detail.clone());
        }
        if failure_details.len() > 5 {
            messages.push(format!("... and {} more feature failure(s)", failure_details.len() - 5));
        }
    }
    Ok(ToolResult { output: Output::Layer(out), messages })
}

/// Returns a borrowed dataset when rows match the full dataset in identity order, or a new dataset containing
/// only the selected rows, in table order, with schema, CRS, and windowed flag preserved.
fn subset<'a>(ds: &'a Dataset, selected_rows: &[usize]) -> Cow<'a, Dataset> {
    // Selections keep the order they were picked in: sort and dedupe so the table's order wins.
    let mut rows: Vec<usize> = selected_rows.iter().copied().filter(|&r| r < ds.features.len()).collect();
    rows.sort_unstable();
    rows.dedup();
    if rows.len() == ds.features.len() {
        Cow::Borrowed(ds)
    } else {
        let mut sub = Dataset::new(ds.name.clone(), ds.geometry_type, ds.fields.clone());
        sub.crs = ds.crs;
        sub.crs_incomplete = ds.crs_incomplete;
        sub.windowed = ds.windowed;
        sub.features = rows.iter().filter_map(|&r| ds.features.get(r).cloned()).collect::<Vec<_>>().into();
        Cow::Owned(sub)
    }
}

/// Normalizes tool-specific parameter aliases before validation and execution.
pub(crate) fn normalize_params(spec: &ToolSpec, p: &mut serde_json::Map<String, Json>) {
    if spec.id == "joinField" {
        normalize_join_field_params(p);
    }
}

pub(crate) fn normalize_join_field_params(p: &mut serde_json::Map<String, Json>) {
    let is_blank = |val: Option<&Json>| val.is_none_or(|v| v.is_null() || v.as_str().is_some_and(|s| s.trim().is_empty()));
    let find_first =
        |map: &serde_json::Map<String, Json>, keys: &[&str]| keys.iter().filter_map(|k| map.get(*k)).find(|v| !is_blank(Some(v))).cloned();

    if is_blank(p.get("target"))
        && let Some(v) = find_first(p, &["target", "input", "target_layer"])
    {
        p.insert("target".to_string(), v);
    }
    if is_blank(p.get("joinTable"))
        && let Some(v) = find_first(p, &["joinTable", "join", "source", "join_table"])
    {
        p.insert("joinTable".to_string(), v);
    }
    if is_blank(p.get("targetField"))
        && let Some(v) = find_first(p, &["targetField", "target_field", "key", "target_key"])
    {
        p.insert("targetField".to_string(), v);
    }
    if is_blank(p.get("joinField")) {
        if let Some(v) = find_first(p, &["joinField", "join_field", "join_key"]) {
            p.insert("joinField".to_string(), v);
        } else if let Some(tf) = p.get("targetField").filter(|v| !is_blank(Some(v))) {
            p.insert("joinField".to_string(), tf.clone());
        }
    }
    if is_blank(p.get("duplicates"))
        && let Some(v) = find_first(p, &["duplicates", "duplicateStrategy", "duplicate_strategy"])
    {
        p.insert("duplicates".to_string(), v);
    }
}

fn join_field_tool(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    let mut normalized_storage;
    let p = match p.as_object() {
        Some(map) => {
            normalized_storage = map.clone();
            normalize_join_field_params(&mut normalized_storage);
            &Json::Object(normalized_storage)
        }
        None => p,
    };

    let target = input(inp, p, "target")?;
    let source = input(inp, p, "joinTable")?;

    let mut messages = Vec::new();

    let target_sel = inp.selection(s(p, "target"));
    let target_rows = rows(inp, p, "target", target);
    if !target_sel.is_empty() {
        if target_rows.is_empty() {
            messages.push(format!(
                "Warning: the selection on target '{}' contains only stale indices (0/{} features selected); output is empty",
                target.name,
                target.features.len()
            ));
        } else if target_rows.len() < target.features.len() {
            messages.push(format!(
                "Processing {} selected feature(s) from target '{}' (out of {})",
                target_rows.len(),
                target.name,
                target.features.len()
            ));
        }
    }

    let source_sel = inp.selection(s(p, "joinTable"));
    let source_rows = rows(inp, p, "joinTable", source);
    if !source_sel.is_empty() {
        if source_rows.is_empty() {
            messages.push(format!(
                "Warning: the selection on source '{}' contains only stale indices (0/{} records selected); no attributes will match",
                source.name,
                source.features.len()
            ));
        } else if source_rows.len() < source.features.len() {
            messages.push(format!(
                "Joining against {} selected record(s) from source '{}' (out of {})",
                source_rows.len(),
                source.name,
                source.features.len()
            ));
        }
    }

    let target_ref = subset(target, &target_rows);
    let source_ref = subset(source, &source_rows);

    let target_field = s(p, "targetField");
    if target_field.is_empty() {
        return Err("target key field is required".into());
    }

    let join_field = s(p, "joinField");
    let duplicate_strategy = crate::attribute_join::DuplicateStrategy::parse(s(p, "duplicates"))?;
    let prefix = s(p, "prefix");

    let explicit_fields: Option<Vec<String>> = match p.get("fields") {
        None | Some(Json::Null) => None,
        Some(Json::Array(arr)) => {
            let names: Vec<String> = arr
                .iter()
                .map(|v| {
                    v.as_str().map(|s| s.trim().to_string()).ok_or_else(|| "Transfer Fields: array entries must be strings".to_string())
                })
                .collect::<Result<Vec<String>, String>>()?
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect();
            Some(names)
        }
        Some(Json::String(str_val)) => {
            let text = str_val.trim();
            if text.is_empty() {
                None
            } else if text.starts_with('[') {
                let names: Vec<String> = serde_json::from_str::<Vec<String>>(text)
                    .map_err(|e| format!("Transfer Fields: invalid JSON array: {e}"))?
                    .into_iter()
                    .map(|n| n.trim().to_string())
                    .filter(|n| !n.is_empty())
                    .collect();
                Some(names)
            } else {
                let names: Vec<String> = text.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                Some(names)
            }
        }
        Some(_) => return Err("Transfer Fields must be a string or JSON array".to_string()),
    };

    let default_out_name = format!("{}_JoinField", target.name);
    let output_name = out_name(p, default_out_name);

    let opts = crate::attribute_join::JoinFieldOptions {
        target_field,
        join_field,
        fields_to_transfer: explicit_fields.as_deref(),
        prefix,
        duplicate_strategy,
        output_name,
    };

    let (out, report) = crate::attribute_join::join_field(&target_ref, &source_ref, &opts)?;
    messages.push(report.message(&target.name));
    messages.extend(report.leading_zero_notice());

    let output = if out.geometry_type == rustgis_data::GeometryType::None { Output::Table(out) } else { Output::Layer(out) };

    Ok(ToolResult { output, messages })
}

/// Keeps polygon boundaries in stored coordinates, including interior rings.
fn polygon_to_line(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    geometry::convert(inp, p, geometry::Conversion::PolygonToLine)
}

/// Emits consecutive vertex pairs without interpolating or projecting them.
fn split_line_at_vertices(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    geometry::split(inp, p)
}

/// Encloses each feature independently in longitude/latitude axes.
fn feature_envelope_to_polygon(inp: &dyn Inputs, p: &Json) -> Result<ToolResult, String> {
    geometry::convert(inp, p, geometry::Conversion::FeatureEnvelopeToPolygon)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geodesic::geodesic_buffer;
    use rustgis_data::Feature;
    use std::collections::HashMap;

    struct Map(HashMap<String, Dataset>);
    impl Inputs for Map {
        fn dataset(&self, layer: &str) -> Option<&Dataset> {
            self.0.get(layer)
        }
        fn selection(&self, _: &str) -> &[usize] {
            &[]
        }
    }

    struct RasterMap<'a> {
        datasets: HashMap<String, Dataset>,
        rasters: HashMap<String, &'a rustgis_data::raster::Raster>,
    }

    impl<'a> Inputs for RasterMap<'a> {
        fn dataset(&self, layer: &str) -> Option<&Dataset> {
            self.datasets.get(layer)
        }
        fn selection(&self, _: &str) -> &[usize] {
            &[]
        }
        fn raster(&self, layer: &str) -> Option<crate::RasterInput<'_>> {
            let r = self.rasters.get(layer)?;
            Some(crate::RasterInput { raster: r, path: None, embedded: None })
        }
    }

    fn sq(x: f64, y: f64, s: f64) -> Geometry {
        Geometry::Polygon(vec![vec![vec![
            Coord::new(x, y),
            Coord::new(x + s, y),
            Coord::new(x + s, y + s),
            Coord::new(x, y + s),
            Coord::new(x, y),
        ]]])
    }

    fn inputs() -> Map {
        let mut polys = Dataset::new("polys", GeometryType::Polygon, vec![Field::new("k", FieldType::Text)]);
        polys.push(Some(sq(0.0, 0.0, 2.0)), vec![Value::Text("a".into())]);
        polys.push(Some(sq(1.0, 0.0, 2.0)), vec![Value::Text("a".into())]);
        polys.push(Some(sq(10.0, 10.0, 1.0)), vec![Value::Text("b".into())]);
        let mut pts = Dataset::new("pts", GeometryType::Point, vec![Field::new("v", FieldType::Double)]);
        for (x, y, v) in [(0.5, 0.5, 1.0), (1.5, 1.5, 2.0), (50.0, 50.0, 3.0)] {
            pts.push(Some(Geometry::Point(Coord::new(x, y))), vec![Value::Float(v)]);
        }
        Map(HashMap::from([("polys".to_string(), polys), ("pts".to_string(), pts)]))
    }

    fn layer(r: ToolResult) -> Dataset {
        match r.output {
            Output::Layer(d) | Output::Table(d) | Output::Update { dataset: d, .. } => d,
            Output::Selection { .. } | Output::Layers(_) | Output::File { .. } => panic!("expected a dataset"),
        }
    }

    #[test]
    fn simplify_rings_do_not_grow_with_tolerance() {
        use geo::{Distance, Euclidean};
        let ring = |x: f64, y: f64, radius: f64| {
            (0..=80)
                .map(|i| {
                    let angle = (i % 80) as f64 * std::f64::consts::TAU / 80.0;
                    let r = radius * (1.0 + 0.1 * (7.0 * angle).sin());
                    Coord::new(x + r * angle.cos(), y + r * angle.sin())
                })
                .collect::<Vec<_>>()
        };
        let original = Geometry::Polygon(vec![vec![ring(10.5, 46.5, 0.1), ring(10.5, 46.52, 0.001)], vec![ring(11.0, 46.5, 0.05)]]);
        let mut ds = Dataset::new("zones", GeometryType::Polygon, vec![]);
        ds.push(Some(original.clone()), vec![]);
        let inputs = Map(HashMap::from([("zones".into(), ds)]));
        let mut previous = original.coord_count();
        for tolerance in [0.0, 100.0, 500.0, 1000.0, 2000.0, 5000.0, 25000.0, 100000.0] {
            let simple = simplify_geometry(&original, tolerance);
            assert!(simple.coord_count() <= previous, "{tolerance} m: {} > {previous}", simple.coord_count());
            previous = simple.coord_count();
            let Geometry::Polygon(parts) = &simple else { panic!("polygon required") };
            assert_eq!(parts.iter().map(Vec::len).collect::<Vec<_>>(), [2, 1]);
            for rings in parts {
                for r in rings {
                    assert!(r.len() >= 4);
                    assert_eq!(r.first(), r.last());
                    assert!(rustgis_data::geometry::ring_area(r).abs() > 1e-14);
                }
            }
            let source = to_multipolygon(&original).unwrap();
            let target = to_multipolygon(&simple).unwrap();
            for (a, b) in source.0.iter().zip(&target.0) {
                for (a, b) in std::iter::once(a.exterior()).chain(a.interiors()).zip(std::iter::once(b.exterior()).chain(b.interiors())) {
                    let deviation = a.0.iter().map(|c| Euclidean.distance(&geo::Point::from(*c), b)).fold(0.0_f64, f64::max);
                    assert!(deviation <= tolerance * merc_scale(feature_lat(&original)) + 1e-6, "{tolerance} m: {deviation}");
                }
            }
            let out =
                layer(crate::run("simplify", &inputs, &serde_json::json!({"input":"zones", "tolerance":tolerance / 1000.0})).unwrap());
            assert_eq!(out.features[0].geometry.as_ref(), Some(&simple));
        }
        assert_eq!(previous, 12, "three closed triangle rings at a very large tolerance");
    }

    #[test]
    fn simplify_keeps_points_lines_and_degenerate_rings() {
        use geo::Area;
        let point = Geometry::Point(Coord::new(10.0, 46.0));
        assert_eq!(simplify_geometry(&point, 1e6), point);
        let line = Geometry::Line(vec![vec![Coord::new(10.0, 46.0), Coord::new(10.1, 46.0), Coord::new(10.2, 46.0)]]);
        assert_eq!(simplify_geometry(&line, 1e6).coord_count(), 2);
        let repeated = geo::LineString::new(vec![geo::Coord { x: 1.0, y: 1.0 }; 6]);
        assert_eq!(simplify_ring(&repeated, 1e6), repeated);
        let collinear = geo::LineString::from(vec![(0.0, 0.0), (1.0, 0.0), (2.0, 0.0), (3.0, 0.0), (0.0, 0.0)]);
        assert_eq!(simplify_ring(&collinear, 1e6), collinear);
        let both_sides = geo::LineString::from(vec![(0.0, 0.0), (3.0, 0.0), (-3.0, 0.0), (0.0, 0.0), (1.0, 0.0), (0.0, 0.0)]);
        assert_eq!(simplify_ring(&both_sides, 1e6), both_sides);
        let concave = geo::LineString::from(vec![(0.0, 0.0), (3.0, 0.0), (2.0, 1.0), (-2.0, 1.0), (-3.0, 0.0), (-2.0, -1.0), (0.0, 0.0)]);
        let mut previous = concave.0.len();
        for tolerance in [0.1, 1.1, 2.5, 1e6] {
            use geo::{Distance, Euclidean};
            let reduced = simplify_ring(&concave, tolerance);
            assert!((4..=previous).contains(&reduced.0.len()));
            previous = reduced.0.len();
            let deviation = concave.0.iter().map(|c| Euclidean.distance(&geo::Point::from(*c), &reduced)).fold(0.0_f64, f64::max);
            assert!(deviation <= tolerance + 1e-12);
            assert!(geo::Polygon::new(reduced, vec![]).unsigned_area() > 0.0);
        }
        assert_eq!(previous, 4);
        let triangle = geo::LineString::from(vec![(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (0.0, 0.0)]);
        assert_eq!(simplify_ring(&triangle, 1e6), triangle);
        let nonfinite = geo::LineString::from(vec![(0.0, 0.0), (f64::INFINITY, 0.0), (1.0, 1.0), (0.0, 1.0), (0.0, 0.0)]);
        assert_eq!(simplify_ring(&nonfinite, 1e6), nonfinite);
    }

    #[test]
    fn all_tools_run() {
        let m = inputs();
        let j = |v: serde_json::Value| v;
        let b = layer(crate::run("buffer", &m, &j(serde_json::json!({"input": "pts", "distance": 50}))).unwrap());
        assert_eq!(b.features.len(), 3);
        let a = b.features[0].geometry.as_ref().unwrap().extent();
        assert!((a.width() - 0.9).abs() < 0.1, "≈100 km wide at the equator: {}", a.width());
        let d = layer(crate::run("dissolve", &m, &j(serde_json::json!({"input": "polys", "field": "k"}))).unwrap());
        assert_eq!(d.features.len(), 2);
        let c = layer(crate::run("clip", &m, &j(serde_json::json!({"input": "pts", "clip": "polys"}))).unwrap());
        assert_eq!(c.features.len(), 2);
        let cnt = layer(crate::run("countPoints", &m, &j(serde_json::json!({"polygons": "polys", "points": "pts"}))).unwrap());
        assert_eq!(cnt.value(0, 1), &Value::Int(2));
        let st = layer(crate::run("summaryStatistics", &m, &j(serde_json::json!({"input": "pts", "field": "v"}))).unwrap());
        assert_eq!(st.value(0, st.field_index("MEAN_v").unwrap()), &Value::Float(2.0));
        match crate::run("selectByLocation", &m, &j(serde_json::json!({"input": "pts", "select": "polys"}))).unwrap().output {
            Output::Selection { rows, .. } => assert_eq!(rows, vec![0, 1]),
            _ => panic!(),
        }
        let sj = layer(crate::run("spatialJoin", &m, &j(serde_json::json!({"target": "pts", "join": "polys"}))).unwrap());
        assert_eq!(sj.features.len(), 3);
        assert_eq!(sj.value(0, sj.field_index("join_count").unwrap()), &Value::Int(1));
        assert_eq!(sj.value(0, sj.field_index("join_k").unwrap()), &Value::Text("a".into()));
        assert_eq!(sj.value(2, sj.field_index("join_count").unwrap()), &Value::Int(0));
        for id in [
            "convexHull",
            "featureToPoint",
            "featureVerticesToPoints",
            "pointsAlongLines",
            "simplify",
            "calculateGeometry",
            "exportFeatures",
            "erase",
        ] {
            let r = crate::run(id, &m, &j(serde_json::json!({"input": "polys", "erase": "polys"})));
            assert!(r.is_ok(), "{id}: {r:?}");
        }
    }

    #[test]
    fn feature_vertices_to_points_numbers_each_vertex_and_keeps_attributes() {
        // A selection on one layer only, so looking it up under another name finds none.
        struct Sel(Map, &'static str, Vec<usize>);
        impl Inputs for Sel {
            fn dataset(&self, layer: &str) -> Option<&Dataset> {
                self.0.dataset(layer)
            }
            fn selection(&self, layer: &str) -> &[usize] {
                if layer == self.1 { &self.2 } else { &[] }
            }
        }
        let c = Coord::new;
        let mut polys = Dataset::new("polys", GeometryType::Polygon, vec![Field::new("k", FieldType::Text)]);
        polys.crs = Some(Crs::WebMercator);
        // A square with a triangular hole, then a second polygon whose ring isn't closed: a
        // closing point is not a vertex of its own, and an open ring loses none.
        let outer = vec![c(0.0, 0.0), c(4.0, 0.0), c(4.0, 4.0), c(0.0, 4.0), c(0.0, 0.0)];
        let hole = vec![c(1.0, 1.0), c(1.0, 2.0), c(2.0, 2.0), c(1.0, 1.0)];
        let open = vec![c(9.0, 9.0), c(9.5, 9.0), c(9.0, 9.5)];
        polys.push(Some(Geometry::Polygon(vec![vec![outer, hole], vec![open]])), vec![Value::Text("a".into())]);
        polys.push(None, vec![Value::Text("no geometry".into())]);
        let mut lines = Dataset::new("lines", GeometryType::Polyline, vec![]);
        lines.push(Some(Geometry::Line(vec![vec![c(0.0, 0.0), c(1.0, 1.0)], vec![c(5.0, 5.0), c(6.0, 6.0), c(7.0, 7.0)]])), vec![]);
        lines.push(Some(Geometry::Line(vec![vec![c(8.0, 8.0), c(9.0, 9.0)]])), vec![]);
        let mut pts = Dataset::new("pts", GeometryType::Point, vec![]);
        pts.push(Some(Geometry::Point(c(0.0, 0.0))), vec![]);
        pts.push(Some(Geometry::MultiPoint(vec![c(1.0, 1.0), c(2.0, 2.0)])), vec![]);
        let table = Dataset::new("table", GeometryType::None, vec![]);
        let m = Map(HashMap::from([
            ("polys".to_string(), polys),
            ("lines".to_string(), lines),
            ("pts".to_string(), pts),
            ("table".to_string(), table),
        ]));
        // A point's [orig_fid, vertex_part, vertex_part_ring, vertex_part_index].
        let place = |v: &Dataset, row: usize| VERTEX_FIELDS.map(|f| v.value(row, v.field_index(f).unwrap()).clone());
        let ints = |a: [i64; 4]| a.map(Value::Int);

        let r = crate::run("featureVerticesToPoints", &m, &serde_json::json!({"input": "polys"})).unwrap();
        assert_eq!(r.messages, vec!["Feature Vertices To Points: 10 points"]);
        let v = layer(r);
        assert_eq!((v.name.as_str(), v.geometry_type, v.crs), ("polys_Vertices", GeometryType::Point, Some(Crs::WebMercator)));
        assert!(v.features.iter().map(|f| f.id).eq(1..=10));
        // The hole's last vertex: part 0, ring 1, third along, with its feature's attribute.
        assert_eq!(v.features[6].geometry, Some(Geometry::Point(c(2.0, 2.0))));
        assert_eq!(place(&v, 6), ints([1, 0, 1, 2]));
        assert_eq!(v.value(6, v.field_index("k").unwrap()), &Value::Text("a".into()));
        // The open ring's last vertex, in the second polygon.
        assert_eq!(place(&v, 9), ints([1, 1, 0, 2]));

        let v = layer(crate::run("featureVerticesToPoints", &m, &serde_json::json!({"input": "lines", "output": "nodes"})).unwrap());
        assert_eq!((v.name.as_str(), v.features.len()), ("nodes", 7));
        assert_eq!((place(&v, 4), place(&v, 6)), (ints([1, 1, 0, 2]), ints([2, 0, 0, 1])));
        // A point layer's points are their own vertices; a table has none.
        let v = layer(crate::run("featureVerticesToPoints", &m, &serde_json::json!({"input": "pts"})).unwrap());
        // A multipoint's points are its parts.
        assert_eq!((v.features.len(), place(&v, 1), place(&v, 2)), (3, ints([2, 0, 0, 0]), ints([2, 1, 0, 0])));
        assert!(crate::run("featureVerticesToPoints", &m, &serde_json::json!({"input": "table"})).is_err());
        // Only the selected feature's vertices.
        let v = layer(crate::run("featureVerticesToPoints", &Sel(m, "lines", vec![1]), &serde_json::json!({"input": "lines"})).unwrap());
        assert_eq!(
            v.features.iter().map(|f| f.geometry.clone()).collect::<Vec<_>>(),
            [Some(Geometry::Point(c(8.0, 8.0))), Some(Geometry::Point(c(9.0, 9.0)))]
        );
        assert_eq!(place(&v, 0), ints([2, 0, 0, 0]));
    }

    #[test]
    fn feature_vertices_to_points_keeps_same_named_fields() {
        let c = Coord::new;
        let old = Field { name: "VERTEX_PART_INDEX".into(), ty: FieldType::Text, alias: Some("Old".into()) };
        let mut clash = Dataset::new("clash", GeometryType::Polyline, vec![old.clone(), Field::new("vertex_part", FieldType::Integer)]);
        clash.push(Some(Geometry::Line(vec![vec![c(0.0, 0.0), c(1.0, 1.0)]])), vec![Value::Text("old".into()), Value::Int(9)]);
        let m = Map(HashMap::from([("clash".to_string(), clash)]));

        // The input's fields stay as they were; the new ones of a name it has get a number.
        let v = layer(crate::run("featureVerticesToPoints", &m, &serde_json::json!({"input": "clash"})).unwrap());
        let names: Vec<_> = v.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["VERTEX_PART_INDEX", "vertex_part", "orig_fid", "vertex_part_2", "vertex_part_ring", "vertex_part_index_2"]);
        assert_eq!(v.fields[0], old);
        // The second point: the input's values, then orig_fid 1, part 0, ring 0, index 1.
        let row = [Value::Text("old".into()), Value::Int(9), Value::Int(1), Value::Int(0), Value::Int(0), Value::Int(1)];
        assert_eq!(v.features[1].attrs, row);
    }

    #[test]
    fn feature_vertices_to_points_keeps_a_wrapped_negative_id() {
        // A GeoPackage feature with fid -2 is read with the id -2 wraps to; its points say -2.
        let c = Coord::new;
        let mut gpkg = Dataset::new("gpkg", GeometryType::Polyline, vec![]);
        let line = Some(Geometry::Line(vec![vec![c(0.0, 0.0), c(1.0, 1.0)]]));
        gpkg.features = vec![Feature { id: (-2i64).cast_unsigned(), geometry: line, attrs: vec![] }].into();
        let m = Map(HashMap::from([("gpkg".to_string(), gpkg)]));
        let v = layer(crate::run("featureVerticesToPoints", &m, &serde_json::json!({"input": "gpkg"})).unwrap());
        assert_eq!(v.value(1, v.field_index("orig_fid").unwrap()), &Value::Int(-2));
    }

    #[test]
    fn feature_vertices_to_points_estimates_and_asks_for_memory() {
        let own = [Value::Text("abc".into()), Value::Int(1)];
        // A point with no attributes is the base: its fixed part and the row's allocation. On top
        // of it, each of the 6 values, the text (3 B) and its allocation (16 B).
        let base = point_bytes(&[], 0);
        assert_eq!(point_bytes(&own, 6), base + u64::try_from(6 * size_of::<Value>() + 3 + 16).unwrap());
        // An empty text allocates nothing, so it costs what a null does.
        assert_eq!(point_bytes(&[Value::Text(String::new())], 6), point_bytes(&[Value::Null], 6));
        // Every vertex is counted; the memory only when asked, for the attributes copied.
        let c = Coord::new;
        let line = Geometry::Line(vec![vec![c(0.0, 0.0), c(1.0, 1.0), c(2.0, 2.0)]]);
        let mut attrs = own.to_vec();
        attrs.push(Value::Text("not copied".into()));
        let a = Feature { id: 1, geometry: Some(line), attrs };
        let b = Feature { id: 2, geometry: None, attrs: vec![] };
        assert_eq!(size_output_points([&a, &b], 2, 6, false, u64::MAX, vertex_count), (3, None));
        assert_eq!(size_output_points([&a, &b], 2, 6, true, u64::MAX, vertex_count), (3, Some(3 * point_bytes(&own, 6))));
        // Over the limit after the first feature, the second isn't counted.
        assert_eq!(size_output_points([&a, &a], 2, 6, true, 1, vertex_count), (3, Some(3 * point_bytes(&own, 6))));
        // Refused when the memory isn't there, saying what the points would need.
        assert!(check_room("Tool", 3, 1 << 20, "", |_| true).is_ok());
        let e = check_room("Tool", 3, (1 << 20) + 1, "use fewer", |need| need < 1 << 20).unwrap_err();
        assert_eq!(e, "Tool: 3 points would need about 2 MiB, more than the browser can give now: use fewer");
    }

    /// A line layer `lines` with a feature for each geometry, named `f0`, `f1`…, and a point layer.
    fn along_inputs(geometries: Vec<Geometry>) -> Map {
        let mut lines = Dataset::new("lines", GeometryType::Polyline, vec![Field::new("name", FieldType::Text)]);
        for (i, g) in geometries.into_iter().enumerate() {
            lines.push(Some(g), vec![Value::Text(format!("f{i}"))]);
        }
        let mut pts = Dataset::new("pts", GeometryType::Point, vec![]);
        pts.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![]);
        Map(HashMap::from([("lines".to_string(), lines), ("pts".to_string(), pts)]))
    }

    fn along(m: &dyn Inputs, params: serde_json::Value) -> Dataset {
        layer(crate::run("pointsAlongLines", m, &params).unwrap())
    }

    /// A point's place, and its `distance` and `angle`.
    fn place(v: &Dataset, row: usize) -> (Coord, f64, f64) {
        let float = |field: &str| match v.value(row, v.field_index(field).unwrap()) {
            Value::Float(x) => *x,
            other => panic!("{field}: {other:?}"),
        };
        match &v.features[row].geometry {
            Some(Geometry::Point(c)) => (*c, float("distance"), float("angle")),
            g => panic!("{g:?}"),
        }
    }

    /// Longitude along the equator, a geodesic: a meter is 1/6,378,137 of a radian.
    fn equator_degrees(meters: f64) -> f64 {
        (meters / 6_378_137.0).to_degrees()
    }

    #[test]
    fn points_along_lines_spaces_points_on_the_ground() {
        let c = Coord::new;
        let m = along_inputs(vec![Geometry::Line(vec![vec![c(0.0, 0.0), c(0.1, 0.0)]])]);
        // 0.1° of the equator is 11.13 km: a point every kilometer from 0 to 11.
        let len_km = 6_378_137.0 * 0.1f64.to_radians() / 1000.0;
        let r = crate::run("pointsAlongLines", &m, &serde_json::json!({"input": "lines"})).unwrap();
        assert_eq!(r.messages, ["Points Along Lines: 12 points"]);
        let v = layer(r);
        assert_eq!((v.name.as_str(), v.geometry_type, v.features.len()), ("lines_PointsAlong", GeometryType::Point, 12));
        let names: Vec<_> = v.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["name", "orig_fid", "distance", "angle"]);
        assert!(v.features.iter().map(|f| f.id).eq(1..=12));
        for k in 0..12 {
            let (p, d, angle) = place(&v, k);
            let km = k as f64;
            // 1e-9° is a tenth of a millimeter.
            assert!((p.x - equator_degrees(km * 1000.0)).abs() < 1e-9 && p.y.abs() < 1e-12, "{k}: {p:?}");
            assert!((d - km).abs() < 1e-9 && (angle - 90.0).abs() < 1e-9, "{k}: {d} {angle}");
            assert_eq!(&v.features[k].attrs[..2], &[Value::Text("f0".into()), Value::Int(1)]);
        }

        // The end as well, 11.13 km along.
        let v = along(&m, serde_json::json!({"input": "lines", "includeEnd": true}));
        let (p, d, _) = place(&v, 12);
        assert_eq!(v.features.len(), 13);
        assert!((p.x - 0.1).abs() < 1e-12 && (d - len_km).abs() < 1e-9, "{p:?} {d}");
        // Half a kilometer in from either end: 0.5 to 10.5 km, then the end at 10.63 km.
        let v = along(&m, serde_json::json!({"input": "lines", "startOffset": 0.5, "endOffset": "0.5", "includeEnd": "true"}));
        let ds: Vec<f64> = (0..v.features.len()).map(|k| place(&v, k).1).collect();
        let want: Vec<f64> = (0..11).map(|k| 0.5 + f64::from(k)).chain([len_km - 0.5]).collect();
        assert!(ds.len() == want.len() && ds.iter().zip(&want).all(|(a, b)| (a - b).abs() < 1e-9), "{ds:?}");
        // Two miles apart (3,218.688 m), the distances in miles: 11.13 km is 6.92 miles.
        let v = along(&m, serde_json::json!({"input": "lines", "distance": 2, "units": "miles"}));
        assert_eq!(v.features.len(), 4);
        let (p, d, _) = place(&v, 3);
        assert!((p.x - equator_degrees(6.0 * 1609.344)).abs() < 1e-9 && (d - 6.0).abs() < 1e-9, "{p:?} {d}");
        // Longer than the line: only its start.
        assert_eq!(along(&m, serde_json::json!({"input": "lines", "distance": 20})).features.len(), 1);
        // Offsets that leave nothing of it: no points.
        assert!(along(&m, serde_json::json!({"input": "lines", "startOffset": 6, "endOffset": 6})).features.is_empty());
    }

    #[test]
    fn points_along_lines_turn_at_vertices_and_close_rings() {
        let c = Coord::new;
        // East along the equator for 1,113.19 m, then north. A point on the corner faces between the two.
        let leg = 6_378_137.0 * 0.01f64.to_radians();
        let m = along_inputs(vec![Geometry::Line(vec![vec![c(0.0, 0.0), c(0.01, 0.0), c(0.01, 0.01)]])]);
        let v = along(&m, serde_json::json!({"input": "lines", "distance": leg, "units": "Meters"}));
        assert_eq!(v.features.len(), 2);
        let ((start, _, east), (corner, _, north_east)) = (place(&v, 0), place(&v, 1));
        assert_eq!((start, corner), (c(0.0, 0.0), c(0.01, 0.0)));
        assert!((east - 90.0).abs() < 1e-9 && (north_east - 45.0).abs() < 1e-9, "{east} {north_east}");
        // The end of an open line faces the way its last segment goes: north.
        let v = along(&m, serde_json::json!({"input": "lines", "distance": 10, "includeEnd": true}));
        let (end, _, north) = place(&v, 1);
        assert!(end == c(0.01, 0.01) && north.min(360.0 - north) < 1e-9, "{end:?} {north}");

        // Rings, each walked from its first vertex: a square's outer ring (east first, back south),
        // and a hole left open (north first, back west along its closing segment).
        let outer = vec![c(0.0, 0.0), c(0.01, 0.0), c(0.01, 0.01), c(0.0, 0.01), c(0.0, 0.0)];
        let hole = vec![c(0.004, 0.004), c(0.004, 0.006), c(0.006, 0.006), c(0.006, 0.004)];
        let mut polys = Dataset::new("polys", GeometryType::Polygon, vec![]);
        polys.push(Some(Geometry::Polygon(vec![vec![outer, hole]])), vec![]);
        let m = Map(HashMap::from([("polys".to_string(), polys)]));
        // Farther than either ring is long: a point at each ring's first vertex.
        let v = along(&m, serde_json::json!({"input": "polys", "distance": 100}));
        assert_eq!(v.features.len(), 2);
        let ((a, _, south_east), (b, _, north_west)) = (place(&v, 0), place(&v, 1));
        assert_eq!((a, b), (c(0.0, 0.0), c(0.004, 0.004)));
        // Halfway between south (180°) and east, and between west (270° to within a hair: the
        // closing segment is a geodesic, not a parallel) and north.
        assert!((south_east - 135.0).abs() < 1e-9 && (north_west - 315.0).abs() < 1e-6, "{south_east} {north_west}");
        // Every 100 m, each ring's end is its first point again, which is made once.
        let v = along(&m, serde_json::json!({"input": "polys", "distance": 100, "units": "Meters", "includeEnd": true}));
        let firsts = (0..v.features.len()).filter(|&k| [a, b].contains(&place(&v, k).0)).count();
        assert_eq!((v.features.len() > 50, firsts), (true, 2));
        // Half a kilometer in, then every kilometer: the outer ring (4.44 km) at 0.5 to 3.5 and its
        // end, which is its first vertex, facing between its last segment and its first; the hole
        // (0.89 km) at 0.5 and its end.
        let v = along(&m, serde_json::json!({"input": "polys", "distance": 1, "startOffset": 0.5, "includeEnd": true}));
        let (end, d, angle) = place(&v, 4);
        assert_eq!(v.features.len(), 7);
        assert!(end == c(0.0, 0.0) && (angle - 135.0).abs() < 1e-9 && d > 4.4 && d < 4.5, "{end:?} {d} {angle}");

        // A ring of two vertices goes out along the equator and back. A step of one leg lands on
        // its far vertex, then on its end, which is its start: two points.
        let mut thin = Dataset::new("thin", GeometryType::Polygon, vec![]);
        thin.push(Some(Geometry::Polygon(vec![vec![vec![c(0.0, 0.0), c(0.01, 0.0)]]])), vec![]);
        // A vertex a micrometer from the one before (at a line's start, and before a ring closes)
        // doesn't turn the angle: the line still leaves east, the ring still comes back from the south.
        let mut dup = Dataset::new("dup", GeometryType::Polygon, vec![]);
        let ring = vec![c(0.0, 0.0), c(0.01, 0.0), c(0.01, 0.01), c(0.0, 0.01), c(1e-11, 1e-11)];
        dup.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![]);
        let mut m = along_inputs(vec![Geometry::Line(vec![vec![c(0.0, 0.0), c(0.0, 1e-11), c(0.01, 0.0)]])]);
        m.0.extend([("thin".to_string(), thin), ("dup".to_string(), dup)]);
        let v = along(&m, serde_json::json!({"input": "thin", "distance": leg, "units": "Meters", "includeEnd": true}));
        assert_eq!((0..v.features.len()).map(|k| place(&v, k).0).collect::<Vec<_>>(), [c(0.0, 0.0), c(0.01, 0.0)]);
        let east = place(&along(&m, serde_json::json!({"input": "lines"})), 0).2;
        let south_east = place(&along(&m, serde_json::json!({"input": "dup", "distance": 100})), 0).2;
        assert!((east - 90.0).abs() < 1e-9 && (south_east - 135.0).abs() < 1e-6, "{east} {south_east}");
    }

    #[test]
    fn points_along_lines_survives_hostile_coordinates() {
        let c = Coord::new;
        // Latitudes past the poles, longitudes at the ends of f64, a pole and antipodes: walked
        // without a panic, every point on the Earth (a vertex past a pole at the pole, where the
        // walk measured it). The line ends at -95° (the South Pole), not where it starts (the
        // North Pole), so it stays open and its end gets a point of its own; the same vertices
        // as a polygon's ring are walked round to the start.
        let line = vec![c(0.0, 1e300), c(1e308, 0.0), c(-1e308, 90.0), c(0.0, -90.0), c(180.0, 0.0), c(0.0, -95.0)];
        let ring = vec![c(0.0, 1e300), c(1e308, 0.0), c(-1e308, 90.0), c(0.0, -90.0), c(180.0, 0.0), c(f64::NAN, 0.0)];
        let mut polys = Dataset::new("polys", GeometryType::Polygon, vec![]);
        polys.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![]);
        let mut m = along_inputs(vec![Geometry::Line(vec![line])]);
        m.0.insert("polys".to_string(), polys);
        let on_earth = |v: &Dataset| {
            assert!(!v.features.is_empty());
            for k in 0..v.features.len() {
                let (p, d, angle) = place(v, k);
                let on_earth = (-180.0..=180.0).contains(&p.x) && (-90.0..=90.0).contains(&p.y);
                assert!(on_earth && d.is_finite() && (0.0..360.0).contains(&angle), "{k}: {p:?} {d} {angle}");
            }
        };
        let run = |input: &str| {
            crate::run("pointsAlongLines", &m, &serde_json::json!({"input": input, "distance": 1000, "includeEnd": true})).unwrap()
        };
        let r = run("lines");
        // Every vertex finite (if wild): none passed over; the two past a pole (1e300 and -95)
        // are taken as it, and the tool says so.
        assert_eq!(
            r.messages[1..],
            ["Points Along Lines: 2 vertex(es) with a latitude past a pole found (latitudes limited to the poles when measuring segments)"],
            "{:?}",
            r.messages
        );
        let v = layer(r);
        on_earth(&v);
        assert_eq!((place(&v, 0).0.y, place(&v, v.features.len() - 1).0.y), (90.0, -90.0));
        // The ring's vertex missing its longitude is passed over, the one past a pole taken as
        // it, and the tool says both, each once.
        let r = run("polys");
        assert_eq!(
            r.messages[1..],
            [
                "Points Along Lines: 1 vertex(es) with a coordinate missing or infinite passed over",
                "Points Along Lines: 1 vertex(es) with a latitude past a pole found (latitudes limited to the poles when measuring segments)"
            ],
        );
        let v = layer(r);
        on_earth(&v);
        assert_eq!(place(&v, 0).0.y, 90.0);
        // Lines too: one left with a single vertex (no points), one trimmed at its end; both
        // counted, in one message.
        let lossy = along_inputs(vec![
            Geometry::Line(vec![vec![c(0.0, 0.0), c(f64::NAN, 0.0)]]),
            Geometry::Line(vec![vec![c(0.0, 0.0), c(1.0, 0.0), c(f64::INFINITY, 5.0)]]),
        ]);
        let r = crate::run("pointsAlongLines", &lossy, &serde_json::json!({"input": "lines", "distance": 10})).unwrap();
        assert_eq!(r.messages[1], "Points Along Lines: 2 vertex(es) with a coordinate missing or infinite passed over");
        assert!(!layer(r).features.is_empty());
        // A line whose ends are the same wild coordinate is a ring, closed without a geodesic.
        let wild = vec![c(1e308, 1e300), c(0.0, -95.0), c(-1e308, 0.0), c(1e308, 1e300)];
        let m = along_inputs(vec![Geometry::Line(vec![wild])]);
        on_earth(&along(&m, serde_json::json!({"input": "lines", "distance": 1000, "includeEnd": true})));
    }

    /// Mixed line input reports skipped point and absent geometries; clean lines have no loss notice.
    #[test]
    fn points_along_lines_reports_skipped_features() {
        let line = Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(0.1, 0.0)]]);
        let mut inputs = along_inputs(vec![line, Geometry::Point(Coord::new(0.0, 0.0)), Geometry::MultiPoint(vec![Coord::new(1.0, 0.0)])]);
        inputs.0.get_mut("lines").unwrap().push(None, vec![Value::Text("missing".into())]);
        let params = serde_json::json!({"input": "lines", "distance": 1});
        let result = crate::run("pointsAlongLines", &inputs, &params).unwrap();
        assert_eq!(
            result.messages[1..],
            [
                "Points Along Lines: 1 feature(s) without geometry skipped",
                "Points Along Lines: 2 point feature(s) skipped; only lines and polygon rings produce points",
            ]
        );
        assert!(!layer(result).features.is_empty());
        inputs.0.get_mut("lines").unwrap().features.truncate(1);
        assert_eq!(crate::run("pointsAlongLines", &inputs, &params).unwrap().messages.len(), 1);
    }

    /// Degenerate features are reported once; usable parts and offset-only empty output are clean.
    #[test]
    fn points_along_lines_reports_features_without_segments() {
        let c = Coord::new;
        let inputs = along_inputs(vec![
            Geometry::Line(vec![]),
            Geometry::Polygon(vec![]),
            Geometry::Line(vec![vec![c(0.0, 0.0)]]),
            Geometry::Line(vec![vec![c(1.0, 0.0), c(1.0, 0.0)]]),
            Geometry::Line(vec![vec![c(2.0, 0.0), c(2.0 + equator_degrees(0.0005), 0.0)]]),
            Geometry::Line(vec![vec![c(3.0, 0.0)], vec![c(4.0, 0.0), c(4.1, 0.0)]]),
        ]);
        let result = crate::run("pointsAlongLines", &inputs, &serde_json::json!({"input":"lines", "distance":1})).unwrap();
        assert_eq!(result.messages[1..], ["Points Along Lines: 5 line or polygon feature(s) without measurable segments skipped"]);
        assert!(!layer(result).features.is_empty());
        let clean = along_inputs(vec![Geometry::Line(vec![vec![c(0.0, 0.0), c(0.1, 0.0)]])]);
        let result =
            crate::run("pointsAlongLines", &clean, &serde_json::json!({"input":"lines", "distance":1, "startOffset":100})).unwrap();
        assert_eq!(result.messages.len(), 1, "offsets do not make a feature geometrically unusable");
        assert!(layer(result).features.is_empty());
    }

    /// Invalid vertices are counted as encountered even when their degenerate part emits no points.
    #[test]
    fn points_along_lines_reports_past_pole_vertices_without_claiming_placement() {
        let inputs = along_inputs(vec![Geometry::Line(vec![vec![Coord::new(0.0, 95.0)]])]);
        let result = crate::run("pointsAlongLines", &inputs, &serde_json::json!({"input":"lines", "distance":1})).unwrap();
        assert_eq!(
            result.messages[1..],
            [
                "Points Along Lines: 1 vertex(es) with a latitude past a pole found (latitudes limited to the poles when measuring segments)",
                "Points Along Lines: 1 line or polygon feature(s) without measurable segments skipped",
            ]
        );
        assert!(layer(result).features.is_empty());
    }

    #[test]
    fn point_tools_keep_what_the_input_lost_on_reading() {
        // An input that lost features or Z and M values on reading marks its points so too; one
        // that lost nothing doesn't.
        let c = Coord::new;
        let line = Geometry::Line(vec![vec![c(0.0, 0.0), c(0.1, 0.0)]]);
        for lost in [false, true] {
            let mut m = along_inputs(vec![line.clone()]);
            let ds = m.0.get_mut("lines").unwrap();
            (ds.crs_incomplete, ds.zm_dropped) = (lost, lost);
            for (tool, p) in [
                ("pointsAlongLines", serde_json::json!({"input": "lines"})),
                ("featureVerticesToPoints", serde_json::json!({"input": "lines"})),
            ] {
                let out = layer(crate::run(tool, &m, &p).unwrap());
                assert_eq!((out.crs_incomplete, out.zm_dropped), (lost, lost), "{tool}");
            }
        }
    }

    #[test]
    fn points_along_lines_stops_when_canceled() {
        // 400 lines of a degree along the equator, a point every 100 m: some 445,000 geodesics.
        let c = Coord::new;
        let lines: Vec<Geometry> =
            (0..400).map(|i| Geometry::Line(vec![vec![c(0.0, f64::from(i) * 0.1), c(1.0, f64::from(i) * 0.1)]])).collect();
        let m = along_inputs(lines);
        let params = serde_json::json!({"input": "lines", "distance": 100, "units": "Meters"});
        let control = crate::control::Control::new();
        control.cancel();
        assert_eq!(
            crate::run_with("pointsAlongLines", &m, &params, &control).err().as_deref(),
            Some("Points Along Lines was cancelled before it started")
        );
        // Asked once it has walked some lines: it stops within a line of where it was asked (not
        // at the end, where a run that only checked once it was done would stop).
        let control = crate::control::Control::new();
        let watcher = {
            let control = control.clone();
            std::thread::spawn(move || {
                let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
                while control.progress().is_none_or(|(done, _)| done == 0) && std::time::Instant::now() < end {
                    std::thread::yield_now();
                }
                let asked = control.progress().map_or(0, |(done, _)| done);
                control.cancel();
                asked
            })
        };
        let r = crate::run_with("pointsAlongLines", &m, &params, &control);
        let asked = watcher.join().unwrap_or(0);
        let e = r.err().unwrap_or_default();
        assert!(e.starts_with("Points Along Lines was cancelled "), "{e}");
        let (done, total) = control.progress().unwrap_or_default();
        assert!(total == 400 && done <= asked + 1 && done < total, "{done} of {total} lines, asked at {asked}");
    }

    #[test]
    fn points_along_lines_makes_a_point_each_time_a_line_passes_a_place() {
        // Out along the equator and back (A to B to A), then north (to C): a step of two legs
        // lands on A going out and on A again coming back, both points. Out to B and on to D, then
        // back to B (a lollipop's stick): from a leg in, every two legs lands on B both times.
        let c = Coord::new;
        let leg = 6_378_137.0 * 0.01f64.to_radians();
        let (a, b, d) = (c(0.0, 0.0), c(0.01, 0.0), c(0.02, 0.0));
        let m = along_inputs(vec![Geometry::Line(vec![vec![a, b, a, c(0.0, 0.01)]]), Geometry::Line(vec![vec![a, b, d, b]])]);
        // Feature `fid`'s points: where, and how far along.
        let of = |v: &Dataset, fid: i64| -> Vec<(Coord, f64)> {
            let at = v.field_index("orig_fid").unwrap();
            (0..v.features.len()).filter(|&k| *v.value(k, at) == Value::Int(fid)).map(|k| (place(v, k).0, place(v, k).1)).collect()
        };
        let first = of(&along(&m, serde_json::json!({"input": "lines", "distance": 2.0 * leg, "units": "Meters"})), 1);
        assert!(first.len() == 2 && first[1].0 == a && (first[1].1 - 2.0 * leg).abs() < 1e-6, "{first:?}");
        let second = of(&along(&m, serde_json::json!({"input": "lines", "distance": 2.0 * leg, "startOffset": leg, "units": "Meters"})), 2);
        assert!(second.len() == 2 && second.iter().all(|(p, _)| *p == b) && (second[1].1 - 3.0 * leg).abs() < 1e-6, "{second:?}");
    }

    #[test]
    fn points_along_lines_walks_a_parallel_round_the_world_as_a_ring() {
        // Written from -180° to 180° at 60°N in four equal edges, it ends where it starts: a ring,
        // so a step of one edge puts a point on each vertex, the last one once, end or no end.
        let c = Coord::new;
        let line = vec![c(-180.0, 60.0), c(-90.0, 60.0), c(0.0, 60.0), c(90.0, 60.0), c(180.0, 60.0)];
        let edge = rustgis_geom::wgs84_inverse(c(-180.0, 60.0), c(-90.0, 60.0)).0;
        let m = along_inputs(vec![Geometry::Line(vec![line])]);
        let v = along(&m, serde_json::json!({"input": "lines", "distance": edge, "units": "Meters", "includeEnd": true}));
        let xs: Vec<f64> = (0..v.features.len()).map(|k| place(&v, k).0.x).collect();
        assert_eq!(xs, [-180.0, -90.0, 0.0, 90.0]);
    }

    #[test]
    fn points_along_lines_walks_a_ring_offset_a_little_once_round() {
        // A ring some 10,000 km round, a quarter of it a step: from its first vertex, or 5 mm on
        // (within the walk's tolerance of it, a centimeter), four points, one of them there, never
        // more than the list is sized for.
        let c = Coord::new;
        let ring = vec![vec![c(0.0, 0.0), c(30.0, 0.0), c(30.0, 30.0), c(0.0, 30.0)]];
        let segs: Vec<Segment> = segments(&ring, true).collect();
        let len: f64 = segs.iter().map(|s| s.len).sum();
        for start in [0.0, 0.005] {
            let spacing = Spacing { step: len / 4.0, start, end: 0.0, with_end: true };
            let mut at = Vec::new();
            walk_line(&segs, true, spacing, |p, _, _| {
                at.push(p);
                ControlFlow::Continue(())
            });
            let first = at.iter().filter(|p| rustgis_geom::wgs84_inverse(**p, c(0.0, 0.0)).0 < 0.01).count();
            assert!(at.len() == 4 && at.len() <= spacing.most(len, true) && first == 1, "{start}: {at:?}");
        }
    }

    #[test]
    fn points_along_lines_walks_each_part_on_its_own() {
        // A selection on one layer only, as in Feature Vertices To Points' test.
        struct Sel(Map, Vec<usize>);
        impl Inputs for Sel {
            fn dataset(&self, layer: &str) -> Option<&Dataset> {
                self.0.dataset(layer)
            }
            fn selection(&self, layer: &str) -> &[usize] {
                if layer == "lines" { &self.1 } else { &[] }
            }
        }
        let c = Coord::new;
        let nan = f64::NAN;
        let m = along_inputs(vec![
            // Two parts: 2.23 km and 1.11 km long, each from 0.
            Geometry::Line(vec![vec![c(0.0, 0.0), c(0.02, 0.0)], vec![c(1.0, 0.0), c(1.01, 0.0)]]),
            // A vertex that isn't finite is left out, a lone vertex or a repeated one has no length,
            // and a point has no line.
            Geometry::Line(vec![vec![c(2.0, 0.0), c(nan, 5.0), c(2.01, 0.0)], vec![c(3.0, 0.0)], vec![c(4.0, 0.0), c(4.0, 0.0)]]),
            Geometry::MultiPoint(vec![c(5.0, 0.0), c(5.01, 0.0)]),
        ]);
        let v = along(&m, serde_json::json!({"input": "lines"}));
        let at: Vec<(f64, f64)> = (0..v.features.len()).map(|k| (place(&v, k).0.x, place(&v, k).1)).collect();
        let want =
            [(0.0, 0.0), (equator_degrees(1000.0), 1.0), (equator_degrees(2000.0), 2.0), (1.0, 0.0), (1.0 + equator_degrees(1000.0), 1.0)];
        let second = [(2.0, 0.0), (2.0 + equator_degrees(1000.0), 1.0)];
        assert_eq!(at.len(), want.len() + second.len(), "{at:?}");
        for ((x, d), (wx, wd)) in at.iter().zip(want.iter().chain(&second)) {
            assert!((x - wx).abs() < 1e-9 && (d - wd).abs() < 1e-9, "{at:?}");
        }
        let fids: Vec<_> = v.features.iter().map(|f| f.attrs[1].clone()).collect();
        assert_eq!(fids, [1, 1, 1, 1, 1, 2, 2].map(Value::Int));
        // Only the selected feature.
        let v = along(&Sel(m, vec![1]), serde_json::json!({"input": "lines"}));
        assert_eq!((v.features.len(), v.features[0].attrs[0].clone()), (2, Value::Text("f1".into())));
    }

    #[test]
    fn points_along_lines_folds_longitudes_and_keeps_same_named_fields() {
        let c = Coord::new;
        let fields = vec![Field::new("DISTANCE", FieldType::Text), Field::new("angle", FieldType::Integer)];
        let mut lines = Dataset::new("lines", GeometryType::Polyline, fields);
        lines.push(Some(Geometry::Line(vec![vec![c(179.995, 0.0), c(-179.995, 0.0)]])), vec![Value::Text("far".into()), Value::Int(7)]);
        let m = Map(HashMap::from([("lines".to_string(), lines)]));
        let v = along(&m, serde_json::json!({"input": "lines", "distance": 500, "units": "Meters", "includeEnd": true}));
        let names: Vec<_> = v.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["DISTANCE", "angle", "orig_fid", "distance_2", "angle_2"]);
        assert_eq!(&v.features[2].attrs[..3], &[Value::Text("far".into()), Value::Int(7), Value::Int(1)]);
        // Across 180°, a point is folded onto the far side, where the line's end is.
        let xs: Vec<f64> = v.features.iter().filter_map(|f| f.geometry.as_ref()?.label_point()).map(|p| p.x).collect();
        let want = [179.995, 179.995 + equator_degrees(500.0), 179.995 + equator_degrees(1000.0) - 360.0, -179.995];
        assert!(xs.len() == 4 && xs.iter().zip(want).all(|(x, w)| (x - w).abs() < 1e-9), "{xs:?}");
    }

    #[test]
    fn points_along_lines_carries_a_line_on_across_the_antimeridian() {
        let c = Coord::new;
        // One line split at the antimeridian, as RFC 7946 asks GeoJSON to, heading east and west:
        // 0.01° on either side (2,226.39 m in all), a point every 500 m from one end on.
        let east = vec![vec![c(179.99, 0.0), c(180.0, 0.0)], vec![c(-180.0, 0.0), c(-179.99, 0.0)]];
        let west = vec![vec![c(-179.99, 0.0), c(-180.0, 0.0)], vec![c(180.0, 0.0), c(179.99, 0.0)]];
        // Parts that touch the antimeridian at different places are two lines, each from 0.
        let apart = vec![vec![c(179.99, 0.0), c(180.0, 0.0)], vec![c(-180.0, 0.001), c(-179.99, 0.001)]];
        let m = along_inputs(vec![Geometry::Line(east), Geometry::Line(west), Geometry::Line(apart)]);
        let v = along(&m, serde_json::json!({"input": "lines", "distance": 500, "units": "Meters"}));
        let fold = rustgis_geom::fold_lon;
        // The distances are in meters, as the spacing is.
        let (meters, x) = (|k: f64| k * 500.0, |k: f64| equator_degrees(500.0 * k));
        let mut want: Vec<(f64, f64)> = (0..5).map(|k| (fold(179.99 + x(f64::from(k))), meters(f64::from(k)))).collect();
        want.extend((0..5).map(|k| (fold(-179.99 - x(f64::from(k))), meters(f64::from(k)))));
        want.extend([0, 1, 2].map(|k| (179.99 + x(f64::from(k)), meters(f64::from(k)))));
        want.extend([0, 1, 2].map(|k| (-180.0 + x(f64::from(k)), meters(f64::from(k)))));
        assert_eq!(v.features.len(), want.len());
        for (k, (wx, wd)) in want.into_iter().enumerate() {
            let (p, d, _) = place(&v, k);
            assert!((p.x - wx).abs() < 1e-9 && (d - wd).abs() < 1e-9, "{k}: {p:?} {d}, want {wx} {wd}");
        }
    }

    #[test]
    fn points_along_lines_joins_parts_only_where_they_cross_the_antimeridian() {
        let c = Coord::new;
        // A kilometer either side, so a point every 500 m lands on the antimeridian. Crossing it, as
        // splitters write it: at 180° and −180°, both at −180°, and one a rounding past 180° (Web
        // Mercator's edge, which folds to just over −180°).
        let d = equator_degrees(1000.0);
        let past = 180.0 + 3e-14;
        let crossing = |a: f64, b: f64| vec![vec![c(180.0 - d, 0.0), c(a, 0.0)], vec![c(b, 0.0), c(-180.0 + d, 0.0)]];
        let lines = vec![
            crossing(180.0, -180.0),
            crossing(-180.0, -180.0),
            crossing(past, -180.0),
            // Touching the antimeridian and turning back on the same side; reaching it from opposite
            // sides but the second part heading back west (its first edge 1° west on the ground);
            // and ends 1.05 mm apart, more than the walk passes over: each part from 0.
            vec![vec![c(179.99, 0.0), c(180.0, 0.0)], vec![c(180.0, 0.0), c(179.99, 0.01)]],
            vec![vec![c(179.99, 0.0), c(180.0, 0.0)], vec![c(-180.0, 0.0), c(179.99, 0.01)]],
            vec![vec![c(179.99, 0.0), c(180.0, 0.0)], vec![c(-180.0, 9.5e-9), c(-179.99, 0.0)]],
        ];
        let m = along_inputs(lines.into_iter().map(Geometry::Line).collect());
        let v = along(&m, serde_json::json!({"input": "lines", "distance": 500, "units": "Meters"}));
        let rows = &v;
        let of = |fid: i64| (0..rows.features.len()).filter(move |&k| rows.features[k].attrs[1] == Value::Int(fid));
        // Each line's points from 0 on: the crossings one line each, the others two each.
        let starts = |fid: i64| of(fid).filter(|&k| place(&v, k).1 == 0.0).count();
        assert_eq!([1, 2, 3, 4, 5, 6].map(starts), [1, 1, 1, 2, 2, 2]);
        // Each crossing's point on the antimeridian, 1 km along, heads on east.
        for fid in 1..=3 {
            let seam: Vec<_> = of(fid).map(|k| place(&v, k)).filter(|(p, _, _)| (p.x.abs() - 180.0).abs() < 1e-9).collect();
            assert!(seam.len() == 1 && (seam[0].1 - 1000.0).abs() < 1e-6 && (seam[0].2 - 90.0).abs() < 1e-6, "{fid}: {seam:?}");
        }
    }

    #[test]
    fn points_along_lines_walks_a_closed_line_as_a_ring() {
        // A closed line, such as a contour, gets the points its polygon's ring would: one at the
        // start, made once, its angle halfway between the last edge and the first.
        let c = Coord::new;
        let ring = vec![c(0.0, 0.0), c(0.01, 0.0), c(0.01, 0.01), c(0.0, 0.01), c(0.0, 0.0)];
        // Closed to within a millimeter is closed (here 1e-12°, as a round trip through text leaves it).
        let mut near = ring.clone();
        near[4] = c(1e-12, 0.0);
        let m = along_inputs(vec![Geometry::Line(vec![near])]);
        let mut polys = Dataset::new("polys", GeometryType::Polygon, vec![Field::new("name", FieldType::Text)]);
        polys.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![Value::Text("f0".into())]);
        let mut m = m;
        m.0.insert("polys".to_string(), polys);
        for p in [
            serde_json::json!({"distance": 1000, "units": "Meters"}),
            serde_json::json!({"distance": 500, "units": "Meters", "includeEnd": true}),
        ] {
            let mut lp = p.clone();
            lp["input"] = serde_json::json!("lines");
            let mut pp = p;
            pp["input"] = serde_json::json!("polys");
            let (line, poly) = (along(&m, lp), along(&m, pp));
            assert_eq!(line.features.len(), poly.features.len());
            for k in 0..line.features.len() {
                let ((lp, ld, la), (pp, pd, pa)) = (place(&line, k), place(&poly, k));
                // The same to the line's 1e-12° of difference.
                let same = (lp.x - pp.x).abs() < 1e-9 && (lp.y - pp.y).abs() < 1e-9 && (ld - pd).abs() < 1e-6 && (la - pa).abs() < 1e-6;
                assert!(same, "{k}: {:?} {:?}", place(&line, k), place(&poly, k));
            }
        }
    }

    #[test]
    fn points_along_lines_places_points_off_the_equator() {
        // From (10°, 40°) to (20°, 50°), every 200 km: GeographicLib's own (Python geographiclib 2.1)
        // places and azimuths along that geodesic.
        let c = Coord::new;
        let m = along_inputs(vec![Geometry::Line(vec![vec![c(10.0, 40.0), c(20.0, 50.0)]])]);
        let v = along(&m, serde_json::json!({"input": "lines", "distance": 200}));
        let want = [
            (c(11.266241844369983, 41.52210323065901), 200.0, 32.73068788823587),
            (c(12.592953843951635, 43.02919798608608), 400.0, 33.6232638576897),
            (c(16.99576799827547, 47.444160555622474), 1000.0, 36.75244896333238),
        ];
        assert_eq!(v.features.len(), 7);
        for (k, (wp, wd, wa)) in [1, 2, 5].into_iter().zip(want) {
            let (p, d, a) = place(&v, k);
            assert!(
                (p.x - wp.x).abs() < 1e-9 && (p.y - wp.y).abs() < 1e-9 && (d - wd).abs() < 1e-9 && (a - wa).abs() < 1e-9,
                "{k}: {p:?} {d} {a}"
            );
        }
    }

    #[test]
    fn points_along_lines_never_counts_fewer_points_than_it_makes() {
        // The list is sized from a bound on each part's length, up to 0.5 % over: a longer part never
        // gets fewer points, whatever the step, offsets, end point or ring. Swept over lengths from a
        // meter to 30,000 km, steps from a millionth of the part to three times it, and offsets
        // around the tolerance, where a ring's start and end once counted differently.
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut unit = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        for _ in 0..200_000 {
            let len = 10f64.powf(unit() * 7.5);
            let step = len * 10f64.powf(unit() * 6.5 - 6.0);
            let mut offset = || match (unit() * 4.0) as u32 {
                0 => 0.0,
                1 => unit() * 2e-3,
                2 => unit() * len * 2e-9,
                _ => unit() * step,
            };
            let spacing = Spacing { step, start: offset(), end: offset(), with_end: unit() < 0.5 };
            let ring = unit() < 0.5;
            let (made, counted) = (spacing.places(len, ring).n(), spacing.most(len * (1.0 + unit() * 0.005), ring));
            assert!(counted >= made, "{spacing:?} {len} {ring}: {counted} < {made}");
        }
        // The case found: a ring of 1,010.036 km, a step of 4,015 km, a start offset of 1.012 mm.
        let s = Spacing { step: 4_015_000.0, start: 1.012e-3, end: 0.0, with_end: true };
        assert_eq!((s.places(1_010_036.0, true).n(), s.most(1_010_036.0 * 1.005, true)), (2, 2));
        // A step of 2 mm with a start offset of 0.8 mm: the ring's last point, 0.2 mm before its end,
        // is a millimeter from its first, and is kept.
        let s = Spacing { step: 0.002, start: 0.0008, end: 0.0, with_end: false };
        assert_eq!((s.places(1.001, true).regular, s.most(1.001, true)), (501, 501));
    }

    #[test]
    fn points_along_lines_puts_one_point_on_a_vertex() {
        // The last point 0.9 mm before a vertex and the end 0.2 mm after it, both within the
        // tolerance of it: one point there, not two.
        let c = Coord::new;
        let m =
            along_inputs(vec![Geometry::Line(vec![vec![c(0.0, 0.0), c(equator_degrees(1000.0), 0.0), c(equator_degrees(2000.0), 0.0)]])]);
        let p = serde_json::json!({"input": "lines", "distance": 999.9991, "endOffset": 999.9998, "units": "Meters", "includeEnd": true});
        let v = along(&m, p);
        assert_eq!(v.features.len(), 2);
        assert!((place(&v, 1).0.x - equator_degrees(1000.0)).abs() < 1e-12, "{:?}", place(&v, 1));
    }

    #[test]
    fn points_along_lines_never_piles_points_on_a_vertex() {
        // A step under the tolerance: every half millimeter along 10 m, each point once, in order.
        let c = Coord::new;
        let m = along_inputs(vec![Geometry::Line(vec![vec![c(0.0, 0.0), c(equator_degrees(5.0), 0.0), c(equator_degrees(10.0), 0.0)]])]);
        let v = along(&m, serde_json::json!({"input": "lines", "distance": 0.0005, "units": "Meters"}));
        assert_eq!(v.features.len(), 20_001);
        for k in 1..v.features.len() {
            let ((a, da, _), (b, db, _)) = (place(&v, k - 1), place(&v, k));
            assert!(db > da && b.x > a.x, "{k}: {a:?} {da} then {b:?} {db}");
        }
    }

    #[test]
    fn point_list_refuses_more_points_than_it_counted() {
        let mut list = PointList::new(1).unwrap();
        let c = Coord::new(0.0, 0.0);
        assert!(list.push(c, &[], 0, [Value::Int(1)]).is_continue());
        assert!(list.push(c, &[], 0, [Value::Int(2)]).is_break());
        assert!(list.stopped());
        let e = list.finish().unwrap_err().message("Tool", 1);
        assert_eq!(e, "Tool: made more than the 1 points it counted, so stopped rather than leave some out (please report this)");
        // Counted 100, made 10: the layer gets a list of 10, not the room for 100.
        let mut list = PointList::new(100).unwrap();
        for k in 0..10 {
            assert!(list.push(c, &[], 0, [Value::Int(k)]).is_continue());
        }
        let points = list.finish().unwrap();
        assert!(points.len() == 10 && points.capacity() < 16, "{}", points.capacity());
    }

    #[test]
    fn points_along_lines_takes_geographiclibs_path_between_antipodes() {
        // Antipodes on the equator have no single shortest path: GeographicLib's runs north along
        // the prime meridian, over the pole and down the antimeridian, 20,003.931 km. The places
        // and directions are GeographicLib's own (Python geographiclib 2.1).
        let c = Coord::new;
        let m = along_inputs(vec![Geometry::Line(vec![vec![c(0.0, 0.0), c(180.0, 0.0)]])]);
        let v = along(&m, serde_json::json!({"input": "lines", "distance": 5000}));
        assert_eq!(v.features.len(), 5);
        let ((north, d1, up), (south, d3, down)) = (place(&v, 1), place(&v, 3));
        assert!(north.x.abs() < 1e-9 && (north.y - 45.135_473_786_527_47).abs() < 1e-9 && up.min(360.0 - up) < 1e-9, "{north:?} {up}");
        assert!((south.x.abs() - 180.0).abs() < 1e-9 && (south.y - 45.170_849_381_446_15).abs() < 1e-9, "{south:?}");
        assert!((down - 180.0).abs() < 1e-9 && (d1, d3) == (5000.0, 15000.0), "{down} {d1} {d3}");
    }

    #[test]
    fn points_along_lines_passes_over_pole_and_seam_edges_of_no_length() {
        // A polar cap as plate carrée files cut it: down the antimeridian to the pole, along the
        // pole (one place), up again, and along 85°S between -180 and 180 (one place). The two edges
        // along the antimeridian are 558.456 km each (GeographicLib's own value). Edges being the
        // shortest paths between vertices, the 85°S "edge" (3,504 km in the file's flat sense) has
        // no length: as the docs say, such a cap gets points only along its cut, without a panic
        // or a point twice.
        let c = Coord::new;
        let ring = vec![c(180.0, -85.0), c(180.0, -90.0), c(-180.0, -90.0), c(-180.0, -85.0)];
        let mut cap = Dataset::new("cap", GeometryType::Polygon, vec![]);
        cap.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![]);
        let m = Map(HashMap::from([("cap".to_string(), cap)]));
        let v = along(&m, serde_json::json!({"input": "cap", "distance": 100, "includeEnd": true}));
        // 1,116.911 km around: 0 to 1,100 km, then the end, which is the start, made once.
        assert_eq!(v.features.len(), 12);
        for k in 0..12 {
            let (p, d, angle) = place(&v, k);
            // On the antimeridian, heading south to the pole (558.456 km) and north after it.
            let heading = if d < 558.456 { 180.0 } else { 0.0 };
            assert!((p.x.abs() - 180.0).abs() < 1e-9 && (angle - heading).abs().min(360.0 - angle) < 1e-6, "{k}: {p:?} {d} {angle}");
        }
        assert!((place(&v, 11).1 - 1100.0).abs() < 1e-9);
    }

    #[test]
    fn points_along_lines_takes_include_end_as_true_or_false() {
        let c = Coord::new;
        let m = along_inputs(vec![Geometry::Line(vec![vec![c(0.0, 0.0), c(0.1, 0.0)]])]);
        assert_eq!(along(&m, serde_json::json!({"input": "lines", "includeEnd": " TRUE "})).features.len(), 13);
        assert_eq!(along(&m, serde_json::json!({"input": "lines", "includeEnd": "false"})).features.len(), 12);
        assert_eq!(along(&m, serde_json::json!({"input": "lines", "includeEnd": ""})).features.len(), 12);
        assert_eq!(along(&m, serde_json::json!({"input": "lines", "includeEnd": null})).features.len(), 12);
        // Never a guess at what another value meant; a long one is quoted only in part.
        let refuse = |v: &serde_json::Value| {
            crate::run("pointsAlongLines", &m, &serde_json::json!({"input": "lines", "includeEnd": v})).map(|_| ()).unwrap_err()
        };
        for v in [serde_json::json!("yes"), serde_json::json!(1), serde_json::json!([true])] {
            assert_eq!(refuse(&v), format!("Points Along Lines: 'Include End Point' must be true or false (not {v})"));
        }
        let long = serde_json::json!("y".repeat(10_000));
        assert_eq!(refuse(&long), format!("Points Along Lines: 'Include End Point' must be true or false (not \"{}…)", "y".repeat(79)));
        // Null is the default for every built-in on-off parameter: Merge's is on.
        let merge = crate::tool("merge").unwrap();
        let p = crate::resolve_params(merge, &serde_json::json!({"inputs": ["a"], "attributes": null})).unwrap();
        assert_eq!(p["attributes"], serde_json::json!(true));
    }

    #[test]
    fn length_bound_is_never_short_of_the_ellipsoid() {
        let c = Coord::new;
        // Along the equator and the meridians, near a pole (where the ellipsoid is longest against
        // the sphere), across the antimeridian and over the pole, nearly to the antipode, and round a ring.
        let paths: [(&[Coord], bool); 8] = [
            (&[c(0.0, 0.0), c(1.0, 0.0)], false),
            (&[c(0.0, -0.5), c(0.0, 0.5)], false),
            (&[c(10.0, 89.0), c(10.0, 89.9), c(100.0, 89.9)], false),
            (&[c(179.9, 10.0), c(-179.9, 10.5)], false),
            (&[c(0.0, 89.0), c(180.0, 89.0)], false),
            (&[c(0.0, 0.0), c(179.5, 0.5)], false),
            (&[c(-70.0, -33.0), c(151.0, -34.0), c(37.6, 55.7)], false),
            (&[c(0.0, 0.0), c(0.01, 0.0), c(0.01, 0.01), c(0.0, 0.01)], true),
        ];
        for (path, ring) in paths {
            let line = [path.to_vec()];
            let exact: f64 = segments(&line, ring).map(|s| s.len).sum();
            let bound = length_bound(&line, ring);
            // At least the length, and not loose by more than the sphere and ellipsoid differ (1.1 %).
            assert!(exact <= bound && bound <= exact * 1.011, "{path:?}: {exact} {bound}");
        }
        // A line split at the antimeridian: its two ends there are one place, adding nothing.
        let split = [vec![c(179.9, 10.0), c(180.0, 10.25)], vec![c(-180.0, 10.25), c(-179.9, 10.5)]];
        let whole = [vec![c(179.9, 10.0), c(-179.9, 10.5)]];
        for line in [&split[..], &whole[..]] {
            let (exact, bound): (f64, f64) = (segments(line, false).map(|s| s.len).sum(), length_bound(line, false));
            assert!(exact <= bound && bound <= exact * 1.011, "{line:?}: {exact} {bound}");
        }
    }

    #[test]
    fn points_along_lines_refuses_what_it_cannot_do() {
        let c = Coord::new;
        let m = along_inputs(vec![Geometry::Line(vec![vec![c(0.0, 0.0), c(0.1, 0.0)]])]);
        let run = |p: serde_json::Value| crate::run("pointsAlongLines", &m, &p).map(|_| ()).unwrap_err();
        assert_eq!(run(serde_json::json!({"input": "pts"})), "Points Along Lines needs a line or polygon layer");
        for distance in [serde_json::json!(0), serde_json::json!(-1), serde_json::json!("x"), serde_json::json!(1e308)] {
            assert_eq!(
                run(serde_json::json!({"input": "lines", "distance": distance, "units": "Miles"})),
                "Points Along Lines: Distance must be a number above 0, and not too large"
            );
        }
        let offset = |label: &str| format!("Points Along Lines: {label} must be a number, 0 or more, and not too large");
        assert_eq!(run(serde_json::json!({"input": "lines", "startOffset": -1})), offset("Start Offset"));
        assert_eq!(run(serde_json::json!({"input": "lines", "endOffset": "far"})), offset("End Offset"));
        assert!(run(serde_json::json!({"input": "lines", "units": "Parsecs"})).contains("must be one of"));
        // Blank offsets are none.
        let v = along(&m, serde_json::json!({"input": "lines", "startOffset": "", "endOffset": null}));
        assert_eq!(v.features.len(), 12);
        // Far more points than the memory budget fails from the count, before anything is
        // allocated, on every platform (an exact count stopping as soon as it's over too).
        let e = run(serde_json::json!({"input": "lines", "distance": 1e-15, "units": "Meters"}));
        let refused = "more than the 4 GiB it may use: use a larger distance, or select fewer features";
        let start = "Points Along Lines: up to ";
        assert!(e.starts_with(start) && e.contains(" points would need more memory than it can count, ") && e.ends_with(refused), "{e}");
        // 0.2 mm apart on 11.13 km: some 56 million points, also over the budget.
        let e = run(serde_json::json!({"input": "lines", "distance": 0.0002, "units": "Meters"}));
        assert!(e.ends_with(refused), "{e}");
    }

    #[test]
    fn spacing_counts_each_point_once() {
        let s = |start: f64, end: f64, with_end: bool| Spacing { step: 100.0, start, end, with_end };
        let count = |s: Spacing, len: f64, ring: bool| {
            let p = s.places(len, ring);
            (p.regular, p.with_end)
        };
        // An open part 300 m long: 0, 100, 200 and 300; asking for the end adds nothing.
        assert_eq!(count(s(0.0, 0.0, false), 300.0, false), (4, false));
        assert_eq!(count(s(0.0, 0.0, true), 300.0, false), (4, false));
        // A ring 300 m around: its end is its start, made once, even when asked for.
        assert_eq!(count(s(0.0, 0.0, true), 300.0, true), (3, false));
        // 350 m: the end at 350 is one more; on a ring too, once an offset moves it off the start.
        assert_eq!(count(s(0.0, 0.0, true), 350.0, false), (4, true));
        assert_eq!(count(s(0.0, 10.0, true), 350.0, true), (4, true));
        // Offsets of 50 m: 50, 150 and 250, where they stop.
        assert_eq!(count(s(50.0, 50.0, true), 300.0, false), (3, false));
        // Rounding in a length that is a multiple of the step neither drops a point nor makes one twice.
        assert_eq!(count(s(0.0, 0.0, false), 300.000_000_000_1, false), (4, false));
        assert_eq!(count(s(0.0, 0.0, false), 299.999_999_999_9, false), (4, false));
        // No length, offsets longer than the part, and lengths that aren't numbers: none.
        for (len, start) in [(0.0, 0.0), (300.0, 301.0), (f64::NAN, 0.0), (-1.0, 0.0)] {
            assert_eq!(count(s(start, 0.0, true), len, false), (0, false), "{len} {start}");
        }
        // A step far too small saturates the count rather than wrapping it.
        let tiny = Spacing { step: 1e-300, start: 0.0, end: 0.0, with_end: true };
        assert_eq!(tiny.places(1e10, false).regular, usize::MAX);
        assert_eq!((mean_azimuth(170.0, -170.0), bearing(-1e-17), bearing(-90.0)), (180.0, 0.0, 270.0));
        // Due north as -0 is 0, not "-0".
        assert!(bearing(-0.0).is_sign_positive());
    }

    #[test]
    fn feet_buffer_has_the_requested_ground_radius() {
        let mut pts = Dataset::new("p", GeometryType::Point, vec![]);
        let center = Coord::new(-105.9378, 35.687);
        pts.push(Some(Geometry::Point(center)), vec![]);
        let m = Map(HashMap::from([("p".to_string(), pts)]));
        let r = crate::run("buffer", &m, &serde_json::json!({"input": "p", "distance": 500, "units": "feet"})).unwrap();
        assert_eq!(r.messages, vec!["Buffer: 1 features at 500 Feet"]);
        let ds = layer(r);
        let Geometry::Polygon(polys) = ds.features[0].geometry.as_ref().unwrap() else { panic!("expected buffer polygon") };
        // 500 international feet is exactly 152.4 metres, at every vertex.
        for vertex in polys.iter().flatten().flatten() {
            let radius = on_wgs84_m(center, *vertex);
            assert!((radius - 152.4).abs() < 1e-6, "radius: {radius}");
        }
    }

    // Reference values from PROJ / pyproj 3.7.2 Geod(ellps="WGS84"), in km² and km.
    #[test]
    fn calculate_geometry_uses_wgs84_for_polygons() {
        let rectangle = |x: f64, y: f64, w: f64, h: f64| {
            vec![Coord::new(x, y), Coord::new(x + w, y), Coord::new(x + w, y + h), Coord::new(x, y + h), Coord::new(x, y)]
        };
        for (lat, area, perimeter) in [
            (0.0, 132.93794889680612, 68.77959742996498),
            (46.5, 92.07715389612198, 58.58311003354862),
            (60.0, 67.04587460470582, 52.371257858418176),
        ] {
            let parts = vec![
                vec![rectangle(10.5, lat, 0.1, 0.1), rectangle(10.52, lat + 0.02, 0.02, 0.02)],
                vec![rectangle(11.0, lat, 0.03, 0.04)],
            ];
            for reverse in [false, true] {
                let mut parts = parts.clone();
                if reverse {
                    for ring in parts.iter_mut().flatten() {
                        ring.reverse();
                    }
                }
                let mut ds = Dataset::new("zones", GeometryType::Polygon, vec![Field::new("code", FieldType::Text)]);
                ds.push(Some(Geometry::Polygon(parts)), vec![Value::Text("Z1".into())]);
                let inputs = Map(HashMap::from([("zones".into(), ds.clone())]));
                let out = layer(crate::run("calculateGeometry", &inputs, &serde_json::json!({"input":"zones"})).unwrap());
                assert_eq!(out.features[0].geometry, ds.features[0].geometry);
                assert_eq!(out.value(0, 0), &Value::Text("Z1".into()));
                let measured_area = out.value(0, out.field_index("area_km2").unwrap()).as_f64().unwrap();
                let measured_perimeter = out.value(0, out.field_index("perimeter_km").unwrap()).as_f64().unwrap();
                assert!((measured_area - area).abs() < 1e-6, "{lat}, reversed={reverse}: {measured_area} vs {area}");
                assert!((measured_perimeter - perimeter).abs() < 1e-6, "{lat}: {measured_perimeter} vs {perimeter}");
                assert_eq!(inputs.0["zones"], ds);
            }
        }
        let ring = vec![
            Coord::new(179.9, 10.0),
            Coord::new(-179.9, 10.0),
            Coord::new(-179.9, 10.1),
            Coord::new(179.9, 10.1),
            Coord::new(179.9, 10.0),
        ];
        let mut ds = Dataset::new("island", GeometryType::Polygon, vec![]);
        ds.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![]);
        let inputs = Map(HashMap::from([("island".into(), ds)]));
        let out = layer(crate::run("calculateGeometry", &inputs, &serde_json::json!({"input":"island"})).unwrap());
        assert!((out.value(0, out.field_index("area_km2").unwrap()).as_f64().unwrap() - 242.50305444813918).abs() < 1e-6);
        assert!((out.value(0, out.field_index("perimeter_km").unwrap()).as_f64().unwrap() - 65.97062684618793).abs() < 1e-6);
    }

    /// A cap above 80° N cut at ±180° and closed along the pole (#1353): a 360° edge along 80° N was
    /// unwrapped to no edge, so the ring measured 0 km². The WGS 84 zone is 3,908,572.76 km².
    #[test]
    fn calculate_geometry_measures_a_cap_closed_along_the_pole() {
        let box_cap = vec![
            Coord::new(-180.0, 80.0),
            Coord::new(180.0, 80.0),
            Coord::new(180.0, 90.0),
            Coord::new(-180.0, 90.0),
            Coord::new(-180.0, 80.0),
        ];
        let mut dense: Vec<Coord> = (-180..=180).map(|x| Coord::new(f64::from(x), 80.0)).collect();
        dense.extend([Coord::new(180.0, 90.0), Coord::new(-180.0, 90.0), Coord::new(-180.0, 80.0)]);
        for ring in [box_cap, dense] {
            for reverse in [false, true] {
                let mut ring = ring.clone();
                if reverse {
                    ring.reverse();
                }
                let mut ds = Dataset::new("cap", GeometryType::Polygon, vec![]);
                ds.push(Some(Geometry::Polygon(vec![vec![ring]])), vec![]);
                let inputs = Map(HashMap::from([("cap".into(), ds)]));
                let out = layer(crate::run("calculateGeometry", &inputs, &serde_json::json!({"input":"cap"})).unwrap());
                let area = out.value(0, out.field_index("area_km2").unwrap()).as_f64().unwrap();
                assert!((area - 3_908_572.76).abs() < 1.0, "reversed={reverse}: {area}");
            }
        }
    }

    #[test]
    fn calculate_geometry_keeps_large_regions_and_reversed_small_rings() {
        let ring = |points: &[(f64, f64)]| points.iter().map(|&(x, y)| Coord::new(x, y)).collect::<Vec<_>>();
        let band = ring(&[
            (-170.0, -60.0),
            (-85.0, -60.0),
            (0.0, -60.0),
            (85.0, -60.0),
            (170.0, -60.0),
            (170.0, 60.0),
            (85.0, 60.0),
            (0.0, 60.0),
            (-85.0, 60.0),
            (-170.0, 60.0),
            (-170.0, -60.0),
        ]);
        let hole = ring(&[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (0.0, 0.0)]);
        // GeographicLib 2.1 WGS84 Polygon.Compute: unsigned for the band, signed absolute for caps.
        let cases = [
            ("large", vec![band, hole], 435312377.1122553 - 12308.778361469453, 61868.48818267019 + 443.77091724830194),
            (
                "north",
                vec![ring(&[(-135.0, 80.0), (-45.0, 80.0), (45.0, 80.0), (135.0, 80.0), (-135.0, 80.0)])],
                2507270.031169875,
                6301.599963614222,
            ),
            (
                "south",
                vec![ring(&[(-135.0, -80.0), (-45.0, -80.0), (45.0, -80.0), (135.0, -80.0), (-135.0, -80.0)])],
                2507270.031169875,
                6301.599963614222,
            ),
            (
                "dateline",
                vec![ring(&[(179.9, 10.0), (-179.9, 10.0), (-179.9, 10.1), (179.9, 10.1), (179.9, 10.0)])],
                242.50305444813918,
                65.97062684618793,
            ),
        ];
        for (name, rings, area, perimeter) in cases {
            for reverse in [false, true] {
                let mut rings = rings.clone();
                if reverse {
                    for ring in &mut rings {
                        ring.reverse();
                    }
                }
                let geometry = Geometry::Polygon(vec![rings]);
                let mut ds = Dataset::new(name, GeometryType::Polygon, vec![]);
                ds.push(Some(geometry.clone()), vec![]);
                let inputs = Map(HashMap::from([(name.into(), ds)]));
                let out = layer(crate::run("calculateGeometry", &inputs, &serde_json::json!({"input":name})).unwrap());
                let measured = out.value(0, out.field_index("area_km2").unwrap()).as_f64().unwrap();
                let per = out.value(0, out.field_index("perimeter_km").unwrap()).as_f64().unwrap();
                assert!((measured - area).abs() < 1e-6, "{name}, reversed={reverse}: {measured} vs {area}");
                assert!((per - perimeter).abs() < 1e-6, "{name}, reversed={reverse}: {per} vs {perimeter}");
                assert_eq!(out.features[0].geometry.as_ref(), Some(&geometry));
            }
        }
    }

    #[test]
    fn calculate_geometry_does_not_orient_invalid_coordinates() {
        for bad in [Coord::new(f64::NAN, 0.0), Coord::new(f64::INFINITY, 0.0), Coord::new(0.0, f64::NAN), Coord::new(0.0, 100.0)] {
            let mut ds = Dataset::new("invalid", GeometryType::Polygon, vec![]);
            ds.push(
                Some(Geometry::Polygon(vec![vec![vec![
                    Coord::new(0.0, 0.0),
                    Coord::new(1.0, 0.0),
                    bad,
                    Coord::new(0.0, 1.0),
                    Coord::new(0.0, 0.0),
                ]]])),
                vec![],
            );
            let inputs = Map(HashMap::from([("invalid".into(), ds)]));
            assert!(crate::run("calculateGeometry", &inputs, &serde_json::json!({"input":"invalid"})).is_ok());
        }
    }

    #[test]
    fn calculate_geometry_uses_wgs84_for_lines() {
        let mut ds = Dataset::new("routes", GeometryType::Polyline, vec![]);
        let parts = vec![
            vec![Coord::new(0.0, 0.0), Coord::new(1.0, 0.0)],
            vec![Coord::new(10.0, 60.0), Coord::new(10.0, 61.0)],
            vec![Coord::new(179.9, 10.0), Coord::new(-179.9, 10.0)],
        ];
        ds.push(Some(Geometry::Line(parts)), vec![]);
        ds.push(None, vec![]);
        let inputs = Map(HashMap::from([("routes".into(), ds.clone())]));
        let out = layer(crate::run("calculateGeometry", &inputs, &serde_json::json!({"input":"routes"})).unwrap());
        let length = out.field_index("length_km").unwrap();
        let expected = 111.31949079327356 + 111.42072786973769 + 21.92787247793737;
        assert!((out.value(0, length).as_f64().unwrap() - expected).abs() < 1e-6);
        assert_eq!(out.value(1, length).as_f64(), Some(0.0));
        assert_eq!(out.features[0].geometry, ds.features[0].geometry);
    }

    /// Calculate Geometry stores full-precision values (#513): rounding to 0.01 km² gave every
    /// polygon under half a hectare an area of 0, and lines under 5 m a length of 0.
    #[test]
    fn calculate_geometry_keeps_small_values() {
        // A square of about 50 m (0.00045°) at the equator, and one of its sides as a line.
        let s = 0.00045;
        let mut polys = Dataset::new("p", GeometryType::Polygon, vec![]);
        polys.push(Some(sq(0.0, 0.0, s)), vec![]);
        let mut lines = Dataset::new("l", GeometryType::Polyline, vec![]);
        lines.push(Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(0.00002, 0.0)]])), vec![]);
        let m = Map(HashMap::from([("p".to_string(), polys), ("l".to_string(), lines)]));
        let p = layer(crate::run("calculateGeometry", &m, &serde_json::json!({"input": "p"})).unwrap());
        let area = p.value(0, p.field_index("area_km2").unwrap()).as_f64().unwrap();
        let per = p.value(0, p.field_index("perimeter_km").unwrap()).as_f64().unwrap();
        assert!((area - 0.0025).abs() < 0.0001, "≈ 50 m × 50 m: {area}");
        assert!((per - 0.2).abs() < 0.001 && per != 0.2, "≈ 200 m, unrounded: {per}");
        let l = layer(crate::run("calculateGeometry", &m, &serde_json::json!({"input": "l"})).unwrap());
        let len = l.value(0, l.field_index("length_km").unwrap()).as_f64().unwrap();
        assert!((len - 0.00222).abs() < 0.00001, "≈ 2.2 m: {len}");
    }

    /// A choice outside the declared ones is an error naming them, never a guess (#406): "meter"
    /// was a buffer in kilometres. A choice in another case is the declared one.
    #[test]
    fn choices_outside_the_list_are_errors() {
        let mut pts = Dataset::new("p", GeometryType::Point, vec![]);
        pts.push(Some(Geometry::Point(Coord::new(-105.9378, 35.687))), vec![]);
        let m = Map(HashMap::from([("p".to_string(), pts)]));
        let buffer =
            |units: serde_json::Value| crate::run("buffer", &m, &serde_json::json!({"input": "p", "distance": 500, "units": units}));
        for bad in ["meter", "Meter", "foot", "m", "km"] {
            let e = buffer(serde_json::json!(bad)).err().unwrap_or_default();
            assert!(e.contains("Kilometers, Meters, Miles") && e.contains(bad), "{bad}: {e}");
        }
        assert!(buffer(serde_json::json!(5)).is_err());
        // "meters" is "Meters": a 500 m radius (about 0.0045° of latitude), named as declared.
        let r = buffer(serde_json::json!("meters")).unwrap();
        assert_eq!(r.messages, vec!["Buffer: 1 features at 500 Meters".to_string()]);
        let e = layer(r).extent();
        assert!((e.max_y - e.min_y - 1000.0 / 111_000.0).abs() < 0.0005, "{e:?}");
        assert!(buffer(serde_json::Value::Null).is_ok(), "no units: the default");
        assert_eq!(layer(buffer(serde_json::json!("  ")).unwrap()).name, "p_Buffer", "blank units: the default");
        // An external tool's choices (GeoLibre's, a plugin's) aren't checked here: its runner does.
        crate::register_external(
            "choicetest",
            || {
                vec![ToolSpec {
                    id: "choicetest:list",
                    name: "Choice Test",
                    toolbox: "Test",
                    summary: "",
                    params: &[P {
                        name: "features",
                        label: "Features",
                        kind: K::Choice(&["contrast", "energy"]),
                        required: false,
                        default: "",
                        help: "",
                    }],
                    run: crate::external_placeholder,
                }]
            },
            |_, _, _| Err("not run".into()),
        );
        let external = crate::external_tools_of("choicetest").first().unwrap();
        let list = crate::resolve_params(external, &serde_json::json!({"features": "contrast,energy"})).unwrap();
        assert_eq!(list["features"], "contrast,energy");
        // A copy of a built-in spec is still checked.
        let b = crate::tool("buffer").unwrap();
        let copy = ToolSpec { id: b.id, name: b.name, toolbox: b.toolbox, summary: b.summary, params: b.params, run: b.run };
        assert!(crate::resolve_params(&copy, &serde_json::json!({"input": "p", "distance": 1, "units": "meter"})).is_err());
        assert!(crate::run("buffer", &m, &serde_json::json!({"input": "p", "distance": 1, "dissolve": "everything"})).is_err());
        // Every built-in choice defaults to one of its own choices.
        for t in TOOLS {
            for p in t.params {
                if let K::Choice(c) = p.kind
                    && let Ok(serde_json::Value::String(d)) = serde_json::from_str::<serde_json::Value>(p.default)
                {
                    assert!(c.contains(&d.as_str()), "{}: {} defaults to {d:?}", t.id, p.name);
                }
            }
        }
    }

    /// `touches_files` in rustgis-engine lets tasks, models and notebooks from files set a built-in
    /// tool's `output` when it names a new layer. Layer outputs remain plain text; the explicit
    /// `vectorToPmtiles` file output is handled separately by the engine.
    #[test]
    fn built_in_tool_outputs_have_explicit_contracts() {
        for spec in TOOLS {
            for p in spec.params.iter().filter(|p| p.name.eq_ignore_ascii_case("output")) {
                let valid = if matches!(spec.id, "vectorToPmtiles" | "rasterToPmtiles") {
                    matches!(p.kind, K::SaveFile { ext: "pmtiles" })
                } else {
                    matches!(p.kind, K::Text)
                };
                assert!(valid, "{}: unexpected output kind {:?}", spec.id, p.kind);
            }
        }
        let m = inputs();
        let r = crate::run("buffer", &m, &serde_json::json!({"input": "pts", "distance": 1, "output": "Named"})).unwrap();
        assert_eq!(layer(r).name, "Named");
        // A best-effort tripwire, not a proof: the crate's code (every file under src/, up to its
        // test module) names nothing that reaches the file system, a path type included (a tool
        // here takes layers, not paths). It can't see a write through another crate's helper;
        // `no_built_in_tool_writes_its_output_as_a_file` runs the tools for that, and review
        // still has to keep tools to it.
        // (Read where the sources are: not in a wasm test runner.)
        let mut dirs = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
        let mut scanned = 0;
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap().replace("\r\n", "\n");
                    // The tests (this one reads files) are the `mod tests` closing each file: the
                    // first `#[cfg(test)]` must open it, so no test-only item hides the code after
                    // it. (Code after the test module would go unseen; the crate keeps none, and
                    // the runtime test below runs the tools whatever their source.)
                    let code = match text.find("#[cfg(test)]") {
                        None => text.as_str(),
                        Some(at) => {
                            let (code, tests) = text.split_at(at);
                            assert!(
                                tests.starts_with("#[cfg(test)]\nmod tests {"),
                                "{}: a `#[cfg(test)]` before its `mod tests`",
                                path.display()
                            );
                            code
                        }
                    };
                    // The standard library's files and paths, and rustgis-data's writers that take a path
                    // (its byte writers, such as GeoTIFF's, return bytes and are fine).
                    let reaches =
                        ["fs::", "std::fs", "File::", "io::Write", "OpenOptions", "std::path", "write_path", "write_atomic", "gpkg::write"];
                    // Code only, not comment lines. A path-like name counts only where a name starts
                    // (`fs::`, not `bufs::`); a writer's name counts anywhere (`do_write_atomic(`).
                    let code: String = code.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
                    let names = |reach: &str| {
                        let anywhere = reach.starts_with("write_") || reach.starts_with("gpkg::");
                        code.match_indices(reach)
                            .any(|(i, _)| anywhere || !code[..i].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_'))
                    };
                    for reach in reaches {
                        assert!(!names(reach), "{}: names {reach}, so a tool's output could become a place", path.display());
                    }
                    scanned += 1;
                }
            }
        }
        assert!(cfg!(target_arch = "wasm32") || scanned > 5, "the sources were read");
    }

    /// The runtime side of the guard above: layer-producing tools, run with `output` set to a path
    /// in an empty folder (other parameters from their defaults, layers from `inputs`), leave
    /// that folder empty. File outputs are intentionally excluded.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn no_layer_tool_writes_its_output_as_a_file() {
        let dir = std::env::temp_dir().join(format!("rustgis-tool-output-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let m = inputs();
        let mut tested = 0;
        let mut ran = 0;
        for spec in TOOLS {
            if !spec.params.iter().any(|p| p.name.eq_ignore_ascii_case("output") && matches!(p.kind, K::Text)) {
                continue;
            }
            tested += 1;
            let mut p = serde_json::Map::new();
            for param in spec.params {
                let value = match serde_json::from_str::<serde_json::Value>(param.default) {
                    Ok(v) => v,
                    Err(_) => match param.kind {
                        K::Number => serde_json::json!(1),
                        K::Bool => serde_json::json!(false),
                        K::Text => serde_json::json!("a"),
                        K::Choice(c) => serde_json::json!(c.first().copied().unwrap_or_default()),
                        K::Field | K::NumericField | K::CoordinateField => serde_json::json!("v"),
                        K::PolygonLayer => serde_json::json!("polys"),
                        K::Path => continue,
                        _ => serde_json::json!("pts"),
                    },
                };
                p.insert(param.name.into(), value);
            }
            let out = dir.join(format!("{}.geojson", spec.id));
            p.insert("output".into(), serde_json::json!(out.display().to_string()));
            ran += usize::from(crate::run(spec.id, &m, &serde_json::Value::Object(p)).is_ok());
            let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
            if !left.is_empty() {
                // (The folder goes before the test fails, not left behind.)
                let _ = std::fs::remove_dir_all(&dir);
                panic!("{} wrote {left:?} where its output was named", spec.id);
            }
        }
        let _ = std::fs::remove_dir(&dir);
        assert!(ran * 2 >= tested, "only {ran} of {tested} layer tools ran on these inputs");
    }

    /// Complexity is per feature for ordinary buffers, cumulative before dissolve. Multipoint
    /// discs still count before union, including when every disc overlaps.
    #[test]
    fn buffer_vertex_budget_covers_features_and_multipoint_discs() {
        let point = Coord::new(0.0, 0.0);
        let vertices = geodesic_buffer(&Geometry::Point(point), 1000.0).unwrap().coord_count();
        let mut ds = Dataset::new("p", GeometryType::Point, vec![]);
        for c in [point, Coord::new(0.5, 0.0)] {
            ds.push(Some(Geometry::Point(c)), vec![]);
        }
        let mut m = Map(HashMap::from([("p".to_string(), ds)]));
        for dissolve in ["None", "All"] {
            let params = serde_json::json!({"input": "p", "distance": 1, "dissolve": dissolve});
            assert!(buffer_with_limits(&m, &params, 2 * vertices, MAX_BUFFER_BYTES).is_ok());
            let limited = buffer_with_limits(&m, &params, vertices, MAX_BUFFER_BYTES);
            if dissolve == "All" {
                assert!(limited.unwrap_err().contains("vertex budget"));
            } else {
                assert!(limited.is_ok());
            }
        }
        // Crossing thin rectangles add intersection vertices during dissolve even though no
        // original vertex lies inside the other rectangle. The inputs fit; the union does not.
        let rectangle = |x: f64, y: f64| {
            Geometry::Polygon(vec![vec![vec![
                Coord::new(-x, -y),
                Coord::new(x, -y),
                Coord::new(x, y),
                Coord::new(-x, y),
                Coord::new(-x, -y),
            ]]])
        };
        let mut cross = Dataset::new("cross", GeometryType::Polygon, vec![]);
        for g in [rectangle(0.02, 0.001), rectangle(0.001, 0.02)] {
            cross.push(Some(g), vec![]);
        }
        let outline_vertices = cross
            .features
            .iter()
            .filter_map(|f| f.geometry.as_ref())
            .filter_map(|g| geodesic_buffer(g, 0.0))
            .map(|g| g.coord_count())
            .sum();
        let cross_inputs = Map(HashMap::from([("cross".into(), cross)]));
        let mut params = serde_json::json!({"input":"cross", "distance":0, "dissolve":"None"});
        assert!(buffer_with_limits(&cross_inputs, &params, outline_vertices, MAX_BUFFER_BYTES).is_ok());
        params["dissolve"] = serde_json::json!("All");
        let union = layer(buffer_with_limits(&cross_inputs, &params, usize::MAX, MAX_BUFFER_BYTES).unwrap());
        assert!(union.features[0].geometry.as_ref().unwrap().coord_count() > outline_vertices);
        assert!(buffer_with_limits(&cross_inputs, &params, outline_vertices, MAX_BUFFER_BYTES).unwrap_err().contains("vertex budget"));
        // All discs overlap: the final union fits, but collecting the discs must still stop.
        m.0.get_mut("p").unwrap().features.clear();
        m.0.get_mut("p").unwrap().push(Some(Geometry::MultiPoint(vec![point; 3])), vec![]);
        for dissolve in ["None", "All"] {
            let params = serde_json::json!({"input": "p", "distance": 1, "dissolve": dissolve});
            assert!(buffer_with_limits(&m, &params, 2 * vertices, MAX_BUFFER_BYTES).unwrap_err().contains("vertex budget"));
        }
        // Each feature's discs fit alone, but their combined pre-union charge does not.
        m.0.get_mut("p").unwrap().features.clear();
        for _ in 0..2 {
            m.0.get_mut("p").unwrap().push(Some(Geometry::MultiPoint(vec![point; 2])), vec![]);
        }
        for dissolve in ["None", "All"] {
            let params = serde_json::json!({"input":"p", "distance":1, "dissolve":dissolve});
            assert!(buffer_with_limits(&m, &params, 4 * vertices, MAX_BUFFER_BYTES).is_ok());
            let limited = buffer_with_limits(&m, &params, 3 * vertices, MAX_BUFFER_BYTES);
            if dissolve == "All" {
                assert!(limited.unwrap_err().contains("vertex budget"));
            } else {
                assert!(limited.is_ok());
            }
        }
    }

    /// More than two result batches fit a per-feature limit that cannot hold two circles
    /// together. Rows retain their order, ids, attributes and exact existing geometries.
    #[test]
    fn buffer_storage_replaces_the_cross_feature_vertex_ceiling() {
        let point = Geometry::Point(Coord::new(8.0, 46.0));
        let expected = geodesic_buffer(&point, 5000.0).unwrap();
        let vertices = expected.coord_count();
        let mut ds = Dataset::new("points", GeometryType::Point, vec![Field::new("i", FieldType::Integer)]);
        for i in 0..(2 * BUFFER_BATCH + 1) {
            ds.push(Some(point.clone()), vec![Value::Int(i as i64)]);
        }
        let m = Map(HashMap::from([("points".into(), ds)]));
        let p = serde_json::json!({"input":"points", "distance":5});
        let output = layer(buffer_with_limits(&m, &p, vertices, MAX_BUFFER_BYTES).unwrap());
        assert_eq!(output.features.len(), 2 * BUFFER_BATCH + 1);
        for (i, f) in output.features.iter().enumerate() {
            assert_eq!(f.id, i as u64 + 1);
            assert_eq!(f.geometry.as_ref(), Some(&expected));
            assert_eq!(f.attrs, vec![Value::Int(i as i64)]);
        }
        assert!(buffer_with_limits(&m, &p, vertices, 0).unwrap_err().contains("memory budget"));
        // No partial output or input edits are returned on failure after earlier batches fit.
        let first_batch_bytes = rustgis_data::source::chunk_memory::feature_bytes(&output.features[0]) * BUFFER_BATCH
            + std::mem::size_of::<Feature>() * BUFFER_BATCH;
        assert!(buffer_with_limits(&m, &p, vertices, first_batch_bytes).unwrap_err().contains("memory budget"));
        assert_eq!(m.0["points"].features.len(), 2 * BUFFER_BATCH + 1);
    }

    /// Storage includes copied text, geometry containers and the retained row array. Exercise
    /// the exact one-row boundary without allocating a large dataset.
    #[test]
    fn buffer_storage_counts_attributes_and_row_capacity() {
        let g = Geometry::Point(Coord::new(8.0, 46.0));
        let mut ds = Dataset::new("points", GeometryType::Point, vec![Field::new("text", FieldType::Text)]);
        ds.push(Some(g.clone()), vec![Value::Text("x".repeat(4096))]);
        let expected = Feature { id: 1, geometry: geodesic_buffer(&g, 5000.0), attrs: ds.features[0].attrs.clone() };
        let bytes = std::mem::size_of::<Feature>() + rustgis_data::source::chunk_memory::feature_bytes(&expected);
        let m = Map(HashMap::from([("points".into(), ds)]));
        let p = serde_json::json!({"input":"points", "distance":5});
        assert_eq!(layer(buffer_with_limits(&m, &p, MAX_BUFFER_VERTICES, bytes).unwrap()).features[0], expected);
        assert!(buffer_with_limits(&m, &p, MAX_BUFFER_VERTICES, bytes - 1).unwrap_err().contains("memory budget"));
    }

    #[test]
    fn buffers_made_on_threads_keep_their_features_and_say_which_had_no_geometry() {
        // Enough features for several threads: each output keeps its own attributes and buffer.
        let at = |i: i64| Coord::new(-150.0 + i as f64, (i % 120) as f64 - 60.0);
        let mut pts = Dataset::new("p", GeometryType::Point, vec![Field::new("i", FieldType::Integer)]);
        for i in 0..300 {
            pts.push(Some(Geometry::Point(at(i))), vec![Value::Int(i)]);
        }
        let clean = Map(HashMap::from([("p".to_string(), pts.clone())]));
        assert_eq!(crate::run("buffer", &clean, &serde_json::json!({"input": "p", "distance": 5})).unwrap().messages.len(), 1);
        // A point with no finite coordinates makes no buffer either, and says so.
        let mut odd = pts.clone();
        odd.push(Some(Geometry::Point(Coord::new(f64::NAN, 0.0))), vec![Value::Int(-2)]);
        let odd = Map(HashMap::from([("p".to_string(), odd)]));
        let r = crate::run("buffer", &odd, &serde_json::json!({"input": "p", "distance": 5})).unwrap();
        assert_eq!(r.messages.len(), 2);
        assert_eq!(
            r.messages.get(1).map(String::as_str),
            Some("1 of 301 features made no buffer at 5 Kilometers (nothing left, or no finite coordinates)")
        );
        pts.push(None, vec![Value::Int(-1)]);
        let m = Map(HashMap::from([("p".to_string(), pts)]));
        let r = crate::run("buffer", &m, &serde_json::json!({"input": "p", "distance": 5})).unwrap();
        assert_eq!(r.messages.len(), 2);
        assert_eq!(r.messages.get(1).map(String::as_str), Some("1 of 301 features lack geometry, so they have no buffer"));
        let b = layer(r);
        assert_eq!(b.features.len(), 300);
        for (k, f) in b.features.iter().enumerate() {
            assert_eq!(f.attrs, vec![Value::Int(k as i64)]);
            assert_eq!(f.geometry, geodesic_buffer(&Geometry::Point(at(k as i64)), 5_000.0), "feature {k}");
        }
        let r = crate::run("buffer", &m, &serde_json::json!({"input": "p", "distance": 5, "dissolve": "All"})).unwrap();
        assert_eq!(r.messages.len(), 2);
        assert_eq!(r.messages.get(1).map(String::as_str), Some("1 of 301 features lack geometry, so they have no buffer"));
        assert_eq!(layer(r).features.len(), 1);
        // Points at a negative distance have nothing left, and say so.
        let r = crate::run("buffer", &clean, &serde_json::json!({"input": "p", "distance": -5})).unwrap();
        assert_eq!(
            r.messages.get(1).map(String::as_str),
            Some("300 of 300 features made no buffer at -5 Kilometers (nothing left, or no finite coordinates)")
        );
        assert_eq!(r.messages.len(), 2);
        assert!(layer(r).features.is_empty());
    }

    #[test]
    fn dissolved_buffers_merge() {
        let mut pts = Dataset::new("p", GeometryType::Point, vec![]);
        pts.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![]);
        pts.push(Some(Geometry::Point(Coord::new(0.5, 0.0))), vec![]);
        let m = Map(HashMap::from([("p".to_string(), pts)]));
        let d = layer(crate::run("buffer", &m, &serde_json::json!({"input": "p", "distance": 50, "dissolve": "All"})).unwrap());
        match d.features[0].geometry.as_ref().unwrap() {
            Geometry::Polygon(polys) => assert_eq!(polys.len(), 1, "overlapping buffers merge into one polygon"),
            _ => panic!(),
        }
    }

    /// The buffer of one feature, run through the tool.
    fn buffered(g: Geometry, km: f64) -> Vec<Vec<Vec<Coord>>> {
        let mut ds = Dataset::new("f", GeometryType::Polygon, vec![]);
        ds.push(Some(g), vec![]);
        let m = Map(HashMap::from([("f".to_string(), ds)]));
        let b = layer(crate::run("buffer", &m, &serde_json::json!({"input": "f", "distance": km})).unwrap_or_else(|e| panic!("{e}")));
        match b.features.first().and_then(|f| f.geometry.clone()) {
            Some(Geometry::Polygon(polys)) => polys,
            other => panic!("expected a polygon: {other:?}"),
        }
    }

    /// The geodesic distance on the WGS 84 ellipsoid (metres), from GeographicLib through `geo`.
    fn on_wgs84_m(a: Coord, b: Coord) -> f64 {
        use geo::{Distance, Geodesic, Point};
        Geodesic.distance(Point::new(a.x, a.y), Point::new(b.x, b.y))
    }

    /// The distance on WGS 84 (km) from `c` to a line, sampled every `step` metres along it.
    /// Only the samples within 1.2% of the nearest on the sphere are measured on the ellipsoid,
    /// whose distances are within 0.6% of the sphere's.
    fn km_to_every(c: Coord, line: &[Coord], step: f64) -> f64 {
        let samples: Vec<(Coord, f64)> = line
            .windows(2)
            .flat_map(|w| {
                let n = (rustgis_geom::haversine_m(w[0], w[1]) / step).ceil().max(1.0) as usize;
                (0..=n).map(move |i| rustgis_geom::geodesic_interpolate(w[0], w[1], i as f64 / n as f64))
            })
            .map(|p| (p, rustgis_geom::haversine_m(c, p)))
            .collect();
        let nearest = samples.iter().map(|(_, d)| *d).fold(f64::INFINITY, f64::min);
        samples.iter().filter(|(_, d)| *d <= nearest * 1.012 + 1.0).map(|(p, _)| on_wgs84_m(c, *p) / 1000.0).fold(f64::INFINITY, f64::min)
    }

    /// The distance on WGS 84 (km) from `c` to a line, sampled every 500 m along it.
    fn km_to(c: Coord, line: &[Coord]) -> f64 {
        km_to_every(c, line, 500.0)
    }

    /// Every vertex of every ring is `km` from `line`, within 0.2%. Where the arcs of two pieces
    /// cross, the vertex sits inside both circles by up to their chord's sag (0.5% with `geo`'s
    /// 0.2 rad steps), as anywhere between a planar buffer's vertices.
    fn all_at(polys: &[Vec<Vec<Coord>>], line: &[Coord], km: f64) {
        // A cap closed along a pole has corners on the pole, which aren't on the buffer's edge.
        for c in polys.iter().flatten().flatten().filter(|c| c.y.abs() < 90.0) {
            let d = km_to(*c, line);
            assert!(d < km * 1.002 && d > km * 0.994, "{c:?} is {d:.2} km away, not {km} km");
        }
    }

    /// Buffers are geodesic on the WGS 84 ellipsoid, not on a sphere (#888): on a sphere of the
    /// mean radius a 10 km buffer reached 9,944 to 10,011 m at the equator and 9,997 to 10,029 m
    /// at 46.5°N, and a 1 km buffer of a 2 km line 999.7 to 1,002.9 m.
    #[test]
    fn buffers_keep_their_distance_on_the_ellipsoid() {
        for centre in [Coord::new(0.0, 0.0), Coord::new(10.5, 46.5), Coord::new(30.0, 60.0), Coord::new(-70.0, -33.4)] {
            for c in buffered(Geometry::Point(centre), 10.0).iter().flatten().flatten() {
                let d = on_wgs84_m(centre, *c);
                assert!((d - 10_000.0).abs() < 1e-3, "{centre:?}: {c:?} is {d} m away");
            }
        }
        let a = Coord::new(10.5, 46.5);
        for azimuth in [90.0, 0.0, 45.0] {
            let line = [a, rustgis_geom::geodesic_destination(a, azimuth, 2_000.0)];
            for c in buffered(Geometry::Line(vec![line.to_vec()]), 1.0).iter().flatten().flatten() {
                // Sampled every 0.5 m along the line, the nearest sample is at most 0.03 mm further.
                let d = km_to_every(*c, &line, 0.5) * 1000.0;
                assert!((d - 1_000.0).abs() < 1e-3, "{azimuth}: {c:?} is {d} m away");
            }
        }
    }

    /// Points along each edge of each ring at the fractions `ts`, as the tools draw edges: straight
    /// in Web Mercator.
    fn along_edges(polys: &[Vec<Vec<Coord>>], ts: &[f64]) -> Vec<Coord> {
        use rustgis_geom::{lonlat_to_merc, merc_to_lonlat};
        polys
            .iter()
            .flatten()
            .flat_map(|ring| ring.windows(2).map(|w| (lonlat_to_merc(w[0]), lonlat_to_merc(w[1]))).collect::<Vec<_>>())
            .flat_map(|(a, b)| ts.iter().map(move |t| merc_to_lonlat(Coord::new(a.x + t * (b.x - a.x), a.y + t * (b.y - a.y)))))
            .collect()
    }

    /// Points along the geodesic from `a` to `b` on WGS 84, at most 20 m apart.
    fn geodesic_samples(a: Coord, b: Coord) -> Vec<Coord> {
        use geo::InterpolatePoint;
        let n = (rustgis_geom::haversine_m(a, b) / 20.0).ceil().max(1.0) as usize;
        (0..=n)
            .map(|k| geo::Geodesic.point_at_ratio_between(geo::Point::new(a.x, a.y), geo::Point::new(b.x, b.y), k as f64 / n as f64))
            .map(|p| Coord::new(p.x(), p.y()))
            .collect()
    }

    /// The distance (metres) from `c` to a geodesic given by its [`geodesic_samples`], and whether
    /// its nearest point is strictly between the ends: the nearest sample on the sphere (among
    /// every 50th, then its neighbours), then the nearest point on the ellipsoid along the geodesic
    /// either side of it, by golden-section search to well under a millimetre.
    fn m_to_geodesic(c: Coord, samples: &[Coord]) -> (f64, bool) {
        use geo::InterpolatePoint;
        let near = |range: std::ops::Range<usize>, step: usize| {
            range
                .step_by(step)
                .min_by(|&i, &j| rustgis_geom::haversine_m(c, samples[i]).total_cmp(&rustgis_geom::haversine_m(c, samples[j])))
        };
        let coarse = near(0..samples.len(), 50).unwrap_or(0);
        let i = near(coarse.saturating_sub(50)..(coarse + 51).min(samples.len()), 1).unwrap_or(coarse);
        let (first, last) = (i.saturating_sub(1), (i + 1).min(samples.len() - 1));
        let (lo, hi) = (samples[first], samples[last]);
        let at = |t: f64| {
            let p = geo::Geodesic.point_at_ratio_between(geo::Point::new(lo.x, lo.y), geo::Point::new(hi.x, hi.y), t);
            on_wgs84_m(c, Coord::new(p.x(), p.y()))
        };
        let (mut a, mut b) = (0.0, 1.0);
        let g = (5f64.sqrt() - 1.0) / 2.0;
        for _ in 0..60 {
            let (x1, x2) = (b - g * (b - a), a + g * (b - a));
            if at(x1) <= at(x2) { b = x2 } else { a = x1 }
        }
        let t = (a + b) / 2.0;
        // Strictly between the ends unless the nearest point is the line's first or last sample.
        let at_start = first == 0 && t < 1e-6;
        let at_end = last == samples.len() - 1 && t > 1.0 - 1e-6;
        (at(t).min(at(0.0)).min(at(1.0)), !at_start && !at_end)
    }

    /// The outline keeps its distance between its vertices too, not only at them (#1159): edges
    /// drawn straight in the projection came back to lon/lat as straight lines there, so the long
    /// sides of a 2 km buffer of a 96 km east–west line sat up to 190 m off. Every vertex, and every
    /// edge's quarter points and midpoint as the tools draw edges (straight in Web Mercator), is now
    /// within 0.012% of the distance (or 1 cm) along a line's sides, and no further inside an arc
    /// than 0.031% of the distance in the projection plane (#1221). The old 32-edge circles
    /// and 15-edge caps cut about 0.5% inside and fail these bounds.
    #[test]
    fn buffer_outlines_keep_their_distance_between_vertices() {
        let fractions = [0.25, 0.5, 0.75];
        let points = |polys: &[Vec<Vec<Coord>>]| -> Vec<Coord> {
            polys.iter().flatten().flatten().copied().chain(along_edges(polys, &fractions)).collect()
        };
        // Bound the geometric error independently of how many vertices the buffer emits.
        let arc_error = |d: f64| d * 3.1e-4;

        let centre = Coord::new(10.125, 46.125);
        for c in points(&buffered(Geometry::Point(centre), 1.0)) {
            let d = on_wgs84_m(centre, c);
            assert!(d < 1_000.1 && d > 1_000.0 - arc_error(1_000.0) - 0.1, "{c:?} is {d} m from the centre");
        }

        // A long east–west line; a diagonal one across the equator, where the stray changes side
        // halfway along an edge (a 10 m buffer of it strayed 0.65 m at the quarter points); and a
        // 298 km one at 84°N, whose sides needed more halvings than the limit first allowed.
        for (a, b, km) in [
            (Coord::new(9.875, 46.25), Coord::new(11.125, 46.25), 2.0),
            (Coord::new(-0.6, -0.6), Coord::new(0.6, 0.6), 0.01),
            (Coord::new(-12.559_750_946_199_074, 83.853_999_752_232_98), Coord::new(12.559_750_946_199_074, 83.853_999_752_232_98), 0.01),
            // Longer than one projection holds (150 km from its centre): buffered in stretches,
            // densified along the WGS 84 geodesic.
            (Coord::new(6.0, 46.25), Coord::new(14.0, 46.25), 2.0),
        ] {
            let (d0, samples) = (km * 1000.0, geodesic_samples(a, b));
            // A point beside the line just inside an end is held to the side's bound; one just
            // beyond the end, to the cap's.
            // Probes 1 m inside and beyond the end, 10 m to the side, along the geodesic's last
            // 20 m step (in local metres, so the side is square to it at any latitude).
            let (end, last) = (samples[samples.len() - 1], samples[samples.len() - 2]);
            let k = end.y.to_radians().cos();
            let (dx, dy) = (end.x - last.x, end.y - last.y);
            for (along, beside) in [(-0.05, true), (0.05, false)] {
                let c = Coord::new(end.x + dx * along - dy / k * 0.5, end.y + dy * along + dx * k * 0.5);
                assert_eq!(m_to_geodesic(c, &samples).1, beside, "{c:?} {along}");
            }
            for c in points(&buffered(Geometry::Line(vec![vec![a, b]]), km)) {
                let (d, beside) = m_to_geodesic(c, &samples);
                // Along the sides, within 0.012% or 1 cm (and the sampler's millimetre); round the
                // caps, add the 0.031% arc approximation in the projection plane.
                let side = (d0 * 1.2e-4).max(0.01) + 1e-3;
                let bound = if beside { side } else { arc_error(d0) + side };
                assert!(d < d0 + side && d > d0 - bound, "{km} km: {c:?} is {d} m from the line");
            }
        }

        // Near a pole: a circle reaching past Web Mercator's 85.05° keeps its distance up to 85°
        // (an edge across 85.05° was left whole, its visible part 80 km out). Within the last
        // twentieth of a degree, where Mercator stretches its edges most, it can be 0.03% out.
        let north = Coord::new(10.0, 83.0);
        for c in points(&buffered(Geometry::Point(north), 1000.0)).into_iter().filter(|c| c.y.abs() < 85.0) {
            let d = on_wgs84_m(north, c);
            assert!(d < 1_000_000.0 * (1.0 + 1.2e-4) && d > 1_000_000.0 - arc_error(1_000_000.0) - 100.0, "{c:?} is {d} m from {north:?}");
        }

        // Coordinates off the Earth have no geodesic: buffering them doesn't panic.
        let off = [Coord::new(0.0, 100.0), Coord::new(20.0, 100.0)];
        let mut ds = Dataset::new("f", GeometryType::Polyline, vec![]);
        ds.push(Some(Geometry::Line(vec![off.to_vec()])), vec![]);
        let m = Map(HashMap::from([("f".to_string(), ds)]));
        let r = crate::run("buffer", &m, &serde_json::json!({"input": "f", "distance": 1}));
        assert!(!matches!(&r, Err(e) if e.ends_with("failed unexpectedly")), "{:?}", r.err());
    }

    /// Buffers are geodesic (#394): Web Mercator's scale changes with latitude, so one scale for
    /// the whole feature made a 1000 km circle at 60°N reach 872 km north and 1142 km south, and a
    /// 50 km buffer of a line from 5°S to 33°S 59 km wide at one end and 50 km at the other.
    #[test]
    fn buffers_keep_their_distance_on_the_earth() {
        let centre = Coord::new(10.0, 60.0);
        let circle = buffered(Geometry::Point(centre), 1000.0);
        all_at(&circle, &[centre, centre], 1000.0);
        let north = circle.iter().flatten().flatten().map(|c| c.y).fold(f64::MIN, f64::max);
        let south = circle.iter().flatten().flatten().map(|c| c.y).fold(f64::MAX, f64::min);
        // As far north as south, where GeographicLib puts 1000 km on WGS 84 (a degree of
        // latitude grows towards the pole, so 8.970° north and 8.982° south).
        assert!((north - 68.969_947_795_748).abs() < 1e-9 && (south - 51.017_861_794_779).abs() < 1e-9, "{north} {south}");

        let line = [Coord::new(-45.0, -5.0), Coord::new(-45.0, -33.0)];
        all_at(&buffered(Geometry::Line(vec![line.to_vec()]), 50.0), &line, 50.0);

        // A polygon too large for one projection, outward and inward, keeps the distance from
        // its outline; inward, the ring sits inside.
        let ring =
            vec![Coord::new(0.0, 40.0), Coord::new(20.0, 40.0), Coord::new(20.0, 60.0), Coord::new(0.0, 60.0), Coord::new(0.0, 40.0)];
        let grown = buffered(Geometry::Polygon(vec![vec![ring.clone()]]), 100.0);
        assert_eq!(grown.len(), 1, "one polygon, not the pieces it was drawn in");
        all_at(&grown, &ring, 100.0);
        let shrunk = buffered(Geometry::Polygon(vec![vec![ring.clone()]]), -100.0);
        all_at(&shrunk, &ring, 100.0);
        assert!(shrunk.iter().flatten().flatten().all(|c| c.x > 0.0 && c.x < 20.0 && c.y > 40.0 && c.y < 60.0));

        // A polygon split at the antimeridian keeps each half's buffer next to the half: buffered
        // in a projection centred near 180°, the western half came back at 185°, not −175°, and
        // missed the polygon it was joined to.
        let half = |w: f64, e: f64| {
            vec![vec![Coord::new(w, 60.0), Coord::new(e, 60.0), Coord::new(e, 70.0), Coord::new(w, 70.0), Coord::new(w, 60.0)]]
        };
        let split = buffered(Geometry::Polygon(vec![half(170.0, 180.0), half(-180.0, -170.0)]), 100.0);
        assert_eq!(split.len(), 2, "{split:?}");
        let within = |poly: &Vec<Vec<Coord>>, w: f64, e: f64| poly.iter().flatten().all(|c| (w..=e).contains(&c.x));
        assert!(split.iter().any(|p| within(p, 165.0, 185.0)) && split.iter().any(|p| within(p, -185.0, -165.0)), "{split:?}");
        // The error of a projection centred on the feature grows with a vertex's distance from
        // the centre, not with the buffer's distance: a small square keeps 3000 km.
        let small =
            vec![Coord::new(30.0, 50.0), Coord::new(31.0, 50.0), Coord::new(31.0, 51.0), Coord::new(30.0, 51.0), Coord::new(30.0, 50.0)];
        all_at(&buffered(Geometry::Polygon(vec![vec![small.clone()]]), 3000.0), &small, 3000.0);
        // A line round the world at the largest distance is a few hundred pieces, joined.
        let round = (0..=8).map(|i| Coord::new(-180.0 + 45.0 * i as f64, 10.0)).collect::<Vec<_>>();
        assert!(!buffered(Geometry::Line(vec![round]), 5000.0).is_empty());
        // A long line keeps a long distance: a stretch's vertices are up to 300 km from the
        // centre of its projection.
        all_at(&buffered(Geometry::Line(vec![line.to_vec()]), 1000.0), &line, 1000.0);
        // A circle round a pole is closed along it, so in lon/lat it holds the pole's
        // neighbourhood at every longitude; a circle centred on a pole is one too.
        for (centre, km) in [(Coord::new(10.0, 85.0), 1000.0), (Coord::new(0.0, 90.0), 500.0), (Coord::new(-60.0, -80.0), 1500.0)] {
            let cap = buffered(Geometry::Point(centre), km);
            all_at(&cap, &[centre, centre], km);
            let pole = centre.y.signum() * 89.0;
            let poly = cap.first().map(|rings| geo::Polygon::new(rings[0].iter().map(|c| geo::Coord { x: c.x, y: c.y }).collect(), vec![]));
            for lon in [-179.0, -90.0, 0.0, 45.0, 179.0] {
                assert!(
                    poly.as_ref().is_some_and(|p| geo::Contains::contains(p, &geo::Point::new(lon, pole))),
                    "{centre:?}: ({lon}, {pole})"
                );
            }
        }
        // Two antipodes still have their circles.
        let ends = buffered(Geometry::MultiPoint(vec![Coord::new(0.0, 0.0), Coord::new(180.0, 0.0)]), 100.0);
        assert_eq!(ends.len(), 2);
    }

    #[test]
    fn a_tool_cancelled_while_it_runs_stops_with_how_far_it_came() {
        // 2,000 points on 400 × 400 cells: 320 M cell-point pairs, far longer than the watcher takes
        // to notice the first rows.
        let mut pts = Dataset::new("pts", GeometryType::Point, vec![Field::new("v", FieldType::Double)]);
        for i in 0..2_000 {
            let (x, y) = (f64::from(i % 50) * 0.2, f64::from(i / 50) * 0.25);
            pts.push(Some(Geometry::Point(Coord::new(x, y))), vec![Value::Float(f64::from(i))]);
        }
        let m = Map(HashMap::from([("pts".to_string(), pts)]));
        let params = serde_json::json!({"input": "pts", "field": "v", "extent": "0,0,10,10", "pixelSize": 0.025});
        // Asked to stop before it starts: it never does.
        let control = crate::control::Control::new();
        control.cancel();
        assert_eq!(
            crate::run_with("idwInterpolation", &m, &params, &control).err().as_deref(),
            Some("IDW Interpolation was cancelled before it started")
        );
        // Asked once it has made some rows: it stops well before the end.
        let control = crate::control::Control::new();
        let watcher = {
            let control = control.clone();
            std::thread::spawn(move || {
                let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
                while control.progress().is_none_or(|(done, _)| done == 0) && std::time::Instant::now() < end {
                    std::thread::yield_now();
                }
                control.cancel();
            })
        };
        let r = crate::run_with("idwInterpolation", &m, &params, &control);
        let _ = watcher.join();
        let e = r.err().unwrap_or_default();
        assert!(e.starts_with("IDW Interpolation was cancelled "), "{e}");
        let (done, total) = control.progress().unwrap_or_default();
        assert!(total == 400 && done < total, "{done} of {total} rows");
        // Without a control nothing changes.
        assert!(
            crate::run("idwInterpolation", &m, &serde_json::json!({"input": "pts", "field": "v", "extent": "0,0,10,10", "pixelSize": 1.0}))
                .is_ok()
        );
    }

    #[test]
    fn buffers_count_their_features_toward_the_progress() {
        let mut pts = Dataset::new("pts", GeometryType::Point, vec![]);
        for i in 0..40 {
            pts.push(Some(Geometry::Point(Coord::new(f64::from(i) * 0.1, 0.0))), vec![]);
        }
        let m = Map(HashMap::from([("pts".to_string(), pts)]));
        let control = crate::control::Control::new();
        crate::run_with("buffer", &m, &serde_json::json!({"input": "pts", "distance": 100.0}), &control).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(control.progress(), Some((40, 40)));
        // The same control for a tool that doesn't count shows no count, not the buffers'.
        crate::run_with("convexHull", &m, &serde_json::json!({"input": "pts"}), &control).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(control.progress(), None);
        // Dissolved, the union after the buffers isn't counted: the buffers' count ends before it.
        let dissolve = serde_json::json!({"input": "pts", "distance": 100.0, "dissolve": "All"});
        crate::run_with("buffer", &m, &dissolve, &control).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(control.progress(), None);
        // A loop counting each feature as it reaches it ends with all of them counted.
        let near = serde_json::json!({"input": "pts", "select": "pts", "distance": 10.0});
        crate::run_with("selectByLocation", &m, &near, &control).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(control.progress(), Some((40, 40)));
    }

    #[test]
    fn idw_interpolation_writes_a_raster() {
        let m = inputs();
        let r =
            crate::run("idwInterpolation", &m, &serde_json::json!({"input": "pts", "field": "v", "extent": "0,0,2,2", "pixelSize": 0.5}))
                .unwrap_or_else(|e| panic!("{e}"));
        let Output::Layers(out) = r.output else { panic!("expected layers") };
        let Some(OutputLayer::Raster { name, file_name, bytes }) = out.first() else { panic!("expected a raster: {out:?}") };
        assert_eq!(name, "pts_v_idw");
        let g = rustgis_data::raster::read_bytes(file_name, bytes).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((g.width, g.height), (4, 4));
        // Every value lies between the inputs', nearest the point it sits closest to.
        let d = &g.bands[0].data;
        assert!(d.iter().all(|v| (1.0..=3.0).contains(v)), "{d:?}");
        assert!(d[12] < d[3], "south-west, by the 1, is below north-east, by the 2: {d:?}");
        // A text field gives no values; a bad extent or power is refused.
        assert!(crate::run("idwInterpolation", &m, &serde_json::json!({"input": "polys", "field": "k"})).is_err(), "text field");
        for p in [
            serde_json::json!({"extent": "1,2,3"}),
            serde_json::json!({"extent": "0,bad,1,2,3"}),
            serde_json::json!({"extent": "0,0,1,NaN"}),
            serde_json::json!({"power": -1}),
            serde_json::json!({"power": "x"}),
            serde_json::json!({"pixelSize": 0}),
            serde_json::json!({"pixelSize": "-1"}),
            serde_json::json!({"pixelSize": "x"}),
        ] {
            let mut p = p;
            p["input"] = "pts".into();
            p["field"] = "v".into();
            assert!(crate::run("idwInterpolation", &m, &p).is_err(), "{p}");
        }
    }

    #[test]
    fn idw_auto_cell_stays_within_the_work_limit() {
        let e = Extent::new(0.0, 0.0, 3.0, 3.0);
        assert!((idw_auto_cell(&e, 10) - 0.01).abs() < 1e-12, "few points keep 300 cells across");
        for n in [5_000, 50_000, 1_000_000] {
            let c = idw_auto_cell(&e, n);
            let cells = ((3.0 / c).ceil() * (3.0 / c).ceil()) as usize;
            assert!(cells * n <= crate::rasterops::MAX_IDW_WORK, "{n} points: {cells} cells of {c}");
        }
    }

    #[test]
    fn kriging_interpolation_writes_prediction_and_variance_rasters() {
        let m = inputs();
        let r = crate::run(
            "kriging",
            &m,
            &serde_json::json!({
                "input": "pts",
                "field": "v",
                "model": "Spherical",
                "extent": "0,0,2,2",
                "pixelSize": 0.5
            }),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let Output::Layers(out) = r.output else { panic!("expected layers") };
        assert_eq!(out.len(), 2, "Expected prediction and variance rasters");
        let (pred_layer, var_layer) = match (&out[0], &out[1]) {
            (
                OutputLayer::Raster { name: p_name, file_name: p_file, bytes: p_bytes },
                OutputLayer::Raster { name: v_name, file_name: v_file, bytes: v_bytes },
            ) => {
                assert_eq!(p_name, "pts_v_kriging");
                assert_eq!(v_name, "pts_v_kriging_variance");
                (
                    rustgis_data::raster::read_bytes(p_file, p_bytes).unwrap_or_else(|e| panic!("{e}")),
                    rustgis_data::raster::read_bytes(v_file, v_bytes).unwrap_or_else(|e| panic!("{e}")),
                )
            }
            _ => panic!("expected two raster output layers: {out:?}"),
        };
        assert_eq!((pred_layer.width, pred_layer.height), (4, 4));
        assert_eq!((var_layer.width, var_layer.height), (4, 4));
        // Values in prediction band
        let d = &pred_layer.bands[0].data;
        assert!(d.iter().all(|v| (0.5..=3.5).contains(v)), "{d:?}");
        // Variance in variance raster is non-negative
        let var = &var_layer.bands[0].data;
        assert!(var.iter().all(|v| *v >= 0.0), "{var:?}");

        // Invalid models or parameters return errors cleanly
        assert!(crate::run("kriging", &m, &serde_json::json!({"input": "pts", "field": "v", "model": "UnknownModel"})).is_err());
        assert!(crate::run("kriging", &m, &serde_json::json!({"input": "pts", "field": "v", "neighbours": 1})).is_err());
        assert!(crate::run("kriging", &m, &serde_json::json!({"input": "pts", "field": "v", "nugget": -1})).is_err());
        assert!(crate::run("kriging", &m, &serde_json::json!({"input": "pts", "field": "v", "nugget": "abc"})).is_err());
        assert!(crate::run("kriging", &m, &serde_json::json!({"input": "pts", "field": "v", "sill": 0})).is_err());
        assert!(crate::run("kriging", &m, &serde_json::json!({"input": "pts", "field": "v", "sill": "xyz"})).is_err());
        assert!(crate::run("kriging", &m, &serde_json::json!({"input": "pts", "field": "v", "range": -5})).is_err());
        assert!(crate::run("kriging", &m, &serde_json::json!({"input": "pts", "field": "v", "range": "none"})).is_err());

        // Source CRS metadata does not change the normalized longitude/latitude geometry.
        let Some(pts_layer) = m.0.get("pts") else { panic!("missing pts") };
        let mut proj_pts = pts_layer.clone();
        proj_pts.crs = Some(rustgis_data::raster::Crs::WebMercator);
        let m_proj = Map(HashMap::from([("proj_pts".to_string(), proj_pts)]));
        let projected =
            crate::run("kriging", &m_proj, &serde_json::json!({"input": "proj_pts", "field": "v", "extent": "0,0,2,2", "pixelSize": 0.5}))
                .unwrap();
        let Output::Layers(projected_layers) = projected.output else { panic!("expected layers") };
        assert_eq!(projected_layers.len(), out.len());
        for (actual, expected) in projected_layers.iter().zip(&out) {
            let (OutputLayer::Raster { bytes: actual, .. }, OutputLayer::Raster { bytes: expected, .. }) = (actual, expected) else {
                panic!("expected raster layers")
            };
            assert_eq!(actual, expected);
        }

        // Datasets with fewer than two points are cleanly rejected
        let mut single_pt = Dataset::new("single", GeometryType::Point, vec![Field::new("v", FieldType::Double)]);
        single_pt.push(Some(Geometry::Point(Coord::new(0.5, 0.5))), vec![Value::Float(1.0)]);
        let m_single = Map(HashMap::from([("single".to_string(), single_pt)]));
        assert_eq!(
            crate::run("kriging", &m_single, &serde_json::json!({"input": "single", "field": "v"})).unwrap_err(),
            "kriging needs at least two points with values"
        );

        // Deduplication averages coincident points with true arithmetic mean
        let mut dup_pts = Dataset::new("dup", GeometryType::Point, vec![Field::new("v", FieldType::Double)]);
        dup_pts.push(Some(Geometry::Point(Coord::new(0.5, 0.5))), vec![Value::Float(10.0)]);
        dup_pts.push(Some(Geometry::Point(Coord::new(0.5, 0.5))), vec![Value::Float(20.0)]);
        dup_pts.push(Some(Geometry::Point(Coord::new(0.5, 0.5))), vec![Value::Float(30.0)]);
        dup_pts.push(Some(Geometry::Point(Coord::new(1.5, 1.5))), vec![Value::Float(20.0)]);
        let m_dup = Map(HashMap::from([("dup".to_string(), dup_pts)]));
        let r_dup = crate::run(
            "kriging",
            &m_dup,
            &serde_json::json!({
                "input": "dup",
                "field": "v",
                "extent": "0,0,2,2",
                "pixelSize": 0.5
            }),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let Output::Layers(dup_layers) = r_dup.output else { panic!("expected layers") };
        let pred = match &dup_layers[0] {
            OutputLayer::Raster { file_name, bytes, .. } => rustgis_data::raster::read_bytes(file_name, bytes).unwrap(),
            _ => panic!("expected raster"),
        };
        assert!(pred.bands[0].data.iter().all(|v| (19.99..=20.01).contains(v)), "{:?}", pred.bands[0].data);

        // Deduplication merges near-duplicate samples across x-windows even when separated by different y
        let mut inter_pts = Dataset::new("inter", GeometryType::Point, vec![Field::new("v", FieldType::Double)]);
        inter_pts.push(Some(Geometry::Point(Coord::new(0.5, 0.5))), vec![Value::Float(10.0)]);
        inter_pts.push(Some(Geometry::Point(Coord::new(0.5 + 5e-13, 10.0))), vec![Value::Float(100.0)]);
        inter_pts.push(Some(Geometry::Point(Coord::new(0.5 + 9e-13, 0.5))), vec![Value::Float(30.0)]);
        inter_pts.push(Some(Geometry::Point(Coord::new(1.5, 1.5))), vec![Value::Float(20.0)]);
        let m_inter = Map(HashMap::from([("inter".to_string(), inter_pts)]));
        let r_inter = crate::run(
            "kriging",
            &m_inter,
            &serde_json::json!({
                "input": "inter",
                "field": "v",
                "extent": "0.25,0.25,0.75,0.75",
                "pixelSize": 0.5
            }),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let Output::Layers(inter_layers) = r_inter.output else { panic!("expected layers") };
        let pred_inter = match &inter_layers[0] {
            OutputLayer::Raster { file_name, bytes, .. } => rustgis_data::raster::read_bytes(file_name, bytes).unwrap(),
            _ => panic!("expected raster"),
        };
        assert!((pred_inter.bands[0].data[0] - 20.0).abs() < 1e-4, "expected 20.0 at (0.5, 0.5), got {}", pred_inter.bands[0].data[0]);
    }

    /// A Buffer refused for memory names the budget on this computer (#1291 step 2), which on the
    /// desktop follows its memory and is never below the fixed one; other errors are left as they are.
    #[test]
    fn buffer_budget_follows_the_computer_and_is_named() {
        assert!(max_buffer_bytes() >= MAX_BUFFER_BYTES && max_buffer_bytes() <= BUFFER_BYTES_CEILING.max(MAX_BUFFER_BYTES));
        let e = with_budget(BUFFER_STORAGE_ERROR.to_string(), 512 << 20);
        assert!(e.starts_with(BUFFER_STORAGE_ERROR) && e.contains("512 MiB"), "{e}");
        assert_eq!(with_budget("Distance must be a number".into(), 512 << 20), "Distance must be a number");
    }

    #[test]
    fn kernel_density_analysis_tool_tests() {
        // Create point dataset with UTM projection
        let mut pts = Dataset::new("stations", GeometryType::Point, vec![Field::new("pop", FieldType::Double)]);
        pts.crs = Some(Crs::from_epsg(32618).unwrap()); // UTM zone 18N
        let tr = pts.crs.unwrap().transformer().unwrap();

        // 3 points around central meridian (-75.0, 40.0)
        let c1_proj = Coord::new(500_000.0, 4_428_000.0);
        let c2_proj = Coord::new(500_500.0, 4_428_200.0);
        let c3_proj = Coord::new(501_000.0, 4_428_000.0);

        pts.push(Some(Geometry::Point(tr.to_lonlat(c1_proj).unwrap())), vec![Value::Float(10.0)]);
        pts.push(Some(Geometry::Point(tr.to_lonlat(c2_proj).unwrap())), vec![Value::Float(20.0)]);
        pts.push(Some(Geometry::Point(tr.to_lonlat(c3_proj).unwrap())), vec![Value::Float(30.0)]);

        let mut polys = Dataset::new("bounds", GeometryType::Polygon, vec![]);
        polys.push(Some(sq(0.0, 0.0, 1.0)), vec![]);

        let m = Map(HashMap::from([("stations".to_string(), pts), ("bounds".to_string(), polys)]));

        // 1. Normal run with weights and explicit bandwidth
        let res = crate::run(
            "kernelDensity",
            &m,
            &serde_json::json!({
                "input": "stations",
                "field": "pop",
                "radius": 1000.0,
                "pixelSize": 50.0,
                "output": "stations_kde"
            }),
        )
        .unwrap();

        let Output::Layers(layers) = res.output else { panic!("expected Output::Layers") };
        assert_eq!(layers.len(), 1);
        let (name, r) = match &layers[0] {
            OutputLayer::Raster { name, file_name, bytes } => {
                assert_eq!(file_name, "stations_kde.tif");
                (name.clone(), rustgis_data::raster::geotiff::read(std::io::Cursor::new(bytes), name).unwrap())
            }
            _ => panic!("expected OutputLayer::Raster"),
        };
        assert_eq!(name, "stations_kde");
        assert_eq!(r.crs, Crs::from_epsg(32618).unwrap());
        assert!(r.width > 0 && r.height > 0);
        let peak = r.bands[0].data.iter().copied().fold(0.0f32, f32::max);
        assert!(peak > 0.0, "peak density must be positive, got {peak}");

        // 2. Auto bandwidth and omitted weight field
        let res_auto = crate::run(
            "kernelDensity",
            &m,
            &serde_json::json!({
                "input": "stations",
                "pixelSize": 50.0
            }),
        )
        .unwrap();
        let Output::Layers(layers_auto) = res_auto.output else { panic!("expected layers") };
        assert_eq!(layers_auto.len(), 1);

        let pt_extent = Extent::new(500_000.0, 4_428_000.0, 501_000.0, 4_428_200.0);
        let expected_bw = crate::density::silverman_bandwidth(&[(c1_proj, 1.0), (c2_proj, 1.0), (c3_proj, 1.0)], &pt_extent);
        assert!(
            res_auto.messages[0].contains(&format!("bandwidth: {expected_bw:.2}")),
            "expected silverman bandwidth {expected_bw:.2} in message: {}",
            res_auto.messages[0]
        );

        let OutputLayer::Raster { bytes: auto_bytes, .. } = &layers_auto[0] else { panic!("expected OutputLayer::Raster") };
        let r_auto = rustgis_data::raster::geotiff::read(std::io::Cursor::new(auto_bytes), "stations_density").unwrap();
        let expected_origin_x = pt_extent.min_x - expected_bw;
        assert!(
            (r_auto.transform.origin_x - expected_origin_x).abs() < 1e-4,
            "origin_x ({}) must match expected Silverman bandwidth expansion ({expected_origin_x})",
            r_auto.transform.origin_x
        );

        // Explicit radius 0.0 behaves identically to omitted radius (sentinel for Silverman bandwidth)
        let res_zero = crate::run(
            "kernelDensity",
            &m,
            &serde_json::json!({
                "input": "stations",
                "pixelSize": 50.0,
                "radius": 0.0
            }),
        )
        .unwrap();
        assert!(res_zero.messages[0].contains(&format!("bandwidth: {expected_bw:.2}")));

        // 3. Gaussian kernel run
        let res_gauss = crate::run(
            "kernelDensity",
            &m,
            &serde_json::json!({
                "input": "stations",
                "radius": 800.0,
                "pixelSize": 50.0,
                "kernel": "Gaussian"
            }),
        )
        .unwrap();
        let Output::Layers(layers_gauss) = res_gauss.output else { panic!("expected layers") };
        assert_eq!(layers_gauss.len(), 1);

        // 4. Reject non-point layer
        let err_poly = crate::run("kernelDensity", &m, &serde_json::json!({"input": "bounds"})).unwrap_err();
        assert!(err_poly.contains("must be a point layer"), "{err_poly}");

        // 5. Reject geographic CRS without explicit projected crs param
        let mut geo_pts = Dataset::new("geopts", GeometryType::Point, vec![]);
        geo_pts.crs = Some(Crs::LonLat);
        geo_pts.push(Some(Geometry::Point(Coord::new(10.0, 10.0))), vec![]);
        let mut no_crs_pts = Dataset::new("nocrs", GeometryType::Point, vec![]);
        no_crs_pts.crs = None;
        no_crs_pts.push(Some(Geometry::Point(Coord::new(10.0, 10.0))), vec![]);
        let m_geo = Map(HashMap::from([("geopts".to_string(), geo_pts), ("nocrs".to_string(), no_crs_pts)]));
        let err_geo = crate::run("kernelDensity", &m_geo, &serde_json::json!({"input": "geopts"})).unwrap_err();
        assert!(err_geo.contains("is in geographic coordinates (degrees)"), "{err_geo}");
        assert!(err_geo.contains("Provide a projected system under 'crs'"), "{err_geo}");
        let err_explicit_geo =
            crate::run("kernelDensity", &m_geo, &serde_json::json!({"input": "geopts", "crs": "EPSG:4326"})).unwrap_err();
        assert!(err_explicit_geo.contains("crs must be a projected coordinate system with linear units"), "{err_explicit_geo}");
        let err_nocrs = crate::run("kernelDensity", &m_geo, &serde_json::json!({"input": "nocrs"})).unwrap_err();
        assert!(err_nocrs.contains("has no coordinate system"), "{err_nocrs}");

        // 6. Geographic CRS succeeds when projected crs parameter is supplied
        let res_projected = crate::run(
            "kernelDensity",
            &m_geo,
            &serde_json::json!({
                "input": "geopts",
                "crs": "EPSG:3857",
                "radius": 50000.0,
                "pixelSize": 5000.0
            }),
        )
        .unwrap();
        let Output::Layers(layers_proj) = res_projected.output else { panic!("expected layers") };
        assert_eq!(layers_proj.len(), 1);

        // 7. Reject negative radius and negative pixelSize
        assert!(crate::run("kernelDensity", &m, &serde_json::json!({"input": "stations", "radius": -10.0})).is_err());
        assert!(crate::run("kernelDensity", &m, &serde_json::json!({"input": "stations", "pixelSize": -5.0})).is_err());
        assert!(crate::run("kernelDensity", &m, &serde_json::json!({"input": "stations", "kernel": "InvalidKernel"})).is_err());

        // 8. Reject invalid field parameter types (must be string or null/missing)
        let err_num_field = crate::run("kernelDensity", &m, &serde_json::json!({"input": "stations", "field": 123})).unwrap_err();
        assert!(err_num_field.contains("field must be text"), "{err_num_field}");
        let err_bool_field = crate::run("kernelDensity", &m, &serde_json::json!({"input": "stations", "field": true})).unwrap_err();
        assert!(err_bool_field.contains("field must be text"), "{err_bool_field}");
        let err_arr_field = crate::run("kernelDensity", &m, &serde_json::json!({"input": "stations", "field": ["pop"]})).unwrap_err();
        assert!(err_arr_field.contains("field must be text"), "{err_arr_field}");
        let err_obj_field =
            crate::run("kernelDensity", &m, &serde_json::json!({"input": "stations", "field": {"name": "pop"}})).unwrap_err();
        assert!(err_obj_field.contains("field must be text"), "{err_obj_field}");

        // Explicit null field runs successfully unweighted
        assert!(crate::run("kernelDensity", &m, &serde_json::json!({"input": "stations", "field": null})).is_ok());

        // Reject invalid types for crs, kernel, and output
        let err_crs_num = crate::run("kernelDensity", &m, &serde_json::json!({"input": "stations", "crs": 3857})).unwrap_err();
        assert!(err_crs_num.contains("crs must be text"), "{err_crs_num}");
        let err_kernel_num = crate::run("kernelDensity", &m, &serde_json::json!({"input": "stations", "kernel": 123})).unwrap_err();
        assert!(err_kernel_num.contains("must be one of Quartic, Gaussian"), "{err_kernel_num}");
        let err_out_bool = crate::run("kernelDensity", &m, &serde_json::json!({"input": "stations", "output": true})).unwrap_err();
        assert!(err_out_bool.contains("output must be text"), "{err_out_bool}");

        // Extent parameter accepts both text and JSON number arrays, and rejects invalid types
        let res_ext_str = crate::run(
            "kernelDensity",
            &m,
            &serde_json::json!({
                "input": "stations",
                "extent": "500000, 4428000, 501000, 4428200",
                "pixelSize": 50.0
            }),
        )
        .unwrap();
        assert_eq!(res_ext_str.messages.len(), 1);

        let res_ext_arr = crate::run(
            "kernelDensity",
            &m,
            &serde_json::json!({
                "input": "stations",
                "extent": [500000.0, 4428000.0, 501000.0, 4428200.0],
                "pixelSize": 50.0
            }),
        )
        .unwrap();
        assert_eq!(res_ext_arr.messages.len(), 1);

        let err_ext_short =
            crate::run("kernelDensity", &m, &serde_json::json!({"input": "stations", "extent": [500000.0, 4428000.0]})).unwrap_err();
        assert!(err_ext_short.contains("the extent is xmin,ymin,xmax,ymax"), "{err_ext_short}");
        let err_ext_num = crate::run("kernelDensity", &m, &serde_json::json!({"input": "stations", "extent": 12345})).unwrap_err();
        assert!(err_ext_num.contains("the extent is xmin,ymin,xmax,ymax"), "{err_ext_num}");
        let err_ext_zero =
            crate::run("kernelDensity", &m, &serde_json::json!({"input": "stations", "extent": "500000,4428000,500000,4428000"}))
                .unwrap_err();
        assert!(err_ext_zero.contains("positive width and height"), "{err_ext_zero}");

        // 9. Report skipped points with non-positive or invalid weights
        let mut mixed_ds = m.0.get("stations").unwrap().clone();
        mixed_ds.push(Some(Geometry::Point(tr.to_lonlat(Coord::new(500_500.0, 4_428_100.0)).unwrap())), vec![Value::Float(-10.0)]);
        let m_mixed = Map(HashMap::from([("mixed".to_string(), mixed_ds)]));
        let res_mixed = crate::run(
            "kernelDensity",
            &m_mixed,
            &serde_json::json!({
                "input": "mixed",
                "field": "pop",
                "pixelSize": 50.0
            }),
        )
        .unwrap();
        assert!(res_mixed.messages[0].contains("1 skipped"), "expected '1 skipped' in: {}", res_mixed.messages[0]);

        // 10. MultiPoint geometry support
        let mut mp_ds = Dataset::new("mp", GeometryType::Point, vec![Field::new("w", FieldType::Double)]);
        mp_ds.crs = Some(Crs::from_epsg(32618).unwrap());
        mp_ds.push(
            Some(Geometry::MultiPoint(vec![
                tr.to_lonlat(Coord::new(500_000.0, 4_428_000.0)).unwrap(),
                tr.to_lonlat(Coord::new(500_100.0, 4_428_100.0)).unwrap(),
            ])),
            vec![Value::Float(5.0)],
        );
        let m_mp = Map(HashMap::from([("mp".to_string(), mp_ds)]));
        let res_mp =
            crate::run("kernelDensity", &m_mp, &serde_json::json!({"input": "mp", "field": "w", "pixelSize": 25.0, "radius": 100.0}))
                .unwrap();
        assert_eq!(res_mp.messages.len(), 1);

        // 11. Reject excessive workload exceeding MAX_DENSITY_WORK
        let mut crowded = Dataset::new("crowded", GeometryType::Point, vec![]);
        crowded.crs = Some(Crs::from_epsg(32618).unwrap());
        for i in 0..500 {
            crowded.push(Some(Geometry::Point(tr.to_lonlat(Coord::new(500_000.0 + (i as f64) * 2.0, 4_428_000.0)).unwrap())), vec![]);
        }
        let m_crowded = Map(HashMap::from([("crowded".to_string(), crowded)]));
        let err_work = crate::run(
            "kernelDensity",
            &m_crowded,
            &serde_json::json!({
                "input": "crowded",
                "radius": 1000.0,
                "pixelSize": 1.0
            }),
        )
        .unwrap_err();
        assert!(err_work.contains("too much work"), "{err_work}");

        // 12. Text-valued weight field produces clear, actionable error
        let mut text_field_ds = Dataset::new("stations_text", GeometryType::Point, vec![Field::new("city", FieldType::Text)]);
        text_field_ds.crs = Some(Crs::from_epsg(32618).unwrap());
        text_field_ds
            .push(Some(Geometry::Point(tr.to_lonlat(Coord::new(500_000.0, 4_428_000.0)).unwrap())), vec![Value::Text("Boston".into())]);
        let m_text = Map(HashMap::from([("stations_text".to_string(), text_field_ds)]));
        let err_text_field =
            crate::run("kernelDensity", &m_text, &serde_json::json!({"input": "stations_text", "field": "city"})).unwrap_err();
        assert!(err_text_field.contains("field 'city' must be numeric (is Text)"), "{err_text_field}");

        // 13. MultiPoint vs single point vs multiple independent points
        let p1 = tr.to_lonlat(Coord::new(500_000.0, 4_428_000.0)).unwrap();
        let p2 = tr.to_lonlat(Coord::new(500_100.0, 4_428_100.0)).unwrap();

        // Single point dataset
        let mut single_ds = Dataset::new("single", GeometryType::Point, vec![Field::new("w", FieldType::Double)]);
        single_ds.crs = Some(Crs::from_epsg(32618).unwrap());
        single_ds.push(Some(Geometry::Point(p1)), vec![Value::Float(2.0)]);

        // MultiPoint dataset with 2 parts
        let mut multi_ds = Dataset::new("multi", GeometryType::Point, vec![Field::new("w", FieldType::Double)]);
        multi_ds.crs = Some(Crs::from_epsg(32618).unwrap());
        multi_ds.push(Some(Geometry::MultiPoint(vec![p1, p2])), vec![Value::Float(2.0)]);

        // Two independent point features
        let mut two_pts_ds = Dataset::new("two_pts", GeometryType::Point, vec![Field::new("w", FieldType::Double)]);
        two_pts_ds.crs = Some(Crs::from_epsg(32618).unwrap());
        two_pts_ds.push(Some(Geometry::Point(p1)), vec![Value::Float(2.0)]);
        two_pts_ds.push(Some(Geometry::Point(p2)), vec![Value::Float(2.0)]);

        let m_cmp =
            Map(HashMap::from([("single".to_string(), single_ds), ("multi".to_string(), multi_ds), ("two_pts".to_string(), two_pts_ds)]));

        let res_single = crate::run(
            "kernelDensity",
            &m_cmp,
            &serde_json::json!({"input": "single", "field": "w", "extent": "499800,4427800,500300,4428300", "pixelSize": 25.0, "radius": 150.0}),
        ).unwrap();
        let res_multi = crate::run(
            "kernelDensity",
            &m_cmp,
            &serde_json::json!({"input": "multi", "field": "w", "extent": "499800,4427800,500300,4428300", "pixelSize": 25.0, "radius": 150.0}),
        ).unwrap();
        let res_two_pts = crate::run(
            "kernelDensity",
            &m_cmp,
            &serde_json::json!({"input": "two_pts", "field": "w", "extent": "499800,4427800,500300,4428300", "pixelSize": 25.0, "radius": 150.0}),
        ).unwrap();

        let extract_raster_bytes = |out: Output| match out {
            Output::Layers(mut layers) => match layers.remove(0) {
                OutputLayer::Raster { bytes, .. } => bytes,
                _ => panic!("expected Raster"),
            },
            _ => panic!("expected Layers"),
        };
        let bytes_single = extract_raster_bytes(res_single.output);
        let bytes_multi = extract_raster_bytes(res_multi.output);
        let bytes_two_pts = extract_raster_bytes(res_two_pts.output);

        let r_single = rustgis_data::raster::geotiff::read(std::io::Cursor::new(&bytes_single), "s").unwrap();
        let r_multi = rustgis_data::raster::geotiff::read(std::io::Cursor::new(&bytes_multi), "m").unwrap();
        let r_two_pts = rustgis_data::raster::geotiff::read(std::io::Cursor::new(&bytes_two_pts), "t").unwrap();

        // MultiPoint with 2 parts must produce exact same output as 2 independent points each with weight 2.0
        assert_eq!(r_multi.bands[0].data, r_two_pts.bands[0].data, "MultiPoint must match two independent point features");
        // And must differ from single point (which only contains p1)
        assert_ne!(r_multi.bands[0].data, r_single.bands[0].data, "MultiPoint must differ from single point");

        // 14. Extent whose span overflows f64 must be rejected with extent error, not pixel size error
        let err_str_overflow =
            crate::run("kernelDensity", &m_cmp, &serde_json::json!({"input": "single", "extent": "-1e308,-1e308,1e308,1e308"}))
                .unwrap_err();
        assert!(
            err_str_overflow.contains("the extent must have a positive width and height"),
            "expected extent error, got: {err_str_overflow}"
        );
        assert!(
            !err_str_overflow.contains("pixel size must be a positive number"),
            "should not blame pixel size when user did not set one: {err_str_overflow}"
        );

        let err_arr_overflow =
            crate::run("kernelDensity", &m_cmp, &serde_json::json!({"input": "single", "extent": [-1e308, -1e308, 1e308, 1e308]}))
                .unwrap_err();
        assert!(
            err_arr_overflow.contains("the extent must have a positive width and height"),
            "expected extent error, got: {err_arr_overflow}"
        );

        // 15. Hostile and extreme parameters must be rejected cleanly without panic
        let hostile_cases = vec![
            serde_json::json!({"input": "single", "radius": -100.0}),
            serde_json::json!({"input": "single", "radius": 1e308}),
            serde_json::json!({"input": "single", "pixelSize": -5.0}),
            serde_json::json!({"input": "single", "pixelSize": 0.0}),
            serde_json::json!({"input": "single", "pixelSize": 1e-12}),
            serde_json::json!({"input": "single", "kernel": "nonexistent"}),
            serde_json::json!({"input": "single", "extent": "0,0,10"}),
            serde_json::json!({"input": "single", "extent": [10.0, 20.0]}),
        ];
        for case in hostile_cases {
            let res = crate::run("kernelDensity", &m_cmp, &case);
            assert!(res.is_err(), "hostile params should return Err, but got Ok");
        }

        // 16. pixelSize > radius must be rejected with informative error
        let err_pixel_larger =
            crate::run("kernelDensity", &m_cmp, &serde_json::json!({"input": "single", "pixelSize": 250.0, "radius": 50.0})).unwrap_err();
        assert!(
            err_pixel_larger.contains("pixel size must not be larger than"),
            "expected pixel size larger error, got: {err_pixel_larger}"
        );

        // 17. Clustered points with far outlier must succeed with default parameters
        let mut outlier_ds = Dataset::new("outlier_ds", GeometryType::Point, vec![]);
        outlier_ds.crs = Some(Crs::from_epsg(32618).unwrap());
        outlier_ds.push(Some(Geometry::Point(tr.to_lonlat(Coord::new(500000.0, 4428000.0)).unwrap())), vec![]);
        outlier_ds.push(Some(Geometry::Point(tr.to_lonlat(Coord::new(500010.0, 4428010.0)).unwrap())), vec![]);
        outlier_ds.push(Some(Geometry::Point(tr.to_lonlat(Coord::new(500020.0, 4428020.0)).unwrap())), vec![]);
        outlier_ds.push(Some(Geometry::Point(tr.to_lonlat(Coord::new(500030.0, 4428030.0)).unwrap())), vec![]);
        outlier_ds.push(Some(Geometry::Point(tr.to_lonlat(Coord::new(550000.0, 4478000.0)).unwrap())), vec![]);
        let m_outlier = Map(HashMap::from([("outlier_ds".to_string(), outlier_ds)]));
        let res_outlier = crate::run("kernelDensity", &m_outlier, &serde_json::json!({"input": "outlier_ds"}));
        assert!(res_outlier.is_ok(), "default run with outlier should succeed: {:?}", res_outlier.err());

        // 18. Default run where Silverman radius is bumped to cell_size does not clip grid extent
        let mut bump_ds = Dataset::new("bump_ds", GeometryType::Point, vec![]);
        bump_ds.crs = Some(Crs::from_epsg(32618).unwrap());
        bump_ds.push(Some(Geometry::Point(tr.to_lonlat(Coord::new(500000.0, 4428000.0)).unwrap())), vec![]);
        bump_ds.push(Some(Geometry::Point(tr.to_lonlat(Coord::new(500005.0, 4428005.0)).unwrap())), vec![]);
        bump_ds.push(Some(Geometry::Point(tr.to_lonlat(Coord::new(500010.0, 4428010.0)).unwrap())), vec![]);
        bump_ds.push(Some(Geometry::Point(tr.to_lonlat(Coord::new(550000.0, 4478000.0)).unwrap())), vec![]);
        let m_bump = Map(HashMap::from([("bump_ds".to_string(), bump_ds)]));
        let res_bump = crate::run("kernelDensity", &m_bump, &serde_json::json!({"input": "bump_ds"})).unwrap();
        let bytes_bump = extract_raster_bytes(res_bump.output);
        let r_bump = rustgis_data::raster::geotiff::read(std::io::Cursor::new(&bytes_bump), "b").unwrap();
        let left_point_x = 500000.0;
        let grid_min_x = r_bump.transform.origin_x;
        assert!(
            left_point_x - grid_min_x >= r_bump.transform.pixel_width * 0.99,
            "grid must not clip at edge: {grid_min_x} vs {left_point_x}"
        );

        // 19. pixelSize larger than automatic Silverman radius raises radius to pixelSize instead of erroring
        let res_auto_bump = crate::run("kernelDensity", &m_outlier, &serde_json::json!({"input": "outlier_ds", "pixelSize": 1000.0}));
        assert!(res_auto_bump.is_ok(), "pixelSize > auto radius should succeed by bumping radius: {:?}", res_auto_bump.err());
    }

    #[test]
    fn xy_table_to_point_places_rows_by_their_coordinates() {
        let fields = vec![Field::new("name", FieldType::Text), Field::new("lon", FieldType::Text), Field::new("lat", FieldType::Double)];
        let mut stops = Dataset::new("stops", GeometryType::None, fields);
        for (name, lon, lat) in [
            // A decimal comma, a decimal point with spaces around, and an integer in text.
            ("comma", Value::Text("-74,08".into()), Value::Float(4.6)),
            ("point", Value::Text(" -75.5 ".into()), Value::Float(6.25)),
            ("whole", Value::Text("-76".into()), Value::Int(3)),
            // No number, two commas, nothing at all, and off the globe: rows without a point.
            ("words", Value::Text("n/a".into()), Value::Float(1.0)),
            ("commas", Value::Text("1,234,5".into()), Value::Float(1.0)),
            ("empty", Value::Null, Value::Null),
            ("far", Value::Text("200".into()), Value::Float(0.0)),
        ] {
            stops.push(None, vec![Value::Text(name.into()), lon, lat]);
        }
        // The fields offered for coordinates: those holding numbers, as text or not.
        assert_eq!([0, 1, 2, 3].map(|i| holds_coordinates(&stops, i)), [false, true, true, false]);
        // A single comma is the decimal separator, never one for thousands.
        assert_eq!(["1,234", "1234,567", "1,234.5"].map(|t| coordinate(&Value::Text(t.into()))), [Some(1.234), Some(1234.567), None]);
        // Web Mercator metres: 10° east on the equator.
        let fields = vec![Field::new("e", FieldType::Double), Field::new("n", FieldType::Double)];
        let mut metres = Dataset::new("metres", GeometryType::None, fields);
        metres.push(None, vec![Value::Float(1_113_194.9), Value::Float(0.0)]);
        let m = Map(HashMap::from([("stops".to_string(), stops), ("metres".to_string(), metres)]));
        let run = |p: serde_json::Value| crate::run("xyTableToPoint", &m, &p);

        let r = run(serde_json::json!({"input": "stops", "x": "lon", "y": "lat"})).unwrap();
        assert_eq!(r.messages.len(), 2, "{:?}", r.messages);
        let out = layer(r);
        assert_eq!((out.name.as_str(), out.geometry_type), ("stops_Points", GeometryType::Point));
        assert_eq!((out.fields.len(), out.features.len()), (3, 7));
        let points: Vec<Option<(f64, f64)>> = out
            .features
            .iter()
            .map(|f| match &f.geometry {
                Some(Geometry::Point(c)) => Some((c.x, c.y)),
                _ => None,
            })
            .collect();
        assert_eq!(points, [Some((-74.08, 4.6)), Some((-75.5, 6.25)), Some((-76.0, 3.0)), None, None, None, None]);
        assert_eq!(out.value(3, 0), &Value::Text("words".into()), "rows without a point keep their attributes");

        // Another coordinate system, and the output's name.
        let out = layer(run(serde_json::json!({"input": "metres", "x": "e", "y": "n", "crs": "EPSG:3857", "output": "East"})).unwrap());
        assert_eq!(out.name, "East");
        match out.features.first().and_then(|f| f.geometry.as_ref()) {
            Some(Geometry::Point(c)) => assert!((c.x - 10.0).abs() < 1e-6 && c.y.abs() < 1e-6, "{c:?}"),
            g => panic!("{g:?}"),
        }

        // Metres read as longitude/latitude, fields without numbers, and junk parameters.
        let e = run(serde_json::json!({"input": "metres", "x": "e", "y": "n"})).unwrap_err();
        assert!(e.contains("coordinate system"), "{e}");
        let e = run(serde_json::json!({"input": "stops", "x": "name", "y": "lat"})).unwrap_err();
        assert!(e.contains("no row has numbers"), "{e}");
        for p in [
            serde_json::json!({"input": "stops", "x": "nope", "y": "lat"}),
            serde_json::json!({"input": "stops", "x": "lon"}),
            serde_json::json!({"input": "stops", "x": "lon", "y": "lat", "crs": "EPSG:99999999"}),
            serde_json::json!({"input": "stops", "x": "lon", "y": "lat", "crs": "somewhere"}),
            serde_json::json!({"input": "nope", "x": "lon", "y": "lat"}),
        ] {
            assert!(run(p.clone()).is_err(), "{p}");
        }
    }

    #[test]
    fn hostile_params_never_panic() {
        let m = inputs();
        let junk = [
            serde_json::json!({"input": "pts", "x": "v", "y": "v", "crs": "((", "output": 7}),
            serde_json::json!(null),
            serde_json::json!({}),
            serde_json::json!({"input": 5, "distance": "x"}),
            serde_json::json!({"input": "polys", "distance": 1e308, "tolerance": -1, "field": "nope", "where": "(((", "clip": "pts"}),
            serde_json::json!({"input": "pts", "polygons": "pts", "points": "polys", "select": "pts", "distance": -5}),
            serde_json::json!({"target": "pts", "join": "polys", "match": "Closest", "distance": -1, "merge": 5, "distanceField": 3}),
            serde_json::json!({"target": "polys", "join": "pts", "match": "Within a distance", "distance": 1e308, "units": "Feet", "merge": "[{\"field\": 7}]"}),
            serde_json::json!({"target": "pts", "join": "polys", "match": "Largest overlap", "merge": [{"field": "k", "rule": "sum"}, null, "x"]}),
            serde_json::json!({"target": "polys", "join": "polys", "match": "Have their center in", "merge": [{"field": "k", "rule": "join", "delimiter": null}], "distanceField": ""}),
            serde_json::json!({"input": "polys", "overlay": "polys"}),
            serde_json::json!({"input": "pts", "overlay": "pts", "output": ""}),
            serde_json::json!({"input": 3, "overlay": ["polys"]}),
            serde_json::json!({"input": "polys", "update": "polys", "erase": "pts"}),
            serde_json::json!({"input": "pts", "split": "polys", "field": "k", "output": "\u{0}"}),
            serde_json::json!({"input": "polys", "split": "polys", "field": 5, "update": 2}),
            serde_json::json!({"input": "pts", "near": "pts", "distance": -0.0, "units": "Feet"}),
            serde_json::json!({"input": "polys", "near": "pts", "distance": 1e308}),
            serde_json::json!({"input": "pts", "statistics": [{"field": "v", "statistic": "P0"}, 7], "case": ["k", 3], "separator": 5}),
            serde_json::json!({"input": "polys", "statistics": "[{\"field\": \"k\", \"statistic\": \"CONCATENATE\"}]", "case": "k,k"}),
            serde_json::json!({"input": "pts", "statistics": {"field": "v"}, "case": {"k": 1}, "field": ["v"]}),
            serde_json::json!({"input": "pts", "fields": ["k", 3], "pivot": "k", "value": "v", "statistic": "P100"}),
            serde_json::json!({"input": "polys", "fields": {"k": 1}, "pivot": 5, "value": ["v"], "statistic": 7, "separator": []}),
            serde_json::json!({"input": "pts", "fields": "", "pivot": "v", "value": "k", "statistic": "concatenate", "output": ""}),
            serde_json::json!({"input": "polys", "polygons": "polys", "points": "pts", "field": "k", "statistics": [{"field": "k", "statistic": "CONCATENATE"}, {"field": "k", "statistic": "MODE"}], "separator": "\u{0}"}),
            serde_json::json!({"input": "pts", "polygons": "polys", "points": "pts", "statistics": [{"field": "v", "statistic": "P99"}, {"field": "v", "statistic": "CV"}, {"field": "v", "statistic": "CONCATENATE"}], "separator": ""}),
            serde_json::json!({"input": "polys", "polygons": "polys", "points": "pts", "statistics": [{"field": "k", "statistic": "SUM"}, {"field": "v", "statistic": "MEDIAN"}], "separator": null}),
            serde_json::json!({"input": "polys", "polygons": "polys", "points": "pts", "statistics": "[{\"field\": \"v\", \"statistic\": \"LAST\"}]", "separator": {"a": 1}}),
            serde_json::json!({"zones": "polys", "zoneFields": ["k", 3], "classes": "polys", "classFields": "k", "sumFields": {"v": 1}}),
            serde_json::json!({"zones": "polys", "zoneFields": "k", "classes": "polys", "classFields": ["k", "K"], "sumFields": ["v", "k"]}),
            serde_json::json!({"zones": "pts", "zoneFields": "k", "classes": "polys", "sumFields": "v", "output": 7}),
            serde_json::json!({"zones": "polys", "zoneFields": "k", "classes": "polys", "classFields": "k", "output": ""}),
        ];
        for t in TOOLS {
            for p in &junk {
                // A panic is caught and becomes this error, which would pass unnoticed.
                let r = crate::run(t.id, &m, p);
                assert!(!matches!(&r, Err(e) if e.ends_with("failed unexpectedly")), "{} panicked on {p}", t.id);
            }
        }
    }

    /// The map measure takes rings as the map draws them: a band around the world stored as a
    /// rectangle from −180° to 180° has its whole area, and a ring stored around a pole with a jump
    /// across the antimeridian, which the map draws flat, has none.
    #[test]
    fn map_area_measures_rings_as_the_map_draws_them() {
        let ring = |pts: &[(f64, f64)]| Geometry::Polygon(vec![vec![pts.iter().map(|&(x, y)| Coord::new(x, y)).collect()]]);
        let r2 = rustgis_geom::EARTH_RADIUS_M * rustgis_geom::EARTH_RADIUS_M;
        let band = map_area_m2(&ring(&[(-180.0, 0.0), (180.0, 0.0), (180.0, 60.0), (-180.0, 60.0), (-180.0, 0.0)]));
        let expected = r2 * std::f64::consts::TAU * 60f64.to_radians().sin();
        assert!((band / expected - 1.0).abs() < 1e-12, "{band} m², not {expected}");
        let around_pole = map_area_m2(&ring(&[(0.0, 60.0), (90.0, 60.0), (180.0, 60.0), (-90.0, 60.0), (0.0, 60.0)]));
        // Zero, up to rounding on the scale of the Earth's radius squared.
        assert!(around_pole.abs() < 1.0, "{around_pole} m²");
    }

    /// Parcels and zones for Intersect: `p1` is a 2° square that zone `z1` covers the right half of;
    /// `z2` overlaps `z1` but only touches `p1` along its right edge, and `p2` along its left.
    /// Both layers have an `id` field.
    fn parcels_and_zones() -> Map {
        let text = |t: &str| Value::Text(t.into());
        let fields = |second: &str| vec![Field::new("id", FieldType::Text), Field::new(second, FieldType::Text)];
        let mut parcels = Dataset::new("parcels", GeometryType::Polygon, fields("use"));
        parcels.push(Some(sq(0.0, 0.0, 2.0)), vec![text("p1"), text("home")]);
        parcels.push(Some(sq(4.0, 0.0, 1.0)), vec![text("p2"), text("shop")]);
        let mut zones = Dataset::new("zones", GeometryType::Polygon, fields("zone"));
        zones.push(Some(sq(1.0, 0.0, 2.0)), vec![text("z1"), text("R1")]);
        zones.push(Some(sq(2.0, 0.0, 2.0)), vec![text("z2"), text("C2")]);
        Map(HashMap::from([("parcels".to_string(), parcels), ("zones".to_string(), zones)]))
    }

    #[test]
    fn intersect_keeps_the_overlap_with_both_layers_attributes() {
        let m = parcels_and_zones();
        let out = layer(crate::run("intersect", &m, &serde_json::json!({"input": "parcels", "overlay": "zones"})).unwrap());
        assert_eq!(out.name, "parcels_Intersect");
        assert_eq!(out.geometry_type, GeometryType::Polygon);
        let names: Vec<&str> = out.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["id", "use", "id_2", "zone"], "the zones' id is taken");
        // Only p1 and z1 overlap; touching along an edge makes no feature.
        assert_eq!(out.features.len(), 1);
        let f = &out.features[0];
        let text = |v: &[&str]| v.iter().map(|t| Value::Text((*t).into())).collect::<Vec<_>>();
        assert_eq!(f.attrs, text(&["p1", "home", "z1", "R1"]));
        let e = f.geometry.as_ref().unwrap().extent();
        for (got, want) in [(e.min_x, 1.0), (e.max_x, 2.0), (e.min_y, 0.0), (e.max_y, 2.0)] {
            assert!((got - want).abs() < 1e-9, "{e:?}");
        }
    }

    #[test]
    fn intersect_unions_an_overlay_feature_whose_parts_overlap() {
        // One zone of two overlapping squares (x 0–2 and 1–3), as a file can hold it.
        let ring =
            |x: f64| vec![Coord::new(x, 0.0), Coord::new(x + 2.0, 0.0), Coord::new(x + 2.0, 2.0), Coord::new(x, 2.0), Coord::new(x, 0.0)];
        let mut zones = Dataset::new("zones", GeometryType::Polygon, vec![]);
        zones.push(Some(Geometry::Polygon(vec![vec![ring(0.0)], vec![ring(1.0)]])), vec![]);
        let mut parcels = Dataset::new("parcels", GeometryType::Polygon, vec![]);
        parcels.push(Some(sq(-1.0, 0.0, 5.0)), vec![]);
        let m = Map(HashMap::from([("zones".to_string(), zones), ("parcels".to_string(), parcels)]));
        let out = layer(crate::run("intersect", &m, &serde_json::json!({"input": "parcels", "overlay": "zones"})).unwrap());
        assert_eq!(out.features.len(), 1);
        // The part is the zone's whole outline, x 0–3, once: the overlap is neither lost nor doubled.
        use geo::Area;
        let part = to_multipolygon(out.features[0].geometry.as_ref().unwrap()).unwrap();
        let outline = vec![Coord::new(0.0, 0.0), Coord::new(3.0, 0.0), Coord::new(3.0, 2.0), Coord::new(0.0, 2.0), Coord::new(0.0, 0.0)];
        let want = to_multipolygon(&Geometry::Polygon(vec![vec![outline]])).unwrap();
        assert_eq!(part.0.len(), 1, "{part:?}");
        assert!((part.unsigned_area() - want.unsigned_area()).abs() < 1e-6 * want.unsigned_area(), "{part:?}");
    }

    #[test]
    fn intersect_splits_points_by_each_polygon_they_fall_in() {
        let m = inputs();
        let out = layer(crate::run("intersect", &m, &serde_json::json!({"input": "pts", "overlay": "polys"})).unwrap());
        assert_eq!(out.geometry_type, GeometryType::Point);
        // (0.5, 0.5) is in the first square only, (1.5, 1.5) in both overlapping ones, (50, 50) in none.
        let rows: Vec<(f64, &str)> = out
            .features
            .iter()
            .map(|f| (f.attrs[0].as_f64().unwrap(), if let Value::Text(t) = &f.attrs[1] { t.as_str() } else { "" }))
            .collect();
        assert_eq!(rows, [(1.0, "a"), (2.0, "a"), (2.0, "a")]);
    }

    /// Summarize Within, through its index, counts what testing every point against every polygon
    /// counts: random triangles and squares with holes, overlapping and of very different sizes
    /// (one covering them all), a polygon feature without geometry, points on vertices (the edge
    /// counts), several-point features, points outside every polygon, past the latitude limit or
    /// with non-finite coordinates, and a selection of points.
    #[test]
    fn count_points_matches_testing_every_point_against_every_polygon() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut rand = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        let c = Coord::new;
        let mut polys = Dataset::new("polys", GeometryType::Polygon, vec![]);
        let mut vertices = Vec::new();
        for i in 0..300 {
            let (x, y, size) = (rand() * 40.0 - 20.0, rand() * 40.0 - 20.0, 0.1 + rand() * 3.0);
            let g = if i % 3 == 0 {
                let hole = vec![
                    c(x + size * 0.25, y + size * 0.25),
                    c(x + size * 0.25, y + size * 0.75),
                    c(x + size * 0.75, y + size * 0.75),
                    c(x + size * 0.25, y + size * 0.25),
                ];
                let outer = vec![c(x, y), c(x + size, y), c(x + size, y + size), c(x, y + size), c(x, y)];
                Geometry::Polygon(vec![vec![outer, hole]])
            } else {
                let (a, b) = (c(x + size * rand(), y), c(x + size, y + size * rand()));
                Geometry::Polygon(vec![vec![vec![c(x, y + size), a, b, c(x, y + size)]]])
            };
            if i % 7 == 0 {
                vertices.extend(match &g {
                    Geometry::Polygon(p) => p.iter().flatten().flatten().copied().collect::<Vec<_>>(),
                    _ => Vec::new(),
                });
            }
            polys.push(Some(g), vec![]);
        }
        polys.push(Some(sq(-30.0, -30.0, 60.0)), vec![]);
        // Reaching past the map's latitude limit, where every latitude beyond it is the same.
        polys.push(Some(sq(40.0, 80.0, 6.0)), vec![]);
        polys.push(None, vec![]);
        let mut pts = Dataset::new("pts", GeometryType::Point, vec![]);
        for _ in 0..3000 {
            pts.push(Some(Geometry::Point(c(rand() * 50.0 - 25.0, rand() * 50.0 - 25.0))), vec![]);
        }
        for v in vertices {
            pts.push(Some(Geometry::Point(v)), vec![]);
        }
        for _ in 0..100 {
            let (x, y) = (rand() * 40.0 - 20.0, rand() * 40.0 - 20.0);
            pts.push(Some(Geometry::MultiPoint(vec![c(x, y), c(x + 2.0, y + 1.0), c(x - 1.0, y + 3.0)])), vec![]);
        }
        // Beyond the latitude limit, and features with non-finite coordinates.
        pts.push(Some(Geometry::Point(c(42.0, 89.0))), vec![]);
        pts.push(Some(Geometry::MultiPoint(vec![c(f64::NAN, 1.0), c(41.0, 81.0)])), vec![]);
        pts.push(Some(Geometry::MultiPoint(vec![c(f64::INFINITY, f64::NAN), c(-29.0, -29.0)])), vec![]);
        pts.push(Some(Geometry::Point(c(f64::NAN, f64::NAN))), vec![]);
        pts.push(None, vec![]);
        let selected: Vec<usize> = (0..pts.features.len()).filter(|r| r % 5 != 0).collect();
        // Enough polygons for the index to build its grid, not fall back to testing them all.
        let extents = polys.features.iter().filter_map(|f| f.geometry.as_ref().and_then(to_multipolygon)).map(|mp| merc_extent(&mp));
        assert!(ExtentIndex::new(extents).cell_size().is_some());
        // Every selected point against every polygon.
        let expected: Vec<i64> = polys
            .features
            .iter()
            .map(|f| {
                let Some(mp) = f.geometry.as_ref().and_then(to_multipolygon) else { return 0 };
                selected.iter().filter_map(|r| pts.features.get(*r)?.geometry.as_ref()).filter(|g| mp.intersects(&to_geo(g))).count() as i64
            })
            .collect();
        let m = Selected(
            Map(HashMap::from([("polys".to_string(), polys), ("pts".to_string(), pts)])),
            HashMap::from([("pts".to_string(), selected)]),
        );
        let out = layer(crate::run("countPoints", &m, &serde_json::json!({"polygons": "polys", "points": "pts"})).unwrap());
        let fi = out.field_index("point_count").unwrap();
        let counts: Vec<i64> = (0..out.features.len())
            .map(|r| match out.value(r, fi) {
                Value::Int(n) => *n,
                v => panic!("{v:?}"),
            })
            .collect();
        assert_eq!(counts, expected);
        assert!(expected.iter().filter(|n| **n > 0).count() > 200, "most polygons hold some points: {expected:?}");
    }

    /// Deterministic random masks and mixed inputs exercise an actual grid, overlapping masks,
    /// holes, multipart features, empty geometry, boundary contacts and projected coordinates.
    fn indexed_overlay_inputs() -> Map {
        let mut seed = 0x1142_9e37_79b9u64;
        let mut rand = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        let mut masks = Dataset::new("masks", GeometryType::Polygon, vec![]);
        for i in 0..80 {
            let (x, y, size) = (rand() * 12.0, rand() * 12.0, 0.2 + rand() * 2.0);
            let mut g = sq(x, y, size);
            if let Geometry::Polygon(polys) = &mut g {
                if i % 3 == 0 {
                    let Geometry::Polygon(hole) = sq(x + size * 0.25, y + size * 0.25, size * 0.5) else {
                        panic!("square fixture must be a polygon")
                    };
                    polys[0].push(hole[0][0].clone());
                } else if i % 3 == 1 {
                    let Geometry::Polygon(extra) = sq(x + size * 0.5, y, size) else { panic!("square fixture must be a polygon") };
                    polys.extend(extra);
                }
            }
            masks.push(Some(g), vec![]);
        }
        masks.push(Some(sq(40.0, 80.0, 6.0)), vec![]);
        masks.push(Some(sq(190.0, 0.0, 2.0)), vec![]);
        masks.push(Some(sq(0.0, 0.0, 1.0)), vec![]);
        masks.push(None, vec![]);
        let mut features = Dataset::new("features", GeometryType::Polygon, vec![Field::new("row", FieldType::Integer)]);
        for i in 0..180 {
            let (x, y, size) = (rand() * 16.0 - 2.0, rand() * 16.0 - 2.0, rand() * 2.0 + 0.1);
            let g = match i % 4 {
                0 => sq(x, y, size),
                1 => Geometry::Line(vec![vec![Coord::new(x, y), Coord::new(x + size, y + size)]]),
                2 => Geometry::Point(Coord::new(x, y)),
                _ => Geometry::MultiPoint(vec![Coord::new(x, y), Coord::new(x + size, y)]),
            };
            features.push(Some(g), vec![Value::Int(i)]);
        }
        for g in [
            Some(Geometry::Point(Coord::new(42.0, 89.0))),
            Some(Geometry::Point(Coord::new(-169.0, 1.0))),
            Some(Geometry::Point(Coord::new(0.0, 0.0))),
            Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 0.0)]])),
            Some(sq(0.0, 0.0, 1.0)),
            Some(sq(-3.0, -3.0, 22.0)),
            Some(Geometry::Polygon(vec![])),
            None,
        ] {
            features.push(g, vec![Value::Int(features.features.len() as i64)]);
        }
        Map(HashMap::from([("masks".into(), masks), ("features".into(), features)]))
    }

    /// Compare polygon coverage rather than vertex order or the overlay's rounding; other
    /// geometries must have the same parts. The fixture spans nearly the whole projected world:
    /// its whole-mask integer overlay rounds to centimetres, so allow a 10 cm boundary band
    /// for symmetric-difference area and 10 cm between line vertices. Points never move.
    fn assert_same_clip_geometry(got: &Geometry, want: &Geometry) {
        use geo::{Area, Euclidean, Length};
        if let (Some(a), Some(b)) = (to_multipolygon(got), to_multipolygon(want)) {
            let perimeter: f64 = [&a, &b]
                .into_iter()
                .flat_map(|m| m.0.iter())
                .flat_map(|p| std::iter::once(p.exterior()).chain(p.interiors()))
                .map(|ring| Euclidean.length(ring))
                .sum();
            let error = a.xor(&b).unsigned_area();
            assert!(error <= perimeter * 0.1, "area error {error}: {got:?} vs {want:?}");
        } else {
            match (got, want) {
                (Geometry::Point(_), Geometry::Point(_)) | (Geometry::MultiPoint(_), Geometry::MultiPoint(_)) => {
                    assert_eq!(got, want);
                    return;
                }
                (Geometry::Line(a), Geometry::Line(b)) => {
                    assert_eq!(a.len(), b.len(), "line parts differ");
                    for (a, b) in a.iter().zip(b) {
                        assert_eq!(a.len(), b.len(), "line vertices differ");
                    }
                }
                _ => panic!("geometry variants differ: {got:?} vs {want:?}"),
            }
            let a = to_geo(got);
            let b = to_geo(want);
            use geo::CoordsIter;
            let a: Vec<_> = a.coords_iter().collect();
            let b: Vec<_> = b.coords_iter().collect();
            assert_eq!(a.len(), b.len(), "{got:?} vs {want:?}");
            for (a, b) in a.iter().zip(&b) {
                assert!((a.x - b.x).abs() < 0.1 && (a.y - b.y).abs() < 0.1, "{got:?} vs {want:?}");
            }
        }
    }

    #[test]
    fn indexed_clip_matches_the_whole_mask() {
        let m = indexed_overlay_inputs();
        let masks = &m.0["masks"];
        assert!(
            ExtentIndex::new(masks.features.iter().filter_map(|f| f.geometry.as_ref().and_then(to_multipolygon)).map(|m| merc_extent(&m)))
                .cell_size()
                .is_some()
        );
        for selection in [None, Some((0..80).filter(|i| i % 3 != 0).collect()), Some(vec![82]), Some(vec![83])] {
            let mut selected = HashMap::new();
            if let Some(s) = selection {
                selected.insert("masks".into(), s);
                selected.insert("features".into(), (0..188).filter(|i| i % 5 != 0).collect());
            }
            let inp = Selected(indexed_overlay_inputs(), selected);
            let p = serde_json::json!({"input":"features", "clip":"masks"});
            let masks = &inp.0.0["masks"];
            let polys: Vec<_> = rows(&inp, &p, "clip", masks)
                .iter()
                .filter_map(|r| masks.features[*r].geometry.as_ref().and_then(to_multipolygon))
                .collect();
            let whole = union_all(polys.iter().flat_map(|m| m.0.iter()));
            let features = &inp.0.0["features"];
            let expected: Vec<_> = rows(&inp, &p, "input", features)
                .into_iter()
                .filter_map(|r| {
                    let f = &features.features[r];
                    let g = part(&dissolved(f.geometry.as_ref()?), &whole, true)?;
                    Some((f.attrs.clone(), g))
                })
                .collect();
            let out = layer(crate::run("clip", &inp, &p).unwrap());
            assert_eq!(out.fields, features.fields);
            assert_eq!(
                out.features.iter().map(|f| &f.attrs).collect::<Vec<_>>(),
                expected.iter().map(|(attrs, _)| attrs).collect::<Vec<_>>()
            );
            for (f, (attrs, g)) in out.features.iter().zip(&expected) {
                assert_eq!(&f.attrs, attrs);
                assert_same_clip_geometry(f.geometry.as_ref().unwrap(), g);
            }
        }
    }

    #[test]
    fn indexed_clip_handles_repeated_and_changing_neighbourhoods() {
        let mut masks = Dataset::new("masks", GeometryType::Polygon, vec![]);
        for i in 0..20 {
            masks.push(Some(sq(f64::from(i) * 2.0, 0.0, 1.0)), vec![]);
        }
        masks.push(Some(sq(0.2, 0.0, 1.0)), vec![]);
        masks.push(Some(sq(4.2, 0.0, 1.0)), vec![]);
        // Repeated multi-mask hits, a different multi-mask set, a singleton, a miss, and the
        // earlier sets again: neither cached unions nor single masks may leak across inputs.
        let xs = [-100.0, 0.5, 0.6, 4.5, 8.5, 100.0, 0.7, 4.7, 4.8];
        let mut pts = Dataset::new("pts", GeometryType::Point, vec![Field::new("x", FieldType::Double)]);
        for x in xs {
            pts.push(Some(Geometry::Point(Coord::new(x, 0.5))), vec![Value::Float(x)]);
        }
        let m = Map(HashMap::from([("masks".into(), masks), ("pts".into(), pts)]));
        let out = layer(crate::run("clip", &m, &serde_json::json!({"input":"pts", "clip":"masks"})).unwrap());
        let kept: Vec<_> = out.features.iter().map(|f| f.attrs[0].as_f64().unwrap()).collect();
        assert_eq!(kept, [0.5, 0.6, 4.5, 8.5, 0.7, 4.7, 4.8]);
        for f in &out.features {
            assert_eq!(f.geometry, Some(Geometry::Point(Coord::new(f.attrs[0].as_f64().unwrap(), 0.5))));
        }
    }

    #[test]
    fn indexed_clip_handles_alternating_neighbourhoods_and_cache_eviction() {
        let mut masks = Dataset::new("masks", GeometryType::Polygon, vec![]);
        for i in 0..12 {
            let x = f64::from(i) * 2.0;
            masks.push(Some(sq(x, 0.0, 1.0)), vec![]);
            masks.push(Some(sq(x + 0.2, 0.0, 1.0)), vec![]);
        }
        let mut pts = Dataset::new("pts", GeometryType::Point, vec![Field::new("x", FieldType::Double)]);
        // Revisit alternating sets, then overflow the eight-entry cache and revisit evicted sets.
        for i in [0, 1, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 0, 1, 11, 0] {
            let x = f64::from(i) * 2.0 + 0.5;
            pts.push(Some(Geometry::Point(Coord::new(x, 0.5))), vec![Value::Float(x)]);
        }
        let expected = pts.features.clone();
        let m = Map(HashMap::from([("masks".into(), masks), ("pts".into(), pts)]));
        let out = layer(crate::run("clip", &m, &serde_json::json!({"input":"pts", "clip":"masks"})).unwrap());
        assert_eq!(out.features.len(), expected.len());
        for (got, want) in out.features.iter().zip(expected) {
            assert_eq!(got.geometry, want.geometry);
            assert_eq!(got.attrs, want.attrs);
        }
    }

    #[test]
    fn projected_extent_matches_full_geometry_projection() {
        let m = indexed_overlay_inputs();
        for ds in m.0.values() {
            for g in ds.features.iter().filter_map(|f| f.geometry.as_ref()) {
                assert_eq!(projected_extent(g), merc_extent(&to_geo(g)), "{g:?}");
            }
        }
        for g in [
            Geometry::MultiPoint(vec![]),
            Geometry::Line(vec![vec![]]),
            Geometry::Polygon(vec![]),
            Geometry::MultiPoint(vec![Coord::new(f64::NAN, 10.0), Coord::new(200.0, 90.0), Coord::new(-220.0, -90.0)]),
            Geometry::Point(Coord::new(f64::INFINITY, f64::NEG_INFINITY)),
        ] {
            assert_eq!(projected_extent(&g), merc_extent(&to_geo(&g)), "{g:?}");
        }
    }

    #[test]
    fn indexed_clip_discards_masks_with_no_filled_area() {
        let mut masks = Dataset::new("masks", GeometryType::Polygon, vec![]);
        masks.push(
            Some(Geometry::Polygon(vec![vec![vec![
                Coord::new(0.0, 0.0),
                Coord::new(1.0, 0.0),
                Coord::new(2.0, 0.0),
                Coord::new(0.0, 0.0),
            ]]])),
            vec![],
        );
        masks.push(Some(sq(10.0, 0.0, 1.0)), vec![]);
        let mut pts = Dataset::new("pts", GeometryType::Point, vec![]);
        pts.push(Some(Geometry::Point(Coord::new(0.5, 0.0))), vec![]);
        pts.push(Some(Geometry::Line(vec![vec![Coord::new(0.5, 0.0), Coord::new(1.5, 0.0)]])), vec![]);
        pts.push(Some(Geometry::MultiPoint(vec![Coord::new(0.5, 0.0), Coord::new(10.5, 0.5)])), vec![]);
        let m = Map(HashMap::from([("masks".into(), masks), ("pts".into(), pts)]));
        let out = layer(crate::run("clip", &m, &serde_json::json!({"input":"pts", "clip":"masks"})).unwrap());
        assert_eq!(out.features.len(), 1);
        assert_eq!(out.features[0].geometry, Some(Geometry::Point(Coord::new(10.5, 0.5))));
    }

    #[test]
    fn indexed_clip_keeps_lines_that_snap_to_mask_edges() {
        let mut masks = Dataset::new("masks", GeometryType::Polygon, vec![]);
        masks.push(Some(sq(0.0, 0.0, 1.0)), vec![]);
        let mut lines = Dataset::new("lines", GeometryType::Polyline, vec![]);
        for y in [-1e-12, -0.01] {
            lines.push(Some(Geometry::Line(vec![vec![Coord::new(0.1, y), Coord::new(0.9, y)]])), vec![]);
        }
        let whole = union_all(to_multipolygon(masks.features[0].geometry.as_ref().unwrap()).unwrap().0.iter());
        let expected: Vec<_> = lines.features.iter().filter_map(|f| part(f.geometry.as_ref().unwrap(), &whole, true)).collect();
        // The integer overlay snaps the near line onto the edge, but leaves the far line out.
        assert_eq!(expected.len(), 1);
        let m = Map(HashMap::from([("masks".into(), masks), ("lines".into(), lines)]));
        let out = layer(crate::run("clip", &m, &serde_json::json!({"input":"lines", "clip":"masks"})).unwrap());
        assert_eq!(out.features.iter().filter_map(|f| f.geometry.clone()).collect::<Vec<_>>(), expected);
    }

    /// Distance selection bounds each selector, including its overlapping multipoint discs,
    /// independently of other selectors. Exhaustion is an error, including when inverted.
    #[test]
    fn location_selection_bounds_accumulated_buffer_outlines() {
        let point = Coord::new(0.0, 0.0);
        let vertices = geodesic_buffer(&Geometry::Point(point), 1000.0).unwrap().coord_count();
        let mut selectors = Dataset::new("selectors", GeometryType::Point, vec![]);
        for c in [point, Coord::new(0.5, 0.0)] {
            selectors.push(Some(Geometry::Point(c)), vec![]);
        }
        let mut features = Dataset::new("features", GeometryType::Point, vec![]);
        features.push(Some(Geometry::Point(point)), vec![]);
        let mut m = Map(HashMap::from([("selectors".into(), selectors), ("features".into(), features)]));
        for invert in ["No", "Yes"] {
            let p = serde_json::json!({"input":"features", "select":"selectors", "distance":1, "invert":invert});
            let result = select_by_location_with_limits(&m, &p, 2 * vertices, MAX_BUFFER_BYTES).unwrap();
            let Output::Selection { rows, .. } = result.output else { panic!("expected selection") };
            assert_eq!(rows, if invert == "No" { vec![0] } else { vec![] });
            assert!(select_by_location_with_limits(&m, &p, vertices, MAX_BUFFER_BYTES).is_ok());
        }
        // A zero-distance predicate does not allocate buffer outlines.
        let p = serde_json::json!({"input":"features", "select":"selectors", "distance":0});
        assert!(select_by_location_with_limits(&m, &p, 0, MAX_BUFFER_BYTES).is_ok());
        m.0.get_mut("selectors").unwrap().features.clear();
        m.0.get_mut("selectors").unwrap().push(Some(Geometry::MultiPoint(vec![point; 3])), vec![]);
        let p = serde_json::json!({"input":"features", "select":"selectors", "distance":1});
        assert!(select_by_location_with_limits(&m, &p, 2 * vertices, MAX_BUFFER_BYTES).unwrap_err().contains("vertex budget"));
        m.0.get_mut("selectors").unwrap().features.clear();
        for _ in 0..2 {
            m.0.get_mut("selectors").unwrap().push(Some(Geometry::MultiPoint(vec![point; 2])), vec![]);
        }
        assert!(select_by_location_with_limits(&m, &p, 4 * vertices, MAX_BUFFER_BYTES).is_ok());
        assert!(select_by_location_with_limits(&m, &p, 3 * vertices, MAX_BUFFER_BYTES).is_ok());
    }

    /// A byte budget charges retained selector geometries, row capacity, extent scratch and
    /// the spatial index. Selector complexity remains per feature, on either inversion path.
    #[test]
    fn distance_selection_storage_includes_its_index() {
        let g = Geometry::Point(Coord::new(8.0, 46.0));
        let buffered = geodesic_buffer(&g, 5000.0).unwrap();
        let gg = to_geo(&buffered);
        let ext = merc_extent(&gg);
        let n = 32;
        let before_index =
            n * (selector_heap_bytes(&gg) + std::mem::size_of::<(geo::Geometry<f64>, Extent)>() + std::mem::size_of::<Extent>());
        let index_bytes = ExtentIndex::new((0..n).map(|_| ext)).storage_bytes();
        assert!(index_bytes > 0);
        let mut selectors = Dataset::new("selectors", GeometryType::Point, vec![]);
        for _ in 0..n {
            selectors.push(Some(g.clone()), vec![]);
        }
        let mut points = Dataset::new("points", GeometryType::Point, vec![]);
        points.push(Some(g), vec![]);
        let m = Map(HashMap::from([("points".into(), points), ("selectors".into(), selectors)]));
        for invert in ["No", "Yes"] {
            let p = serde_json::json!({"input":"points", "select":"selectors", "distance":5, "invert":invert});
            assert!(select_by_location_with_limits(&m, &p, buffered.coord_count(), before_index).unwrap_err().contains("memory budget"));
            let output = select_by_location_with_limits(&m, &p, buffered.coord_count(), before_index + index_bytes).unwrap();
            let Output::Selection { rows, .. } = output.output else { panic!("expected selection") };
            assert_eq!(rows, if invert == "No" { vec![0] } else { vec![] });
        }
    }

    /// A small selector set deliberately has no grid. Its bounded-width scan remains linear
    /// in the target count and must not consume the large-grid fallback allowance.
    #[test]
    fn small_distance_selector_sets_keep_their_unindexed_behavior() {
        let mut selectors = Dataset::new("selectors", GeometryType::Point, vec![]);
        for _ in 0..2 {
            selectors.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![]);
        }
        let mut points = Dataset::new("points", GeometryType::Point, vec![]);
        points.push(Some(Geometry::Point(Coord::new(1.0, 1.0))), vec![]);
        let m = Map(HashMap::from([("points".into(), points), ("selectors".into(), selectors)]));
        let p = serde_json::json!({"input":"points", "select":"selectors", "distance":1});
        let result = select_by_location_with_work_limit(&m, &p, MAX_BUFFER_VERTICES, MAX_BUFFER_BYTES, 0).unwrap();
        let Output::Selection { rows, .. } = result.output else { panic!("expected selection") };
        assert!(rows.is_empty());
    }

    /// Clustered selectors overflow the grid's reference allowance. The first target matches
    /// immediately and the second misses every circle; exhaustion discards earlier matches.
    #[test]
    fn distance_selection_bounds_actual_candidate_checks() {
        let centre = Geometry::Point(Coord::new(0.0, 0.0));
        let mut selectors = Dataset::new("selectors", GeometryType::Point, vec![]);
        let count = 2000;
        for _ in 0..count {
            selectors.push(Some(centre.clone()), vec![]);
        }
        let mut points = Dataset::new("points", GeometryType::Point, vec![]);
        points.push(Some(centre), vec![]);
        points.push(Some(Geometry::Point(Coord::new(0.008, 0.008))), vec![]);
        let mut m = Map(HashMap::from([("points".into(), points), ("selectors".into(), selectors)]));
        for invert in ["No", "Yes"] {
            let p = serde_json::json!({"input":"points", "select":"selectors", "distance":1, "invert":invert});
            let result = select_by_location_with_work_limit(&m, &p, MAX_BUFFER_VERTICES, MAX_BUFFER_BYTES, count + 1).unwrap();
            let Output::Selection { rows, .. } = result.output else { panic!("expected selection") };
            assert_eq!(rows, if invert == "No" { vec![0] } else { vec![1] });
            assert!(
                select_by_location_with_work_limit(&m, &p, MAX_BUFFER_VERTICES, MAX_BUFFER_BYTES, count)
                    .unwrap_err()
                    .contains("selector checks")
            );
        }
        let p = serde_json::json!({"input":"points", "select":"selectors", "distance":0});
        assert!(select_by_location_with_work_limit(&m, &p, MAX_BUFFER_VERTICES, MAX_BUFFER_BYTES, 0).is_ok());
        // Seventeen circles fit the grid. Unique candidates on this path consume the same
        // allowance: one early match followed by seventeen misses needs eighteen checks.
        m.0.get_mut("selectors").unwrap().features.truncate(17);
        for invert in ["No", "Yes"] {
            let p = serde_json::json!({"input":"points", "select":"selectors", "distance":1, "invert":invert});
            let result = select_by_location_with_work_limit(&m, &p, MAX_BUFFER_VERTICES, MAX_BUFFER_BYTES, 18).unwrap();
            let Output::Selection { rows, .. } = result.output else { panic!("expected selection") };
            assert_eq!(rows, if invert == "No" { vec![0] } else { vec![1] });
            assert!(
                select_by_location_with_work_limit(&m, &p, MAX_BUFFER_VERTICES, MAX_BUFFER_BYTES, 17)
                    .unwrap_err()
                    .contains("selector checks")
            );
        }
    }

    #[test]
    fn indexed_selection_matches_every_selector_including_buffers() {
        for selection in [None, Some((0..80).filter(|i| i % 3 != 0).collect()), Some(vec![83])] {
            let mut selected = HashMap::new();
            if let Some(s) = selection {
                selected.insert("masks".into(), s);
                // Input selection must not restrict a new location selection.
                selected.insert("features".into(), vec![0]);
            }
            let inp = Selected(indexed_overlay_inputs(), selected);
            for distance in [0.0, 20_000.0] {
                for invert in [false, true] {
                    let p = serde_json::json!({"input":"features", "select":"masks", "distance": distance, "units":"Meters", "invert": if invert { "Yes" } else { "No" }});
                    let masks = &inp.0.0["masks"];
                    let selectors: Vec<_> = rows(&inp, &p, "select", masks)
                        .iter()
                        .filter_map(|r| masks.features[*r].geometry.as_ref())
                        .map(|g| {
                            let b = if distance > 0.0 { geodesic_buffer(g, distance) } else { None };
                            to_geo(b.as_ref().unwrap_or(g))
                        })
                        .collect();
                    let expected: Vec<_> = inp.0.0["features"]
                        .features
                        .iter()
                        .enumerate()
                        .filter_map(|(r, f)| {
                            let gg = to_geo(f.geometry.as_ref()?);
                            (selectors.iter().any(|s| s.intersects(&gg)) != invert).then_some(r)
                        })
                        .collect();
                    let Output::Selection { rows, .. } = crate::run("selectByLocation", &inp, &p).unwrap().output else {
                        panic!("expected selection")
                    };
                    assert_eq!(rows, expected, "distance={distance}, invert={invert}");
                }
            }
        }
    }

    #[test]
    fn extent_index_any_candidate_short_circuits_and_deduplicates() {
        // Every extent spans many cells. A hit must not collect the rest of the grid first;
        // on a miss, repeated references to the same selector must not repeat its predicate.
        for n in [8, 40] {
            let index = ExtentIndex::new((0..n).map(|_| Extent::new(0.0, 0.0, 10.0, 10.0)));
            let query = Extent::new(-100.0, -100.0, 100.0, 100.0);
            let mut calls = 0;
            assert!(index.any_candidate(&query, |_| {
                calls += 1;
                true
            }));
            assert_eq!(calls, 1);
            let mut seen = std::collections::HashSet::new();
            assert!(!index.any_candidate(&query, |i| {
                assert!(seen.insert(i));
                false
            }));
            assert_eq!(seen.len(), n);
        }
        let index = ExtentIndex::new((0..40).map(|i| Extent::new(f64::from(i), 0.0, f64::from(i) + 0.1, 0.1)));
        let mut calls = 0;
        assert!(!index.any_candidate(&Extent::new(0.0, 0.0, 0.1, 0.1), |_| {
            calls += 1;
            false
        }));
        assert!(calls > 0 && calls < 40, "visited {calls} candidates");
        assert!(!index.any_candidate(&Extent::new(50.0, 50.0, 51.0, 51.0), |_| panic!("disjoint query visited a candidate")));
        assert!(!index.any_candidate(&Extent::EMPTY, |_| panic!("empty query visited a candidate")));
    }

    #[test]
    fn extent_index_looks_only_at_its_own_cells() {
        // 20 small extents in a 1° row: a grid of about 4 cells a side.
        let index = ExtentIndex::new((0..20).map(|i| Extent::new(f64::from(i) * 0.05, 0.0, f64::from(i) * 0.05 + 0.01, 0.01)));
        assert!(index.cell_size().is_some());
        // A query far larger than the layer finds every feature once, in order.
        assert_eq!(index.candidates(&Extent::new(-170.0, -80.0, 170.0, 80.0)), (0..20).collect::<Vec<_>>());
        // A small one finds the features of its cell, not the whole row.
        let near_start = index.candidates(&Extent::new(0.0, 0.0, 0.02, 0.005));
        assert!(near_start.first() == Some(&0) && near_start.len() < 20, "{near_start:?}");
        assert!(index.candidates(&Extent::new(5.0, 5.0, 6.0, 6.0)).is_empty());

        let mut buf = Vec::new();
        index.candidates_into(&Extent::new(0.0, 0.0, 0.02, 0.005), &mut buf);
        assert_eq!(buf, near_start);
        let ptr = buf.as_ptr();
        index.candidates_into(&Extent::new(5.0, 5.0, 6.0, 6.0), &mut buf);
        assert!(buf.is_empty());
        assert_eq!(buf.as_ptr(), ptr, "the buffer's allocation is reused");

        // An index without a grid (<= 16 features) replaces the buffer with all feature indices.
        let small_index = ExtentIndex::new((0..5).map(|i| Extent::new(f64::from(i), 0.0, f64::from(i) + 0.1, 0.1)));
        assert!(small_index.cell_size().is_none());
        let mut stale_buf = vec![999];
        small_index.candidates_into(&Extent::new(0.0, 0.0, 10.0, 10.0), &mut stale_buf);
        assert_eq!(stale_buf, (0..5).collect::<Vec<_>>());
    }

    #[test]
    fn intersect_needs_an_overlay_of_polygons_and_an_input_with_geometry() {
        let mut m = inputs();
        let e = crate::run("intersect", &m, &serde_json::json!({"input": "polys", "overlay": "pts"})).unwrap_err();
        assert!(e.contains("not a polygon layer"), "{e}");
        m.0.insert("table".into(), Dataset::new("table", GeometryType::None, vec![]));
        let e = crate::run("intersect", &m, &serde_json::json!({"input": "table", "overlay": "polys"})).unwrap_err();
        assert!(e.contains("no geometry"), "{e}");
    }

    /// Each feature's `id` and the overlay's (`id_2`), "" for an empty one: "p1|z1", "p1|", "|z2".
    fn overlay_keys(out: &Dataset) -> Vec<String> {
        let text = |v: Option<&Value>| if let Some(Value::Text(t)) = v { t.clone() } else { String::new() };
        out.features.iter().map(|f| format!("{}|{}", text(f.attrs.first()), text(f.attrs.get(2)))).collect()
    }

    #[test]
    fn identity_keeps_every_part_of_the_input() {
        let m = parcels_and_zones();
        let out = layer(crate::run("identity", &m, &serde_json::json!({"input": "parcels", "overlay": "zones"})).unwrap());
        assert_eq!(out.name, "parcels_Identity");
        // p1's half in z1, then its other half; p2 only touches z2, so it is kept whole.
        assert_eq!(overlay_keys(&out), ["p1|z1", "p1|", "p2|"]);
        let rest = out.features[1].geometry.as_ref().unwrap().extent();
        assert!((rest.min_x - 0.0).abs() < 1e-9 && (rest.max_x - 1.0).abs() < 1e-9, "{rest:?}");
        assert_eq!(out.features[1].attrs[2..], [Value::Null, Value::Null]);
        assert_eq!(out.features[2].geometry, m.0["parcels"].features[1].geometry);
    }

    #[test]
    fn identity_and_union_keep_input_features_without_geometry() {
        let mut m = parcels_and_zones();
        m.0.get_mut("parcels").unwrap().push(None, vec![Value::Text("p3".into()), Value::Text("lot".into())]);
        for (tool, keys) in [("identity", vec!["p1|z1", "p1|", "p2|", "p3|"]), ("union", vec!["p1|z1", "p1|", "p2|", "p3|", "|z1", "|z2"])]
        {
            let out = layer(crate::run(tool, &m, &serde_json::json!({"input": "parcels", "overlay": "zones"})).unwrap());
            assert_eq!(overlay_keys(&out), keys, "{tool}");
            let p3 = out.features.iter().find(|f| f.attrs.first() == Some(&Value::Text("p3".into()))).unwrap();
            assert_eq!((p3.geometry.is_none(), &p3.attrs[1]), (true, &Value::Text("lot".into())), "{tool}");
        }
        let out = layer(crate::run("intersect", &m, &serde_json::json!({"input": "parcels", "overlay": "zones"})).unwrap());
        assert_eq!(overlay_keys(&out), ["p1|z1"]);
    }

    #[test]
    fn identity_unions_an_input_feature_whose_parts_overlap() {
        // One parcel of two overlapping squares (x 0–2 and 1–3), and a zone over its right edge.
        let ring =
            |x: f64| vec![Coord::new(x, 0.0), Coord::new(x + 2.0, 0.0), Coord::new(x + 2.0, 2.0), Coord::new(x, 2.0), Coord::new(x, 0.0)];
        let mut parcels = Dataset::new("parcels", GeometryType::Polygon, vec![]);
        parcels.push(Some(Geometry::Polygon(vec![vec![ring(0.0)], vec![ring(1.0)]])), vec![]);
        let mut zones = Dataset::new("zones", GeometryType::Polygon, vec![]);
        zones.push(Some(sq(2.5, 0.0, 2.0)), vec![]);
        let m = Map(HashMap::from([("parcels".to_string(), parcels), ("zones".to_string(), zones)]));
        let out = layer(crate::run("identity", &m, &serde_json::json!({"input": "parcels", "overlay": "zones"})).unwrap());
        assert_eq!(out.features.len(), 2);
        // The part outside is x 0–2.5 whole, the overlap (x 1–2) included.
        use geo::Area;
        let rest = to_multipolygon(out.features[1].geometry.as_ref().unwrap()).unwrap();
        let outline = vec![Coord::new(0.0, 0.0), Coord::new(2.5, 0.0), Coord::new(2.5, 2.0), Coord::new(0.0, 2.0), Coord::new(0.0, 0.0)];
        let want = to_multipolygon(&Geometry::Polygon(vec![vec![outline]])).unwrap();
        assert_eq!(rest.0.len(), 1, "{rest:?}");
        assert!((rest.unsigned_area() - want.unsigned_area()).abs() < 1e-6 * want.unsigned_area(), "{rest:?}");
    }

    #[test]
    fn identity_keeps_a_line_along_a_zone_edge_once() {
        // A road along the top edge of a zone, and on past its corner.
        let mut roads = Dataset::new("roads", GeometryType::Polyline, vec![Field::new("id", FieldType::Text)]);
        roads.push(Some(Geometry::Line(vec![vec![Coord::new(0.0, 2.0), Coord::new(3.0, 2.0)]])), vec![Value::Text("r".into())]);
        let mut zones = Dataset::new("zones", GeometryType::Polygon, vec![Field::new("zone", FieldType::Text)]);
        zones.push(Some(sq(0.0, 0.0, 2.0)), vec![Value::Text("z".into())]);
        let m = Map(HashMap::from([("roads".to_string(), roads), ("zones".to_string(), zones)]));
        let out = layer(crate::run("identity", &m, &serde_json::json!({"input": "roads", "overlay": "zones"})).unwrap());
        // The stretch on the edge is inside (x 0–2), once; the rest (x 2–3) outside.
        let spans: Vec<(f64, f64, &Value)> = out
            .features
            .iter()
            .map(|f| {
                let e = f.geometry.as_ref().unwrap().extent();
                (e.min_x, e.max_x, &f.attrs[1])
            })
            .collect();
        assert_eq!(spans.len(), 2, "{spans:?}");
        let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(near(spans[0].0, 0.0) && near(spans[0].1, 2.0) && *spans[0].2 == Value::Text("z".into()), "{spans:?}");
        assert!(near(spans[1].0, 2.0) && near(spans[1].1, 3.0) && *spans[1].2 == Value::Null, "{spans:?}");
    }

    #[test]
    fn identity_and_union_copy_an_uncut_input_feature_unchanged() {
        // Two overlapping squares as one parcel, far from the zone; and a triangle whose extent
        // meets the zone's but whose shape doesn't reach it.
        let ring = |x: f64| {
            vec![Coord::new(x, 10.0), Coord::new(x + 2.0, 10.0), Coord::new(x + 2.0, 12.0), Coord::new(x, 12.0), Coord::new(x, 10.0)]
        };
        let overlapping = Geometry::Polygon(vec![vec![ring(0.0)], vec![ring(1.0)]]);
        let triangle =
            Geometry::Polygon(vec![vec![vec![Coord::new(0.0, 0.0), Coord::new(4.0, 0.0), Coord::new(0.0, 4.0), Coord::new(0.0, 0.0)]]]);
        let mut parcels = Dataset::new("parcels", GeometryType::Polygon, vec![]);
        parcels.push(Some(overlapping.clone()), vec![]);
        parcels.push(Some(triangle.clone()), vec![]);
        let mut zones = Dataset::new("zones", GeometryType::Polygon, vec![]);
        zones.push(Some(sq(3.0, 3.0, 1.0)), vec![]);
        let m = Map(HashMap::from([("parcels".to_string(), parcels), ("zones".to_string(), zones)]));
        for tool in ["identity", "union"] {
            let out = layer(crate::run(tool, &m, &serde_json::json!({"input": "parcels", "overlay": "zones"})).unwrap());
            let got: Vec<Option<&Geometry>> = out.features.iter().take(2).map(|f| f.geometry.as_ref()).collect();
            assert_eq!(got, [Some(&overlapping), Some(&triangle)], "{tool}: copied as they are, parts and coordinates");
        }
    }

    #[test]
    fn identity_keeps_points_outside_every_polygon() {
        let m = inputs();
        let out = layer(crate::run("identity", &m, &serde_json::json!({"input": "pts", "overlay": "polys"})).unwrap());
        let rows: Vec<(f64, Value)> = out.features.iter().map(|f| (f.attrs[0].as_f64().unwrap(), f.attrs[1].clone())).collect();
        let a = Value::Text("a".into());
        assert_eq!(rows, [(1.0, a.clone()), (2.0, a.clone()), (2.0, a), (3.0, Value::Null)]);
    }

    #[test]
    fn union_adds_the_overlay_left_uncovered() {
        let m = parcels_and_zones();
        let out = layer(crate::run("union", &m, &serde_json::json!({"input": "parcels", "overlay": "zones"})).unwrap());
        assert_eq!(out.name, "parcels_Union");
        // z1 less p1's half of it; z2, which no parcel overlaps, whole (overlapping z1 still).
        assert_eq!(overlay_keys(&out), ["p1|z1", "p1|", "p2|", "|z1", "|z2"]);
        let z1_rest = out.features[3].geometry.as_ref().unwrap().extent();
        assert!((z1_rest.min_x - 2.0).abs() < 1e-9 && (z1_rest.max_x - 3.0).abs() < 1e-9, "{z1_rest:?}");
        assert_eq!(out.features[3].attrs[..2], [Value::Null, Value::Null]);
        assert_eq!(out.features[4].attrs[3], Value::Text("C2".into()));
        let e = crate::run("union", &inputs(), &serde_json::json!({"input": "pts", "overlay": "polys"})).unwrap_err();
        assert!(e.contains("not a polygon layer"), "{e}");
    }

    #[test]
    fn symmetrical_difference_keeps_what_only_one_layer_covers() {
        let mut m = parcels_and_zones();
        m.0.get_mut("parcels").unwrap().push(None, vec![Value::Text("p3".into()), Value::Text("lot".into())]);
        let out = layer(crate::run("symmetricalDifference", &m, &serde_json::json!({"input": "parcels", "overlay": "zones"})).unwrap());
        assert_eq!(out.name, "parcels_SymDiff");
        let names: Vec<&str> = out.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["id", "use", "id_2", "zone"]);
        // Union's features less p1's half in z1: p1's other half, p2 whole, p3 without geometry,
        // then z1 less p1 and z2 whole.
        assert_eq!(overlay_keys(&out), ["p1|", "p2|", "p3|", "|z1", "|z2"]);
        let x = |r: usize| {
            let e = out.features[r].geometry.as_ref().unwrap().extent();
            (e.min_x, e.max_x)
        };
        let near = |(a, b): (f64, f64), (c, d): (f64, f64)| (a - c).abs() < 1e-9 && (b - d).abs() < 1e-9;
        assert!(near(x(0), (0.0, 1.0)) && near(x(3), (2.0, 3.0)), "{:?} {:?}", x(0), x(3));
        assert_eq!(out.features[1].geometry, m.0["parcels"].features[1].geometry, "p2 only touches z2: copied as it is");
        let e = crate::run("symmetricalDifference", &inputs(), &serde_json::json!({"input": "pts", "overlay": "polys"})).unwrap_err();
        assert!(e.contains("not a polygon layer"), "{e}");
    }

    #[test]
    fn symmetrical_difference_honours_both_layers_selections() {
        // Only p2 and z1 selected: p1 doesn't take its half out of z1.
        let m = Selected(parcels_and_zones(), HashMap::from([("parcels".to_string(), vec![1]), ("zones".to_string(), vec![0])]));
        let out = layer(crate::run("symmetricalDifference", &m, &serde_json::json!({"input": "parcels", "overlay": "zones"})).unwrap());
        assert_eq!(overlay_keys(&out), ["p2|", "|z1"]);
    }

    #[test]
    fn erase_takes_points_and_lines_too() {
        let m = inputs();
        let out = layer(crate::run("erase", &m, &serde_json::json!({"input": "pts", "erase": "polys"})).unwrap());
        assert_eq!((out.name.as_str(), out.geometry_type), ("pts_Erase", GeometryType::Point));
        // (0.5, 0.5) and (1.5, 1.5) are in the squares; only (50, 50) is left.
        let left: Vec<&Value> = out.features.iter().map(|f| &f.attrs[0]).collect();
        assert_eq!(left, [&Value::Float(3.0)]);
        // A road across the 2° square x 0–2 (and the one over x 1–3): what is left either side.
        let mut roads = Dataset::new("roads", GeometryType::Polyline, vec![]);
        roads.push(Some(Geometry::Line(vec![vec![Coord::new(-1.0, 1.0), Coord::new(4.0, 1.0)]])), vec![]);
        let mut m = m;
        m.0.insert("roads".into(), roads);
        let out = layer(crate::run("erase", &m, &serde_json::json!({"input": "roads", "erase": "polys"})).unwrap());
        assert_eq!(out.geometry_type, GeometryType::Polyline);
        let Some(Geometry::Line(parts)) = out.features.first().and_then(|f| f.geometry.as_ref()) else { panic!("{:?}", out.features) };
        let mut spans: Vec<(f64, f64)> =
            parts.iter().map(|p| p.iter().fold((f64::MAX, f64::MIN), |(a, b), c| (a.min(c.x), b.max(c.x)))).collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert_eq!(spans.len(), 2, "{spans:?}");
        // Cut in Web Mercator and back, so not to the last bit.
        assert!((spans[0].0 + 1.0).abs() < 1e-7 && spans[0].1.abs() < 1e-7, "{spans:?}");
        assert!((spans[1].0 - 3.0).abs() < 1e-7 && (spans[1].1 - 4.0).abs() < 1e-7, "{spans:?}");
    }

    #[test]
    fn erase_unions_an_input_feature_whose_parts_overlap() {
        // One parcel of two overlapping squares (x 0–2 and 1–3), and an erase square over x 2.5–4.5.
        let ring =
            |x: f64| vec![Coord::new(x, 0.0), Coord::new(x + 2.0, 0.0), Coord::new(x + 2.0, 2.0), Coord::new(x, 2.0), Coord::new(x, 0.0)];
        let mut parcels = Dataset::new("parcels", GeometryType::Polygon, vec![]);
        parcels.push(Some(Geometry::Polygon(vec![vec![ring(0.0)], vec![ring(1.0)]])), vec![]);
        let mut zones = Dataset::new("zones", GeometryType::Polygon, vec![]);
        zones.push(Some(sq(2.5, 0.0, 2.0)), vec![]);
        let m = Map(HashMap::from([("parcels".to_string(), parcels), ("zones".to_string(), zones)]));
        let out = layer(crate::run("erase", &m, &serde_json::json!({"input": "parcels", "erase": "zones"})).unwrap());
        // What is left is x 0–2.5 whole, the overlap (x 1–2) included.
        use geo::Area;
        let rest = to_multipolygon(out.features[0].geometry.as_ref().unwrap()).unwrap();
        let want = to_multipolygon(&Geometry::Polygon(vec![vec![vec![
            Coord::new(0.0, 0.0),
            Coord::new(2.5, 0.0),
            Coord::new(2.5, 2.0),
            Coord::new(0.0, 2.0),
            Coord::new(0.0, 0.0),
        ]]]))
        .unwrap();
        assert_eq!(rest.0.len(), 1, "{rest:?}");
        assert!((rest.unsigned_area() - want.unsigned_area()).abs() < 1e-6 * want.unsigned_area(), "{rest:?}");
    }

    #[test]
    fn update_replaces_the_covered_part_with_the_update_polygons() {
        let mut m = parcels_and_zones();
        m.0.get_mut("parcels").unwrap().push(None, vec![Value::Text("p3".into()), Value::Text("lot".into())]);
        // An update feature without geometry adds nothing.
        m.0.get_mut("zones").unwrap().push(None, vec![Value::Text("z0".into()), Value::Null]);
        let out = layer(crate::run("update", &m, &serde_json::json!({"input": "parcels", "update": "zones"})).unwrap());
        assert_eq!(out.name, "parcels_Update");
        let names: Vec<&str> = out.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["id", "use"], "the input's fields only");
        let text = |t: &str| Value::Text(t.into());
        let attrs: Vec<&[Value]> = out.features.iter().map(|f| f.attrs.as_slice()).collect();
        // p1 less z1, p2 (only touching z2) and p3 as they are; then z1 and z2 whole, their `id`
        // in the input's `id` and no `use`.
        assert_eq!(
            attrs,
            [
                &[text("p1"), text("home")][..],
                &[text("p2"), text("shop")],
                &[text("p3"), text("lot")],
                &[text("z1"), Value::Null],
                &[text("z2"), Value::Null],
            ]
        );
        let p1 = out.features[0].geometry.as_ref().unwrap().extent();
        assert!(p1.min_x.abs() < 1e-9 && (p1.max_x - 1.0).abs() < 1e-9, "{p1:?}");
        let (parcels, zones) = (&m.0["parcels"], &m.0["zones"]);
        assert_eq!(out.features[1].geometry, parcels.features[1].geometry);
        assert!(out.features[2].geometry.is_none());
        assert_eq!(out.features[3].geometry, zones.features[0].geometry, "update polygons as they are");
        assert_eq!(out.features[4].geometry, zones.features[1].geometry);
        let e = crate::run("update", &inputs(), &serde_json::json!({"input": "pts", "update": "polys"})).unwrap_err();
        assert!(e.contains("not a polygon layer"), "{e}");
    }

    #[test]
    fn update_converts_values_to_the_input_fields_types() {
        let fields = vec![
            Field::new("n", FieldType::Integer),
            Field::new("x", FieldType::Double),
            Field::new("t", FieldType::Text),
            Field::new("b", FieldType::Boolean),
        ];
        let mut parcels = Dataset::new("parcels", GeometryType::Polygon, fields);
        parcels.push(Some(sq(0.0, 0.0, 1.0)), vec![Value::Int(1), Value::Float(1.0), Value::Text("a".into()), Value::Bool(false)]);
        // Same names in another case and order, with other types; and a field the input lacks.
        let mut zones = Dataset::new(
            "zones",
            GeometryType::Polygon,
            vec![
                Field::new("other", FieldType::Text),
                Field::new("T", FieldType::Double),
                Field::new("N", FieldType::Text),
                Field::new("X", FieldType::Integer),
                Field::new("B", FieldType::Integer),
            ],
        );
        let row = |t: f64, n: &str, x: i64, b: i64| {
            vec![Value::Text("o".into()), Value::Float(t), Value::Text(n.into()), Value::Int(x), Value::Int(b)]
        };
        zones.push(Some(sq(5.0, 0.0, 1.0)), row(0.1, " 7 ", 3, 1));
        zones.push(Some(sq(7.0, 0.0, 1.0)), row(2.5, "seven", -2, 0));
        let m = Map(HashMap::from([("parcels".to_string(), parcels), ("zones".to_string(), zones)]));
        let out = layer(crate::run("update", &m, &serde_json::json!({"input": "parcels", "update": "zones"})).unwrap());
        let attrs: Vec<&[Value]> = out.features.iter().skip(1).map(|f| f.attrs.as_slice()).collect();
        assert_eq!(
            attrs,
            [
                &[Value::Int(7), Value::Float(3.0), Value::Text("0.1".into()), Value::Bool(true)][..],
                &[Value::Null, Value::Float(-2.0), Value::Text("2.5".into()), Value::Bool(false)],
            ]
        );
    }

    #[test]
    fn converted_keeps_fitting_values_and_converts_the_rest() {
        let third = 1.0 / 3.0;
        assert_eq!(converted(&Value::Float(third), FieldType::Double), Value::Float(third));
        assert_eq!(converted(&Value::Float(third), FieldType::Text), Value::Text(third.to_string()), "full precision");
        assert_eq!(converted(&Value::Float(-2.7), FieldType::Integer), Value::Int(-2));
        assert_eq!(converted(&Value::Float(f64::NAN), FieldType::Integer), Value::Null);
        // Beyond i64: empty, not clamped to its largest value.
        assert_eq!(converted(&Value::Float(1e30), FieldType::Integer), Value::Null);
        assert_eq!(converted(&Value::Float(-1e30), FieldType::Integer), Value::Null);
        assert_eq!(converted(&Value::Float(9.223_372_036_854_776e18), FieldType::Integer), Value::Null, "2⁶³ itself");
        assert_eq!(converted(&Value::Float(-9.223_372_036_854_776e18), FieldType::Integer), Value::Int(i64::MIN));
        // Text goes the same way: an integer as it is, another number as that float.
        let text = |t: &str| converted(&Value::Text(t.into()), FieldType::Integer);
        assert_eq!(text(" 9223372036854775807 "), Value::Int(i64::MAX));
        assert_eq!(text("-2.7"), Value::Int(-2));
        for t in ["1e30", "-1e30", "NaN", "inf", "x", ""] {
            assert_eq!(text(t), Value::Null, "{t}");
        }
        // And into a Double field: finite numbers only.
        let double = |t: &str| converted(&Value::Text(t.into()), FieldType::Double);
        assert_eq!(double(" 2.5 "), Value::Float(2.5));
        for t in ["NaN", "inf", "-infinity", "1e400", "x", ""] {
            assert_eq!(double(t), Value::Null, "{t}");
        }
        assert_eq!(converted(&Value::Bool(true), FieldType::Double), Value::Float(1.0));
        assert_eq!(converted(&Value::Int(5), FieldType::Text), Value::Text("5".into()));
        assert_eq!(converted(&Value::Text("x".into()), FieldType::Double), Value::Null);
        assert_eq!(converted(&Value::Null, FieldType::Text), Value::Null);
    }

    /// The layers of a tool's output with several.
    fn layers_of(r: ToolResult) -> Vec<Dataset> {
        match r.output {
            Output::Layers(v) => v
                .into_iter()
                .map(|o| match o {
                    OutputLayer::Vector(d) => d,
                    _ => panic!("expected vector layers"),
                })
                .collect(),
            _ => panic!("expected several layers"),
        }
    }

    #[test]
    fn split_writes_one_layer_per_split_polygon() {
        let m = parcels_and_zones();
        let r = crate::run("split", &m, &serde_json::json!({"input": "parcels", "split": "zones"})).unwrap();
        // z1 (id 1) holds p1's right half; z2 only touches p1 and p2, so its layer is left out.
        assert!(r.messages[0].contains("1 split area held no features"), "{:?}", r.messages);
        let out = layers_of(r);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "parcels_1");
        assert_eq!(out[0].fields, m.0["parcels"].fields);
        assert_eq!(out[0].features.len(), 1);
        assert_eq!(out[0].features[0].attrs[0], Value::Text("p1".into()));
        let e = out[0].features[0].geometry.as_ref().unwrap().extent();
        assert!((e.min_x - 1.0).abs() < 1e-9 && (e.max_x - 2.0).abs() < 1e-9, "{e:?}");
    }

    #[test]
    fn split_by_a_field_makes_one_layer_per_value() {
        let mut m = parcels_and_zones();
        let text = |t: &str| Value::Text(t.into());
        // z3 shares z1's zone, and covers p2; z4 has no zone, and covers p1's left half.
        let zones = m.0.get_mut("zones").unwrap();
        zones.push(Some(sq(4.0, 0.0, 1.0)), vec![text("z3"), text("R1")]);
        zones.push(Some(sq(0.0, 0.0, 1.0)), vec![text("z4"), Value::Null]);
        let r = crate::run("split", &m, &serde_json::json!({"input": "parcels", "split": "zones", "field": "ZONE", "output": "lots"}));
        let out = layers_of(r.unwrap());
        let names: Vec<&str> = out.iter().map(|d| d.name.as_str()).collect();
        // C2 (z2) holds nothing; z4 is named by its id.
        assert_eq!(names, ["lots_R1", "lots_4"]);
        let ids = |d: &Dataset| d.features.iter().map(|f| f.attrs[0].clone()).collect::<Vec<_>>();
        assert_eq!(ids(&out[0]), [text("p1"), text("p2")]);
        assert_eq!(ids(&out[1]), [text("p1")]);
        let e = crate::run("split", &m, &serde_json::json!({"input": "parcels", "split": "zones", "field": "nope"})).unwrap_err();
        assert!(e.contains("no field named"), "{e}");
    }

    #[test]
    fn split_puts_a_point_on_a_shared_edge_in_both_layers() {
        let mut m = inputs();
        let mut zones = Dataset::new("zones", GeometryType::Polygon, vec![]);
        zones.push(Some(sq(0.0, 0.0, 1.0)), vec![]);
        zones.push(Some(sq(1.0, 0.0, 1.0)), vec![]);
        m.0.insert("zones".into(), zones);
        let mut edge = Dataset::new("edge", GeometryType::Point, vec![]);
        edge.push(Some(Geometry::Point(Coord::new(1.0, 0.5))), vec![]);
        m.0.insert("edge".into(), edge);
        let out = layers_of(crate::run("split", &m, &serde_json::json!({"input": "edge", "split": "zones"})).unwrap());
        assert_eq!(out.iter().map(|d| d.features.len()).collect::<Vec<_>>(), [1, 1]);
        let e = crate::run("split", &m, &serde_json::json!({"input": "pts", "split": "pts"})).unwrap_err();
        assert!(e.contains("not a polygon layer"), "{e}");
    }

    #[test]
    fn split_honours_both_layers_selections() {
        let mut m = parcels_and_zones();
        m.0.get_mut("zones").unwrap().push(Some(sq(4.0, 0.0, 1.0)), vec![Value::Text("z3".into()), Value::Null]);
        // z1 and z3 selected, p2 only: z1's layer would hold p1, so it is left out.
        let m = Selected(m, HashMap::from([("parcels".to_string(), vec![1]), ("zones".to_string(), vec![0, 2])]));
        let out = layers_of(crate::run("split", &m, &serde_json::json!({"input": "parcels", "split": "zones"})).unwrap());
        let names: Vec<&str> = out.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["parcels_3"]);
    }

    #[test]
    fn split_keeps_values_apart_that_would_be_named_alike() {
        let mut m = inputs();
        let long = "x".repeat(MAX_SPLIT_NAME);
        let mut zones = Dataset::new("zones", GeometryType::Polygon, vec![Field::new("name", FieldType::Text)]);
        // Alike in their first 64 characters, and alike without the line break.
        for (x, name) in [(0.0, format!("{long}a")), (0.5, format!("{long}b")), (1.0, "a\nb".into()), (1.5, "ab".into())] {
            zones.push(Some(sq(x, 0.0, 0.5)), vec![Value::Text(name)]);
        }
        m.0.insert("zones".into(), zones);
        let mut row = Dataset::new("row", GeometryType::Point, vec![]);
        for x in [0.25, 0.75, 1.25, 1.75] {
            row.push(Some(Geometry::Point(Coord::new(x, 0.25))), vec![]);
        }
        m.0.insert("row".into(), row);
        let out = layers_of(crate::run("split", &m, &serde_json::json!({"input": "row", "split": "zones", "field": "name"})).unwrap());
        let names: Vec<&str> = out.iter().map(|d| d.name.as_str()).collect();
        let cut = format!("row_{long}");
        assert_eq!(names, [cut.as_str(), cut.as_str(), "row_ab", "row_ab"], "four layers, the names shortened");
        assert!(out.iter().all(|d| d.features.len() == 1));
    }

    #[test]
    fn update_refuses_a_field_two_update_fields_match() {
        let mut m = parcels_and_zones();
        let zones = m.0.get_mut("zones").unwrap();
        zones.fields.push(Field::new("ID", FieldType::Text));
        for f in &mut zones.features {
            f.attrs.push(Value::Null);
        }
        // `use` matches nothing; the parcels' `id` matches the zones' `id` exactly, so that is fine.
        assert!(crate::run("update", &m, &serde_json::json!({"input": "parcels", "update": "zones"})).is_ok());
        // `Id` matches `id` and `ID` alike.
        m.0.get_mut("parcels").unwrap().fields[0].name = "Id".into();
        let e = crate::run("update", &m, &serde_json::json!({"input": "parcels", "update": "zones"})).unwrap_err();
        assert!(e.contains("ambiguous"), "{e}");
    }

    #[test]
    fn erase_keeps_a_feature_no_erase_polygon_overlaps_as_it_is() {
        // Two overlapping squares as one parcel, far from the erase square; and a triangle whose
        // extent meets the erase square's but whose shape doesn't reach it.
        let ring = |x: f64| {
            vec![Coord::new(x, 10.0), Coord::new(x + 2.0, 10.0), Coord::new(x + 2.0, 12.0), Coord::new(x, 12.0), Coord::new(x, 10.0)]
        };
        let overlapping = Geometry::Polygon(vec![vec![ring(0.0)], vec![ring(1.0)]]);
        let triangle =
            Geometry::Polygon(vec![vec![vec![Coord::new(0.0, 0.0), Coord::new(4.0, 0.0), Coord::new(0.0, 4.0), Coord::new(0.0, 0.0)]]]);
        let mut parcels = Dataset::new("parcels", GeometryType::Polygon, vec![]);
        parcels.push(Some(overlapping.clone()), vec![]);
        parcels.push(Some(triangle.clone()), vec![]);
        let mut zones = Dataset::new("zones", GeometryType::Polygon, vec![]);
        zones.push(Some(sq(3.0, 3.0, 1.0)), vec![]);
        let m = Map(HashMap::from([("parcels".to_string(), parcels), ("zones".to_string(), zones)]));
        let out = layer(crate::run("erase", &m, &serde_json::json!({"input": "parcels", "erase": "zones"})).unwrap());
        let got: Vec<Option<&Geometry>> = out.features.iter().map(|f| f.geometry.as_ref()).collect();
        assert_eq!(got, [Some(&overlapping), Some(&triangle)], "copied as they are, parts and coordinates");
    }

    #[test]
    fn erase_needs_an_input_with_geometry() {
        let mut m = inputs();
        m.0.insert("table".into(), Dataset::new("table", GeometryType::None, vec![]));
        let e = crate::run("erase", &m, &serde_json::json!({"input": "table", "erase": "polys"})).unwrap_err();
        assert!(e.contains("no geometry"), "{e}");
    }

    #[test]
    fn split_names_a_value_of_control_characters_by_its_id() {
        let mut m = parcels_and_zones();
        // z1's zone is only a control character (trim leaves it), so its layer is named by its id.
        m.0.get_mut("zones").unwrap().features[0].attrs[1] = Value::Text("\u{1}".into());
        let out = layers_of(crate::run("split", &m, &serde_json::json!({"input": "parcels", "split": "zones", "field": "zone"})).unwrap());
        let names: Vec<&str> = out.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["parcels_1"]);
        assert_eq!(split_name(" \u{1}a\tb "), "ab");
    }

    #[test]
    fn split_keeps_floats_apart_that_display_alike() {
        let mut m = inputs();
        let mut zones = Dataset::new("zones", GeometryType::Polygon, vec![Field::new("v", FieldType::Double)]);
        // Both display as 0.000001; -0 and 0 are one value.
        for (x, v) in [(0.0, 0.000_001_2), (0.5, 0.000_001_4), (1.0, -0.0), (1.5, 0.0)] {
            zones.push(Some(sq(x, 0.0, 0.5)), vec![Value::Float(v)]);
        }
        m.0.insert("zones".into(), zones);
        let mut row = Dataset::new("row", GeometryType::Point, vec![]);
        for x in [0.25, 0.75, 1.25, 1.75] {
            row.push(Some(Geometry::Point(Coord::new(x, 0.25))), vec![]);
        }
        m.0.insert("row".into(), row);
        let out = layers_of(crate::run("split", &m, &serde_json::json!({"input": "row", "split": "zones", "field": "v"})).unwrap());
        let layers: Vec<(&str, usize)> = out.iter().map(|d| (d.name.as_str(), d.features.len())).collect();
        assert_eq!(layers, [("row_0.000001", 1), ("row_0.000001", 1), ("row_0", 2)]);
        // Each float layer holds the point in its own zone: 0.0000012's at x 0.25, 0.0000014's at 0.75.
        let x = |d: &Dataset| d.features.iter().filter_map(|f| f.geometry.as_ref()).map(|g| g.extent().min_x).collect::<Vec<_>>();
        assert_eq!((x(&out[0]), x(&out[1])), (vec![0.25], vec![0.75]));
    }

    #[test]
    fn split_refuses_a_split_field_that_is_not_a_name() {
        let m = parcels_and_zones();
        for bad in [serde_json::json!(5), serde_json::json!(["zone"]), serde_json::json!(true)] {
            let e = crate::run("split", &m, &serde_json::json!({"input": "parcels", "split": "zones", "field": bad})).unwrap_err();
            assert!(e.contains("must be a field name"), "{e}");
        }
        // Empty or null is no field.
        for none in [serde_json::json!(""), serde_json::json!(null)] {
            assert!(crate::run("split", &m, &serde_json::json!({"input": "parcels", "split": "zones", "field": none})).is_ok());
        }
    }

    #[test]
    fn erase_update_split_and_overlays_keep_an_incomplete_crs_flag() {
        for incomplete in ["parcels", "zones"] {
            incomplete_flag_reaches_every_output(incomplete);
        }
    }

    #[test]
    fn overlays_leave_the_crs_flag_clear_when_inputs_are_complete() {
        let m = parcels_and_zones();
        for (tool, p) in [
            ("erase", serde_json::json!({"input": "parcels", "erase": "zones"})),
            ("update", serde_json::json!({"input": "parcels", "update": "zones"})),
            ("symmetricalDifference", serde_json::json!({"input": "parcels", "overlay": "zones"})),
            ("intersect", serde_json::json!({"input": "parcels", "overlay": "zones"})),
        ] {
            assert!(!layer(crate::run(tool, &m, &p).unwrap()).crs_incomplete, "{tool}");
        }
        let out = layers_of(crate::run("split", &m, &serde_json::json!({"input": "parcels", "split": "zones"})).unwrap());
        assert!(!out.is_empty() && out.iter().all(|d| !d.crs_incomplete));
    }

    /// Runs every tool that overlays parcels with zones, with layer `lost` marked as having lost
    /// features on reading, and checks each output is marked so too.
    fn incomplete_flag_reaches_every_output(lost: &str) {
        let mut m = parcels_and_zones();
        m.0.get_mut(lost).unwrap().crs_incomplete = true;
        let params = [
            ("erase", serde_json::json!({"input": "parcels", "erase": "zones"})),
            ("update", serde_json::json!({"input": "parcels", "update": "zones"})),
            ("symmetricalDifference", serde_json::json!({"input": "parcels", "overlay": "zones"})),
            ("intersect", serde_json::json!({"input": "parcels", "overlay": "zones"})),
        ];
        for (tool, p) in params {
            assert!(layer(crate::run(tool, &m, &p).unwrap()).crs_incomplete, "{tool}, {lost} incomplete");
        }
        let out = layers_of(crate::run("split", &m, &serde_json::json!({"input": "parcels", "split": "zones"})).unwrap());
        assert!(!out.is_empty() && out.iter().all(|d| d.crs_incomplete));
    }

    #[test]
    fn split_refuses_more_split_areas_than_it_takes() {
        let mut m = inputs();
        let mut zones = Dataset::new("zones", GeometryType::Polygon, vec![]);
        for i in 0..=MAX_SPLIT_AREAS {
            zones.push_next(Some(sq(f64::from(i as u32 % 100), f64::from(i as u32 / 100), 0.5)), vec![]);
        }
        m.0.insert("zones".into(), zones);
        let e = crate::run("split", &m, &serde_json::json!({"input": "pts", "split": "zones"})).unwrap_err();
        assert!(e.contains("at most"), "{e}");
    }

    #[test]
    fn split_accepts_exactly_max_split_areas() {
        let mut m = inputs();
        let mut zones = Dataset::new("zones", GeometryType::Polygon, vec![]);
        for i in 0..MAX_SPLIT_AREAS {
            zones.push_next(Some(sq(f64::from(i as u32 % 100), f64::from(i as u32 / 100), 0.5)), vec![]);
        }
        m.0.insert("zones".into(), zones);
        // (0.5, 0.5) and (1.5, 1.5) fall in the first and 102nd zones' corners; the rest hold nothing.
        let r = crate::run("split", &m, &serde_json::json!({"input": "pts", "split": "zones"})).unwrap();
        assert!(r.messages[0].contains("held no features"), "{:?}", r.messages);
    }

    /// Near's four fields of row `r`: the near feature's id, the distance and the point.
    fn near_fields(out: &Dataset, r: usize) -> (Option<i64>, Option<f64>, Option<f64>, Option<f64>) {
        let n = out.fields.len();
        let v = |k: usize| out.features[r].attrs[n - 4 + k].clone();
        (v(0).as_f64().map(|f| f as i64), v(1).as_f64(), v(2).as_f64(), v(3).as_f64())
    }

    #[test]
    fn near_finds_the_nearest_feature_its_distance_and_point() {
        let m = inputs();
        let out = layer(crate::run("near", &m, &serde_json::json!({"input": "pts", "near": "polys", "units": "Meters"})).unwrap());
        assert_eq!(out.name, "pts_Near");
        let names: Vec<&str> = out.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["v", "near_fid", "near_dist", "near_x", "near_y"]);
        assert_eq!(out.features.len(), 3);
        // Inside the first square: no distance, and the point itself is the nearest point.
        assert_eq!(near_fields(&out, 0), (Some(1), Some(0.0), Some(0.5), Some(0.5)));
        // (1.5, 1.5) is in both overlapping squares: the first wins the tie.
        assert_eq!(near_fields(&out, 1).0, Some(1));
        // (50, 50) is nearest the third square's far corner, (11, 11), on the ground.
        let (fid, d, x, y) = near_fields(&out, 2);
        assert_eq!(fid, Some(3));
        let want = rustgis_geom::wgs84_distance_m(Coord::new(50.0, 50.0), Coord::new(11.0, 11.0));
        assert!((d.unwrap() - want).abs() < 1e-4 * want, "{d:?} vs {want}");
        assert!((x.unwrap() - 11.0).abs() < 1e-7 && (y.unwrap() - 11.0).abs() < 1e-7, "{x:?} {y:?}");
    }

    /// Features searched on several threads find the same nearest features, distances and points
    /// as on one thread, in input order (#1252), the same layer as both included.
    #[test]
    fn near_on_several_threads_is_near_on_one() {
        let mut targets = Dataset::new("targets", GeometryType::Point, vec![Field::new("v", FieldType::Integer)]);
        let mut near = Dataset::new("near", GeometryType::Polygon, vec![Field::new("k", FieldType::Integer)]);
        for i in 0..1500i64 {
            let (x, y) = ((i * 37 % 359) as f64 - 179.0, (i * 53 % 160) as f64 - 80.0);
            targets.push(if i % 97 == 0 { None } else { Some(Geometry::Point(Coord::new(x, y))) }, vec![Value::Int(i)]);
        }
        for i in 0..300i64 {
            near.push(Some(sq((i * 71 % 350) as f64 - 175.0, (i * 29 % 150) as f64 - 75.0, 0.5)), vec![Value::Int(i)]);
        }
        let m = Map(HashMap::from([("targets".to_string(), targets), ("near".to_string(), near)]));
        for p in [
            serde_json::json!({"input": "targets", "near": "near", "units": "Meters"}),
            serde_json::json!({"input": "targets", "near": "near", "distance": 500, "units": "Kilometers"}),
            serde_json::json!({"input": "targets", "near": "targets"}),
        ] {
            let one = layer(near_on(&m, &p, Some(1)).unwrap());
            assert!(one.features.iter().any(|f| f.attrs[1] != Value::Null), "{p}");
            for threads in [2, 7, 16] {
                let many = layer(near_on(&m, &p, Some(threads)).unwrap());
                assert_eq!(many.features.len(), one.features.len());
                for (a, b) in one.features.iter().zip(&many.features) {
                    assert_eq!(format!("{:?}", a.attrs), format!("{:?}", b.attrs), "{threads} threads, {p}");
                }
            }
        }
    }

    #[test]
    fn near_leaves_features_beyond_the_search_distance_empty() {
        let m = inputs();
        let out = layer(crate::run("near", &m, &serde_json::json!({"input": "pts", "near": "polys", "distance": 100})).unwrap());
        assert_eq!(near_fields(&out, 0).0, Some(1));
        assert_eq!(near_fields(&out, 2), (None, None, None, None), "(50, 50) is thousands of km away");
        for bad in [serde_json::json!(-1), serde_json::json!("far")] {
            assert!(crate::run("near", &m, &serde_json::json!({"input": "pts", "near": "polys", "distance": bad})).is_err());
        }
    }

    #[test]
    fn near_within_one_layer_skips_the_feature_itself() {
        let m = inputs();
        let out = layer(crate::run("near", &m, &serde_json::json!({"input": "pts", "near": "pts"})).unwrap());
        // Each point's nearest is another point, not itself at no distance.
        assert_eq!(near_fields(&out, 0).0, Some(2));
        assert_eq!(near_fields(&out, 1).0, Some(1));
        assert!(near_fields(&out, 0).1.is_some_and(|d| d > 0.0));
    }

    #[test]
    fn near_gives_a_shared_point_for_crossing_lines() {
        let line = |a: (f64, f64), b: (f64, f64)| Geometry::Line(vec![vec![Coord::new(a.0, a.1), Coord::new(b.0, b.1)]]);
        let mut a = Dataset::new("a", GeometryType::Polyline, vec![]);
        a.push(Some(line((0.0, 0.0), (2.0, 2.0))), vec![]);
        let mut b = Dataset::new("b", GeometryType::Polyline, vec![]);
        b.push(Some(line((0.0, 2.0), (2.0, 0.0))), vec![]);
        let m = Map(HashMap::from([("a".to_string(), a), ("b".to_string(), b)]));
        let out = layer(crate::run("near", &m, &serde_json::json!({"input": "a", "near": "b"})).unwrap());
        let (fid, d, x, y) = near_fields(&out, 0);
        assert_eq!((fid, d), (Some(1), Some(0.0)));
        // Where they cross: (1, 1), give or take the map's straight lines.
        assert!((x.unwrap() - 1.0).abs() < 1e-3 && (y.unwrap() - 1.0).abs() < 1e-3, "{x:?} {y:?}");
    }

    /// Layers with a selection in each, as the Tools pane passes them.
    struct Selected(Map, HashMap<String, Vec<usize>>);
    impl Inputs for Selected {
        fn dataset(&self, layer: &str) -> Option<&Dataset> {
            self.0.dataset(layer)
        }
        fn selection(&self, layer: &str) -> &[usize] {
            self.1.get(layer).map_or(&[], Vec::as_slice)
        }
    }

    #[test]
    fn intersect_honours_both_layers_selections() {
        let mut parcels_and_zones = parcels_and_zones();
        // A zone covering p2, which only the selection would leave out.
        let z3 = Some(sq(4.0, 0.0, 1.0));
        parcels_and_zones.0.get_mut("zones").unwrap().push(z3, vec![Value::Text("z3".into()), Value::Null]);
        let m = Selected(parcels_and_zones, HashMap::from([("parcels".to_string(), vec![0]), ("zones".to_string(), vec![1, 2])]));
        let out = layer(crate::run("intersect", &m, &serde_json::json!({"input": "parcels", "overlay": "zones"})).unwrap());
        // p1 meets only z1 (not selected); p2 meets z3 but isn't selected.
        assert!(out.features.is_empty(), "{:?}", out.features);
        // Selecting p2 too gives its part in z3, and only that.
        let m = Selected(m.0, HashMap::from([("parcels".to_string(), vec![0, 1]), ("zones".to_string(), vec![1, 2])]));
        let out = layer(crate::run("intersect", &m, &serde_json::json!({"input": "parcels", "overlay": "zones"})).unwrap());
        let ids: Vec<&Value> = out.features.iter().flat_map(|f| [&f.attrs[0], &f.attrs[2]]).collect();
        assert_eq!(ids, [&Value::Text("p2".into()), &Value::Text("z3".into())]);
    }

    #[test]
    fn identity_and_union_honour_both_layers_selections() {
        // Only z2 is selected: it touches p1 without overlapping it, so z1 doesn't cut p1.
        let m = Selected(parcels_and_zones(), HashMap::from([("zones".to_string(), vec![1])]));
        let out = layer(crate::run("identity", &m, &serde_json::json!({"input": "parcels", "overlay": "zones"})).unwrap());
        assert_eq!(overlay_keys(&out), ["p1|", "p2|"]);
        assert_eq!(out.features[0].geometry, m.0.0["parcels"].features[0].geometry);
        // Only p2 is selected: it touches z2 without overlapping it, so p1 doesn't cover z1.
        let m = Selected(parcels_and_zones(), HashMap::from([("parcels".to_string(), vec![1])]));
        let out = layer(crate::run("union", &m, &serde_json::json!({"input": "parcels", "overlay": "zones"})).unwrap());
        assert_eq!(overlay_keys(&out), ["p2|", "|z1", "|z2"]);
        // z1 whole (back from Web Mercator, so not to the last bit).
        let (got, want) = (out.features[1].geometry.as_ref().unwrap().extent(), sq(1.0, 0.0, 2.0).extent());
        for (g, w) in [(got.min_x, want.min_x), (got.max_x, want.max_x), (got.min_y, want.min_y), (got.max_y, want.max_y)] {
            assert!((g - w).abs() < 1e-9, "{got:?}");
        }
    }

    /// A point layer named `name` with a feature at each lon/lat in `at`, ids 1, 2, ….
    fn points(name: &str, at: &[(f64, f64)]) -> Dataset {
        let mut ds = Dataset::new(name, GeometryType::Point, vec![]);
        for (x, y) in at {
            ds.push_next(Some(Geometry::Point(Coord::new(*x, *y))), vec![]);
        }
        ds
    }

    #[test]
    fn near_searches_the_grid_and_breaks_ties_in_layer_order() {
        // Enough points for a grid: a row 1° apart on the equator, and one far along it.
        let mut at: Vec<(f64, f64)> = (0..20).map(|i| (f64::from(i), 0.0)).collect();
        at.push((100.0, 0.0));
        let m = Map(HashMap::from([("row".to_string(), points("row", &at))]));
        let out = layer(crate::run("near", &m, &serde_json::json!({"input": "row", "near": "row"})).unwrap());
        // The first point's nearest is the second, not itself.
        assert_eq!(near_fields(&out, 0).0, Some(2));
        // The eleventh (10°) is as near the tenth as the twelfth: the tenth wins.
        let (fid, d, _, _) = near_fields(&out, 10);
        assert_eq!(fid, Some(10));
        let degree = rustgis_geom::wgs84_distance_m(Coord::new(0.0, 0.0), Coord::new(1.0, 0.0)) / 1000.0;
        assert!((d.unwrap() - degree).abs() < 1e-6 * degree, "{d:?} km vs {degree} km");
        // The far point's search widens until it reaches the last of the row, 81° away.
        assert_eq!(near_fields(&out, 20).0, Some(20));
    }

    #[test]
    fn near_measures_across_the_antimeridian() {
        let m = Map(HashMap::from([
            ("a".to_string(), points("a", &[(179.9, 0.0)])),
            ("b".to_string(), points("b", &[(178.0, 0.0), (-179.9, 0.0)])),
        ]));
        let out = layer(crate::run("near", &m, &serde_json::json!({"input": "a", "near": "b", "units": "Meters"})).unwrap());
        let (fid, d, x, y) = near_fields(&out, 0);
        assert_eq!(fid, Some(2), "0.2° away across ±180°, not 1.9° on this side");
        let want = rustgis_geom::wgs84_distance_m(Coord::new(179.9, 0.0), Coord::new(-179.9, 0.0));
        assert!((d.unwrap() - want).abs() < 1e-6 * want, "{d:?} vs {want}");
        assert!((x.unwrap() + 179.9).abs() < 1e-7 && y.unwrap().abs() < 1e-7, "{x:?} {y:?}");
    }

    #[test]
    fn near_gives_a_shared_point_within_180_degrees_of_the_meridian() {
        let mut polys = Dataset::new("polys", GeometryType::Polygon, vec![]);
        polys.push(Some(sq(190.0, -1.0, 20.0)), vec![]);
        let m = Map(HashMap::from([("a".to_string(), points("a", &[(200.0, 0.0)])), ("b".to_string(), polys)]));
        let out = layer(crate::run("near", &m, &serde_json::json!({"input": "a", "near": "b"})).unwrap());
        let (fid, d, x, y) = near_fields(&out, 0);
        assert_eq!((fid, d), (Some(1), Some(0.0)));
        // 200° is the meridian of −160°: the shared point comes back in [−180°, 180°) as the nearest points do.
        assert!((x.unwrap() + 160.0).abs() < 1e-7 && y.unwrap().abs() < 1e-7, "{x:?} {y:?}");
    }

    #[test]
    fn near_gives_a_point_for_the_same_place_on_both_sides_of_the_antimeridian() {
        let m = Map(HashMap::from([("a".to_string(), points("a", &[(180.0, 0.0)])), ("b".to_string(), points("b", &[(-180.0, 0.0)]))]));
        let out = layer(crate::run("near", &m, &serde_json::json!({"input": "a", "near": "b", "units": "Meters"})).unwrap());
        let (fid, d, x, y) = near_fields(&out, 0);
        assert_eq!(fid, Some(1));
        assert!(d.is_some_and(|d| d.abs() < 1e-6), "{d:?}");
        assert!(x.is_some_and(|x| (x.abs() - 180.0).abs() < 1e-7) && y.is_some_and(|y| y.abs() < 1e-7), "{x:?} {y:?}");
    }

    #[test]
    fn near_finds_the_nearest_point_along_a_line_within_the_search_distance() {
        let line = |a: (f64, f64), b: (f64, f64)| Geometry::Line(vec![vec![Coord::new(a.0, a.1), Coord::new(b.0, b.1)]]);
        let mut roads = Dataset::new("roads", GeometryType::Polyline, vec![]);
        roads.push(Some(line((-1.0, 0.0), (1.0, 0.0))), vec![]);
        roads.push(Some(line((0.0, 5.0), (1.0, 5.0))), vec![]);
        let m = Map(HashMap::from([("pts".to_string(), points("pts", &[(0.0, 1.0)])), ("roads".to_string(), roads)]));
        let run = |p: serde_json::Value| layer(crate::run("near", &m, &p).unwrap());
        // A degree north of the first road's middle: the point below it, about 110.6 km away.
        let (fid, d, x, y) = near_fields(&run(serde_json::json!({"input": "pts", "near": "roads"})), 0);
        assert_eq!(fid, Some(1));
        let want = rustgis_geom::wgs84_distance_m(Coord::new(0.0, 1.0), Coord::new(0.0, 0.0)) / 1000.0;
        assert!((d.unwrap() - want).abs() < 1e-4 * want, "{d:?} vs {want}");
        // Found on the plane's straight edge, within 0.01 % of the distance of the geodesic's point.
        assert!(x.unwrap().abs() < 1e-4 && y.unwrap().abs() < 1e-4, "{x:?} {y:?}");
        let far = run(serde_json::json!({"input": "pts", "near": "roads", "distance": 100}));
        assert_eq!(near_fields(&far, 0), (None, None, None, None));
        // Only the second road selected: it is the nearest.
        let m = Selected(m, HashMap::from([("roads".to_string(), vec![1])]));
        let out = layer(crate::run("near", &m, &serde_json::json!({"input": "pts", "near": "roads"})).unwrap());
        assert_eq!(near_fields(&out, 0).0, Some(2));
    }

    #[test]
    fn near_gives_a_point_in_the_overlap_of_polygons() {
        let m = inputs();
        let out = layer(crate::run("near", &m, &serde_json::json!({"input": "polys", "near": "polys"})).unwrap());
        // The first two squares overlap (x 1–2): each is the other's nearest, at a point they share.
        for r in [0, 1] {
            let (fid, d, x, y) = near_fields(&out, r);
            assert_eq!((fid, d), (Some(2 - r as i64), Some(0.0)));
            let (x, y) = (x.unwrap(), y.unwrap());
            assert!((1.0 - 1e-9..=2.0 + 1e-9).contains(&x) && (-1e-9..=2.0 + 1e-9).contains(&y), "{x} {y}");
        }
    }

    #[test]
    fn select_by_location_says_when_selecting_features_lack_geometry() {
        let mut polys = Dataset::new("polys", GeometryType::Polygon, vec![]);
        polys.push(Some(sq(0.0, 0.0, 2.0)), vec![]);
        polys.push(None, vec![]); // a selector without geometry
        let mut pts = Dataset::new("pts", GeometryType::Point, vec![]);
        pts.push(Some(Geometry::Point(Coord::new(1.0, 1.0))), vec![]);
        pts.push(Some(Geometry::Point(Coord::new(10.0, 10.0))), vec![]);
        let m = Map(HashMap::from([("polys".to_string(), polys), ("pts".to_string(), pts)]));
        let res = crate::run("selectByLocation", &m, &serde_json::json!({"input": "pts", "select": "polys"})).unwrap();
        // One notice for it, and the selector with geometry still selects.
        assert_eq!(res.messages, vec!["Selected 1 features in pts", "1 of 2 selecting features lack geometry, so they select nothing"]);
        match res.output {
            Output::Selection { rows, .. } => assert_eq!(rows, vec![0]),
            _ => panic!("expected selection"),
        }
    }

    #[test]
    fn select_by_location_invert_excludes_features_without_geometry() {
        let mut polys = Dataset::new("polys", GeometryType::Polygon, vec![]);
        polys.push(Some(sq(0.0, 0.0, 2.0)), vec![]);
        let mut pts = Dataset::new("pts", GeometryType::Point, vec![]);
        pts.push(Some(Geometry::Point(Coord::new(1.0, 1.0))), vec![]); // inside
        pts.push(Some(Geometry::Point(Coord::new(10.0, 10.0))), vec![]); // outside
        pts.push(None, vec![]); // no geometry (unplaced record)
        let m = Map(HashMap::from([("polys".to_string(), polys), ("pts".to_string(), pts)]));

        let res_normal = crate::run("selectByLocation", &m, &serde_json::json!({"input": "pts", "select": "polys"})).unwrap();
        // The point without geometry is said, once; a selecting layer with geometry everywhere isn't.
        assert_eq!(res_normal.messages.len(), 2);
        assert_eq!(res_normal.messages.get(1).map(String::as_str), Some("1 of 3 features lack geometry, so they are not selected"));
        match res_normal.output {
            Output::Selection { rows, .. } => assert_eq!(rows, vec![0], "normal select should only select intersecting feature"),
            _ => panic!("expected selection"),
        }

        let res_inverted =
            crate::run("selectByLocation", &m, &serde_json::json!({"input": "pts", "select": "polys", "invert": "Yes"})).unwrap();
        match res_inverted.output {
            Output::Selection { rows, .. } => {
                assert_eq!(rows, vec![1], "inverted select should exclude unplaced records with null geometry")
            }
            _ => panic!("expected selection"),
        }
    }

    #[test]
    fn spatial_join_cases() {
        let mut polys = Dataset::new("polys", GeometryType::Polygon, vec![Field::new("k", FieldType::Text)]);
        polys.push(Some(sq(0.0, 0.0, 2.0)), vec![Value::Text("a".into())]);
        polys.push(Some(sq(1.0, 0.0, 2.0)), vec![Value::Text("b".into())]);

        let mut pts = Dataset::new("pts", GeometryType::Point, vec![Field::new("name", FieldType::Text), Field::new("k", FieldType::Text)]);
        pts.push(Some(Geometry::Point(Coord::new(1.5, 1.0))), vec![Value::Text("both".into()), Value::Text("orig".into())]);
        pts.push(Some(Geometry::Point(Coord::new(0.5, 0.5))), vec![Value::Text("first".into()), Value::Text("orig".into())]);
        pts.push(Some(Geometry::Point(Coord::new(50.0, 50.0))), vec![Value::Text("outside".into()), Value::Text("orig".into())]);

        let m = Map(HashMap::from([("polys".to_string(), polys), ("pts".to_string(), pts)]));

        // 1. One to one join (Intersects)
        let r1 = crate::run("spatialJoin", &m, &serde_json::json!({"target": "pts", "join": "polys"})).unwrap();
        let ds1 = layer(r1);
        assert_eq!(ds1.features.len(), 3);
        let jc = ds1.field_index("join_count").unwrap();
        let jk = ds1.field_index("join_k").unwrap();
        assert_eq!(ds1.value(0, jc), &Value::Int(2));
        assert!(matches!(ds1.value(0, jk), Value::Text(_)));
        assert_eq!(ds1.value(1, jc), &Value::Int(1));
        assert_eq!(ds1.value(1, jk), &Value::Text("a".into()));
        assert_eq!(ds1.value(2, jc), &Value::Int(0));
        assert_eq!(ds1.value(2, jk), &Value::Null);

        // 2. One to many join
        let r2 =
            crate::run("spatialJoin", &m, &serde_json::json!({"target": "pts", "join": "polys", "operation": "Join one to many"})).unwrap();
        let ds2 = layer(r2);
        assert_eq!(ds2.features.len(), 4);

        // 3. Inner join (Keep only matching)
        let r3 =
            crate::run("spatialJoin", &m, &serde_json::json!({"target": "pts", "join": "polys", "keepAll": "Keep only matching"})).unwrap();
        let ds3 = layer(r3);
        assert_eq!(ds3.features.len(), 2);

        // 4. Match option Contains (target polys, join pts)
        let r4 = crate::run("spatialJoin", &m, &serde_json::json!({"target": "polys", "join": "pts", "match": "Contains"})).unwrap();
        let ds4 = layer(r4);
        assert_eq!(ds4.features.len(), 2);
        let jc4 = ds4.field_index("join_count").unwrap();
        assert_eq!(ds4.value(0, jc4), &Value::Int(2));
        assert_eq!(ds4.value(1, jc4), &Value::Int(1));

        // 5. Match option Within (target pts, join polys)
        let r5 = crate::run("spatialJoin", &m, &serde_json::json!({"target": "pts", "join": "polys", "match": "Within"})).unwrap();
        let ds5 = layer(r5);
        assert_eq!(ds5.features.len(), 3);
        let jc5 = ds5.field_index("join_count").unwrap();
        assert_eq!(ds5.value(0, jc5), &Value::Int(2));
        assert_eq!(ds5.value(1, jc5), &Value::Int(1));
        assert_eq!(ds5.value(2, jc5), &Value::Int(0));

        // 6. Custom/empty prefix and field name collision deduplication
        let r6 = crate::run("spatialJoin", &m, &serde_json::json!({"target": "pts", "join": "polys", "prefix": ""})).unwrap();
        let ds6 = layer(r6);
        assert!(ds6.field_index("k").is_some());
        assert!(ds6.field_index("k_2").is_some(), "colliding field k should be deduplicated to k_2");
        assert_eq!(ds6.value(1, ds6.field_index("k").unwrap()), &Value::Text("orig".into()));
        assert_eq!(ds6.value(1, ds6.field_index("k_2").unwrap()), &Value::Text("a".into()));

        // 7. Indexed path (> 16 join geometries) for Intersects, Contains, and Within, including outside extent
        let mut grid_polys = Dataset::new("grid_polys", GeometryType::Polygon, vec![Field::new("id", FieldType::Integer)]);
        for y in 0..5 {
            for x in 0..5 {
                grid_polys.push(Some(sq(x as f64, y as f64, 1.0)), vec![Value::Int(y * 5 + x)]);
            }
        }
        assert_eq!(grid_polys.features.len(), 25);

        let mut test_pts = Dataset::new("test_pts", GeometryType::Point, vec![Field::new("tag", FieldType::Text)]);
        test_pts.push(Some(Geometry::Point(Coord::new(0.5, 0.5))), vec![Value::Text("in_0".into())]);
        test_pts.push(Some(Geometry::Point(Coord::new(4.5, 4.5))), vec![Value::Text("in_24".into())]);
        test_pts.push(Some(Geometry::Point(Coord::new(100.0, 100.0))), vec![Value::Text("far".into())]);

        let m2 = Map(HashMap::from([("grid_polys".to_string(), grid_polys), ("test_pts".to_string(), test_pts)]));

        let r_int = crate::run("spatialJoin", &m2, &serde_json::json!({"target": "test_pts", "join": "grid_polys", "match": "Intersects"}))
            .unwrap();
        let ds_int = layer(r_int);
        assert_eq!(ds_int.features.len(), 3);
        let jc_int = ds_int.field_index("join_count").unwrap();
        let jid_int = ds_int.field_index("join_id").unwrap();
        assert_eq!(ds_int.value(0, jc_int), &Value::Int(1));
        assert_eq!(ds_int.value(0, jid_int), &Value::Int(0));
        assert_eq!(ds_int.value(1, jc_int), &Value::Int(1));
        assert_eq!(ds_int.value(1, jid_int), &Value::Int(24));
        assert_eq!(ds_int.value(2, jc_int), &Value::Int(0));
        assert_eq!(ds_int.value(2, jid_int), &Value::Null);

        let r_within =
            crate::run("spatialJoin", &m2, &serde_json::json!({"target": "test_pts", "join": "grid_polys", "match": "Within"})).unwrap();
        let ds_within = layer(r_within);
        assert_eq!(ds_within.value(0, jc_int), &Value::Int(1));
        assert_eq!(ds_within.value(1, jc_int), &Value::Int(1));
        assert_eq!(ds_within.value(2, jc_int), &Value::Int(0));

        let r_contains =
            crate::run("spatialJoin", &m2, &serde_json::json!({"target": "grid_polys", "join": "test_pts", "match": "Contains"})).unwrap();
        let ds_contains = layer(r_contains);
        assert_eq!(ds_contains.features.len(), 25);
        let jc_poly = ds_contains.field_index("join_count").unwrap();
        assert_eq!(ds_contains.value(0, jc_poly), &Value::Int(1));
        assert_eq!(ds_contains.value(24, jc_poly), &Value::Int(1));
        assert_eq!(ds_contains.value(12, jc_poly), &Value::Int(0));

        // Source CRS metadata must not affect spatial relationships: geometry is already lon/lat.
        let Some(pts_layer) = m.0.get("pts") else { panic!("missing pts") };
        let Some(polys_layer) = m.0.get("polys") else { panic!("missing polys") };
        let mut pts_crs = pts_layer.clone();
        pts_crs.crs = Some(rustgis_data::raster::Crs::LonLat);
        let mut polys_crs = polys_layer.clone();
        polys_crs.crs = Some(rustgis_data::raster::Crs::WebMercator);
        let m_crs = Map(HashMap::from([("polys".to_string(), polys_crs), ("pts".to_string(), pts_crs)]));
        for (params, expected) in [
            (serde_json::json!({"target": "pts", "join": "polys"}), &ds1),
            (serde_json::json!({"target": "pts", "join": "polys", "match": "Within"}), &ds5),
            (serde_json::json!({"target": "polys", "join": "pts", "match": "Contains"}), &ds4),
        ] {
            let actual = layer(crate::run("spatialJoin", &m_crs, &params).unwrap());
            assert_eq!(actual.fields, expected.fields);
            assert_eq!(actual.features, expected.features);
            let target = m_crs.0.get(params["target"].as_str().unwrap()).unwrap();
            assert_eq!(actual.crs, target.crs);
            assert_eq!(actual.source, None);
        }
    }

    /// Zones split by a boundary at x = 0.3 (zone "A" west of it, listed first, "B" east), parcels
    /// straddling it 30/70, and enough distant zones to use the grid index when `padded`.
    fn split_zones(padded: bool) -> Map {
        let rect = |x0: f64, x1: f64, y0: f64, y1: f64| {
            Geometry::Polygon(vec![vec![vec![
                Coord::new(x0, y0),
                Coord::new(x1, y0),
                Coord::new(x1, y1),
                Coord::new(x0, y1),
                Coord::new(x0, y0),
            ]]])
        };
        let mut zones = Dataset::new("zones", GeometryType::Polygon, vec![Field::new("zone", FieldType::Text)]);
        zones.push(Some(rect(0.0, 0.3, 0.0, 1.0)), vec![Value::Text("A".into())]);
        zones.push(Some(rect(0.3, 1.0, 0.0, 1.0)), vec![Value::Text("B".into())]);
        if padded {
            for k in 0..18 {
                let x = 10.0 + f64::from(k);
                zones.push(Some(rect(x, x + 0.5, 10.0, 10.5)), vec![Value::Text(format!("far{k}"))]);
            }
        }
        let mut parcels = Dataset::new("parcels", GeometryType::Polygon, vec![Field::new("pid", FieldType::Integer)]);
        for (pid, y) in [(1, 0.1), (2, 0.4), (3, 0.7)] {
            parcels.push(Some(rect(0.27, 0.37, y, y + 0.1)), vec![Value::Int(pid)]);
        }
        // Touching zone B along its east edge only, and outside every zone.
        parcels.push(Some(rect(1.0, 1.1, 0.1, 0.2)), vec![Value::Int(4)]);
        parcels.push(Some(rect(5.0, 5.1, 5.0, 5.1)), vec![Value::Int(5)]);
        let mut roads = Dataset::new("roads", GeometryType::Polyline, vec![]);
        roads.push(Some(Geometry::Line(vec![vec![Coord::new(0.1, 0.5), Coord::new(0.4, 0.5)]])), vec![]);
        roads.push(Some(Geometry::Line(vec![vec![Coord::new(0.2, 0.5), Coord::new(0.6, 0.5)]])), vec![]);
        let pts = inputs().0.remove("pts").unwrap_or_else(|| panic!("pts"));
        Map(HashMap::from([
            ("zones".to_string(), zones),
            ("parcels".to_string(), parcels),
            ("roads".to_string(), roads),
            ("pts".to_string(), pts),
        ]))
    }

    /// A text attribute of a joined row.
    fn text_at(ds: &Dataset, row: usize, field: &str) -> Option<String> {
        match ds.value(row, ds.field_index(field)?) {
            Value::Text(t) => Some(t.clone()),
            _ => None,
        }
    }

    #[test]
    fn spatial_join_largest_overlap_and_centre_in() {
        for padded in [false, true] {
            let m = split_zones(padded);
            let run = |params: serde_json::Value| layer(crate::run("spatialJoin", &m, &params).unwrap_or_else(|e| panic!("{e}")));
            let zone = |ds: &Dataset, row| text_at(ds, row, "join_zone");
            // Intersects matches both zones and keeps the first: the 30% zone.
            let any = run(serde_json::json!({"target": "parcels", "join": "zones"}));
            assert_eq!(zone(&any, 0).as_deref(), Some("A"));
            assert_eq!(any.value(0, any.field_index("join_count").unwrap()), &Value::Int(2));
            // Largest overlap joins the zone covering 70%, once.
            let most = run(serde_json::json!({"target": "parcels", "join": "zones", "match": "Largest overlap"}));
            let count = most.field_index("join_count").unwrap();
            for row in 0..3 {
                assert_eq!(zone(&most, row).as_deref(), Some("B"), "padded {padded}, row {row}");
                assert_eq!(most.value(row, count), &Value::Int(1));
            }
            // Touching shares no area, so it isn't an overlap; nor is being outside.
            assert_eq!((most.value(3, count), most.value(4, count)), (&Value::Int(0), &Value::Int(0)));
            assert_eq!(zone(&most, 3), None);
            // A line goes with the zone holding most of its length: 0.2 of 0.3 lies in A for the
            // first road, 0.3 of 0.4 in B for the second.
            let roads = run(serde_json::json!({"target": "roads", "join": "zones", "match": "Largest overlap"}));
            assert_eq!((zone(&roads, 0).as_deref(), zone(&roads, 1).as_deref()), (Some("A"), Some("B")));
            // Have their center in uses Feature To Point's point.
            let centre = run(serde_json::json!({"target": "parcels", "join": "zones", "match": "Have their center in"}));
            let points = layer(crate::run("featureToPoint", &m, &serde_json::json!({"input": "parcels"})).unwrap());
            let pm = Map(HashMap::from([("pp".to_string(), points), ("zones".to_string(), m.0["zones"].clone())]));
            let via_points = layer(crate::run("spatialJoin", &pm, &serde_json::json!({"target": "pp", "join": "zones"})).unwrap());
            for row in 0..5 {
                assert_eq!(zone(&centre, row), text_at(&via_points, row, "join_zone"), "padded {padded}, row {row}");
            }
            assert_eq!(zone(&centre, 0).as_deref(), Some("B"));
            // Largest overlap needs polygons to join, and lines or polygons to receive them.
            assert!(
                crate::run("spatialJoin", &m, &serde_json::json!({"target": "pts", "join": "zones", "match": "Largest overlap"})).is_err()
            );
            assert!(
                crate::run("spatialJoin", &m, &serde_json::json!({"target": "zones", "join": "roads", "match": "Largest overlap"}))
                    .is_err()
            );
        }
    }

    #[test]
    fn spatial_join_closest_and_within_a_distance() {
        let point = |x: f64, y: f64| Some(Geometry::Point(Coord::new(x, y)));
        let mut stops = Dataset::new("stops", GeometryType::Point, vec![Field::new("stop", FieldType::Text)]);
        for (name, x, y) in [("s0", 0.0, 0.0), ("s1", 0.01, 0.0), ("s2", 0.05, 0.0), ("east", -179.999, 0.0), ("pole", 180.0, 89.9)] {
            stops.push(point(x, y), vec![Value::Text(name.into())]);
        }
        let mut homes = Dataset::new("homes", GeometryType::Point, vec![Field::new("h", FieldType::Text)]);
        for (name, x, y) in [("near s0", 0.004, 0.0), ("west of east", 179.999, 0.0), ("polar", 0.0, 89.9)] {
            homes.push(point(x, y), vec![Value::Text(name.into())]);
        }
        let mut road = Dataset::new("road", GeometryType::Polyline, vec![Field::new("r", FieldType::Text)]);
        road.push(Some(Geometry::Line(vec![vec![Coord::new(-1.0, 1.0), Coord::new(1.0, 1.0)]])), vec![Value::Text("main".into())]);
        let mut block = Dataset::new("block", GeometryType::Polygon, vec![]);
        block.push(Some(sq(-0.001, -0.001, 0.002)), vec![]);
        let m = Map(HashMap::from([
            ("stops".to_string(), stops),
            ("homes".to_string(), homes),
            ("road".to_string(), road),
            ("block".to_string(), block),
        ]));
        let run = |params: serde_json::Value| layer(crate::run("spatialJoin", &m, &params).unwrap_or_else(|e| panic!("{e}")));
        let num_at = |ds: &Dataset, row: usize, f: &str| ds.value(row, ds.field_index(f).unwrap()).as_f64();
        // Distances on WGS 84, as the join measures them (#888).
        let hav = |a: (f64, f64), b: (f64, f64)| rustgis_geom::wgs84_distance_m(Coord::new(a.0, a.1), Coord::new(b.0, b.1));

        // Closest, unlimited: the nearest stop, across the antimeridian and over the pole too.
        let closest =
            run(serde_json::json!({"target": "homes", "join": "stops", "match": "Closest", "units": "Meters", "distanceField": "dist"}));
        assert_eq!(text_at(&closest, 0, "join_stop").as_deref(), Some("s0"));
        assert_eq!(text_at(&closest, 1, "join_stop").as_deref(), Some("east"));
        assert_eq!(text_at(&closest, 2, "join_stop").as_deref(), Some("pole"));
        assert_eq!(closest.value(0, closest.field_index("join_count").unwrap()), &Value::Int(1));
        for (row, a, b) in [(0, (0.004, 0.0), (0.0, 0.0)), (1, (179.999, 0.0), (-179.999, 0.0)), (2, (0.0, 89.9), (180.0, 89.9))] {
            let d = num_at(&closest, row, "dist").unwrap();
            assert!((d - hav(a, b)).abs() < 1e-6 * hav(a, b), "row {row}: {d} vs {}", hav(a, b));
        }
        // Within the search distance only, in the units given; exactly at it counts.
        let s0 = hav((0.004, 0.0), (0.0, 0.0));
        let short = run(serde_json::json!({"target": "homes", "join": "stops", "match": "Closest", "distance": 300, "units": "Meters"}));
        assert_eq!(text_at(&short, 0, "join_stop"), None);
        let exact = run(serde_json::json!({"target": "homes", "join": "stops", "match": "Closest", "distance": s0, "units": "Meters"}));
        assert_eq!(text_at(&exact, 0, "join_stop").as_deref(), Some("s0"));
        let feet = run(
            serde_json::json!({"target": "homes", "join": "stops", "match": "Closest", "distance": 1500, "units": "Feet", "distanceField": "ft"}),
        );
        assert!((num_at(&feet, 0, "ft").unwrap() - s0 / 0.3048).abs() < 1e-3);
        // A line's nearest point can be between its vertices: north of the home, 2° of longitude
        // from the vertices. The edge is a geodesic, a little poleward of latitude 1 midway.
        let to_road =
            run(serde_json::json!({"target": "homes", "join": "road", "match": "Closest", "units": "Meters", "distanceField": "d"}));
        let (north, bulge) = (hav((0.004, 0.0), (0.004, 1.0)), hav((0.004, 0.0), (0.004, 1.000_153)));
        let d = num_at(&to_road, 0, "d").unwrap();
        assert!(d > north - 1.0 && d < bulge + 1.0, "{d} not in {north}..{bulge}");
        // A polygon touching a stop is no distance from it.
        let touching = run(serde_json::json!({"target": "block", "join": "stops", "match": "Closest", "distanceField": "d"}));
        assert_eq!(num_at(&touching, 0, "d"), Some(0.0));

        // Within a distance: every stop in range, nearest first, so one to one keeps the nearest.
        let within = run(
            serde_json::json!({"target": "homes", "join": "stops", "match": "Within a distance", "distance": 1, "distanceField": "km"}),
        );
        assert_eq!(within.value(0, within.field_index("join_count").unwrap()), &Value::Int(2));
        assert_eq!(text_at(&within, 0, "join_stop").as_deref(), Some("s0"));
        let many = run(
            serde_json::json!({"target": "homes", "join": "stops", "match": "Within a distance", "distance": 1, "operation": "Join one to many", "distanceField": "km", "keepAll": "Keep only matching"}),
        );
        let names: Vec<_> = (0..many.features.len()).filter_map(|r| text_at(&many, r, "join_stop")).collect();
        // (The polar stop is 22 km away.)
        assert_eq!(names, ["s0", "s1", "east"]);
        assert!(num_at(&many, 0, "km").unwrap() < num_at(&many, 1, "km").unwrap());
        assert!((num_at(&many, 1, "km").unwrap() - hav((0.004, 0.0), (0.01, 0.0)) / 1000.0).abs() < 1e-9);

        // Refused: no distance for Within a distance, a negative one, and distance options where
        // nothing is measured.
        for bad in [
            serde_json::json!({"target": "homes", "join": "stops", "match": "Within a distance"}),
            serde_json::json!({"target": "homes", "join": "stops", "match": "Closest", "distance": -1}),
            serde_json::json!({"target": "homes", "join": "stops", "match": "Closest", "distance": "far"}),
            serde_json::json!({"target": "homes", "join": "stops", "distance": 1}),
            serde_json::json!({"target": "homes", "join": "stops", "distanceField": "d"}),
        ] {
            assert!(crate::run("spatialJoin", &m, &bad).is_err(), "{bad}");
        }
        // Enough stops for the grid index: Closest widens its search until it finds one, and agrees
        // with measuring them all, near the stops, far from them and across the antimeridian.
        let mut many = Dataset::new("many", GeometryType::Point, vec![Field::new("i", FieldType::Integer)]);
        for i in 0..40 {
            let x = if i == 39 { 179.5 } else { f64::from(i) * 0.5 };
            many.push(point(x, (f64::from(i) * 0.37).sin()), vec![Value::Int(i64::from(i))]);
        }
        let mut probes = Dataset::new("probes", GeometryType::Point, vec![]);
        let spots = [(3.1, 0.2), (19.6, -0.8), (60.0, 0.0), (-179.8, 0.5), (10.0, 45.0), (5.0, -89.0)];
        for (x, y) in spots {
            probes.push(point(x, y), vec![]);
        }
        let brute = |x: f64, y: f64| {
            let d = |f: &rustgis_data::Feature| match &f.geometry {
                Some(Geometry::Point(c)) => rustgis_geom::wgs84_distance_m(Coord::new(x, y), *c),
                _ => f64::INFINITY,
            };
            let best = many.features.iter().min_by(|a, b| d(a).total_cmp(&d(b))).map(|f| f.attrs[0].clone());
            let within: Vec<Value> = many.features.iter().filter(|f| d(f) <= 600_000.0).map(|f| f.attrs[0].clone()).collect();
            (best, within)
        };
        let m3 = Map(HashMap::from([("many".to_string(), many.clone()), ("probes".to_string(), probes)]));
        let run3 = |params: serde_json::Value| layer(crate::run("spatialJoin", &m3, &params).unwrap_or_else(|e| panic!("{e}")));
        let closest = run3(serde_json::json!({"target": "probes", "join": "many", "match": "Closest"}));
        let wide = run3(
            serde_json::json!({"target": "probes", "join": "many", "match": "Within a distance", "distance": 600, "operation": "Join one to many"}),
        );
        for (row, (x, y)) in spots.iter().enumerate() {
            let (best, within) = brute(*x, *y);
            assert_eq!(Some(closest.value(row, closest.field_index("join_i").unwrap()).clone()), best, "probe {row}");
            let mut found: Vec<Value> = (0..wide.features.len())
                .filter(|r| wide.features[*r].geometry == m3.0["probes"].features[row].geometry)
                .map(|r| wide.value(r, wide.field_index("join_i").unwrap()).clone())
                .filter(|v| *v != Value::Null)
                .collect();
            let mut expected = within;
            let key = |v: &Value| v.as_f64().unwrap_or(f64::NAN);
            found.sort_by(|a, b| key(a).total_cmp(&key(b)));
            expected.sort_by(|a, b| key(a).total_cmp(&key(b)));
            assert_eq!(found, expected, "probe {row}");
        }

        // Join features all at one spot make a grid of cells of no size; Closest still finds them.
        let mut piled = Dataset::new("piled", GeometryType::Point, vec![Field::new("i", FieldType::Integer)]);
        for i in 0..20 {
            piled.push(point(1.0, 1.0), vec![Value::Int(i)]);
        }
        let m4 = Map(HashMap::from([("piled".to_string(), piled), ("homes".to_string(), m.0["homes"].clone())]));
        let near_pile = layer(
            crate::run(
                "spatialJoin",
                &m4,
                &serde_json::json!({"target": "homes", "join": "piled", "match": "Closest", "units": "Meters", "distanceField": "d"}),
            )
            .unwrap(),
        );
        assert_eq!(near_pile.value(0, near_pile.field_index("join_i").unwrap()), &Value::Int(0));
        let d = num_at(&near_pile, 0, "d").unwrap();
        assert!((d - hav((0.004, 0.0), (1.0, 1.0))).abs() < 1e-6 * d, "{d}");

        // A match option in any case, as the Tools pane reads it.
        assert_eq!(JoinMatch::parse(" closest "), JoinMatch::Closest);
        assert_eq!(JoinMatch::parse("HAVE THEIR CENTER IN"), JoinMatch::CentreIn);
        assert_eq!(JoinMatch::parse("nonsense"), JoinMatch::Intersects);

        // A blank distance is none (as the Tools pane sends an empty box).
        assert!(crate::run("spatialJoin", &m, &serde_json::json!({"target": "homes", "join": "stops", "distance": " "})).is_ok());
    }

    /// Within a distance through the join's grid index (more than 16 join features), at the edge
    /// of 10 km due north of the equator (#888): 0.0904369° is 9,999.995 m on WGS 84 but 10,056 m
    /// on the mean sphere, so a search worked out on that sphere left it out; 0.0905° is
    /// 10,006.972 m and stays out.
    #[test]
    fn spatial_join_within_a_distance_at_the_ellipsoids_edge() {
        let point = |x: f64, y: f64| Some(Geometry::Point(Coord::new(x, y)));
        let mut stops = Dataset::new("stops", GeometryType::Point, vec![Field::new("stop", FieldType::Text)]);
        stops.push(point(0.0, 0.090_436_9), vec![Value::Text("edge".into())]);
        stops.push(point(0.0, 0.0905), vec![Value::Text("past".into())]);
        // More stops, all over 124 km away, so the join indexes them.
        for k in 0..18 {
            stops.push(point(1.0 + f64::from(k) * 0.1, 0.5 + f64::from(k) * 0.02), vec![Value::Text(format!("far {k}"))]);
        }
        let mut homes = Dataset::new("homes", GeometryType::Point, vec![]);
        homes.push(point(0.0, 0.0), vec![]);
        let m = Map(HashMap::from([("stops".to_string(), stops), ("homes".to_string(), homes)]));
        let within = layer(
            crate::run(
                "spatialJoin",
                &m,
                &serde_json::json!({"target": "homes", "join": "stops", "match": "Within a distance", "distance": 10_000, "units": "Meters", "operation": "Join one to many", "distanceField": "d"}),
            )
            .unwrap(),
        );
        let names: Vec<_> = (0..within.features.len()).filter_map(|r| text_at(&within, r, "join_stop")).collect();
        assert_eq!(names, ["edge"]);
        let d = within.value(0, within.field_index("d").unwrap()).as_f64().unwrap();
        assert!((d - 9_999.994_808).abs() < 1e-5, "{d}");
    }

    /// Parcels with a zone, a district, a value (null in one) and an owner name, for Summary Statistics.
    fn parcels() -> Map {
        let fields = vec![
            Field::new("zone", FieldType::Text),
            Field::new("district", FieldType::Integer),
            Field::new("value", FieldType::Double),
            Field::new("owner", FieldType::Text),
        ];
        let mut ds = Dataset::new("parcels", GeometryType::None, fields);
        for (zone, district, value, owner) in [
            (Value::Text("R".into()), 2, Value::Float(100.0), Value::Text("Lee".into())),
            (Value::Text("C".into()), 1, Value::Float(400.0), Value::Text("Ng".into())),
            (Value::Text("R".into()), 1, Value::Float(300.0), Value::Text("Ali".into())),
            (Value::Text("R".into()), 2, Value::Float(200.0), Value::Null),
            (Value::Text("R".into()), 1, Value::Float(500.0), Value::Text("Ali".into())),
            (Value::Null, 3, Value::Null, Value::Text("Bo".into())),
        ] {
            ds.push(None, vec![zone, Value::Int(district), value, owner]);
        }
        Map(HashMap::from([("parcels".to_string(), ds)]))
    }

    /// The column of an output field, by name.
    fn column(ds: &Dataset, name: &str) -> Vec<Value> {
        let i = ds.field_index(name).unwrap_or_else(|| panic!("no field {name}"));
        (0..ds.features.len()).map(|r| ds.value(r, i).clone()).collect()
    }

    /// A field alone, as calls made before the statistics list did, still gives its six statistics
    /// under the same names, now after FREQUENCY; a group with no values reports null rather than 0.
    #[test]
    fn summary_statistics_keeps_the_field_only_call() {
        let m = parcels();
        let out =
            layer(crate::run("summaryStatistics", &m, &serde_json::json!({"input": "parcels", "field": "value", "case": "zone"})).unwrap());
        let names: Vec<&str> = out.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["zone", "FREQUENCY", "COUNT_value", "SUM_value", "MEAN_value", "MIN_value", "MAX_value", "STD_value"]);
        assert_eq!(column(&out, "zone"), [Value::Null, Value::Text("C".into()), Value::Text("R".into())], "null keys group first");
        assert_eq!(column(&out, "FREQUENCY"), [Value::Int(1), Value::Int(1), Value::Int(4)]);
        assert_eq!(column(&out, "COUNT_value"), [Value::Int(0), Value::Int(1), Value::Int(4)]);
        assert_eq!(column(&out, "MEAN_value"), [Value::Null, Value::Float(400.0), Value::Float(275.0)]);
        assert_eq!(column(&out, "SUM_value"), [Value::Null, Value::Float(400.0), Value::Float(1100.0)]);
        assert_eq!(column(&out, "STD_value").first(), Some(&Value::Null));
        let types: Vec<FieldType> = out.fields.iter().skip(1).map(|f| f.ty).collect();
        assert_eq!(
            types,
            [
                FieldType::Integer,
                FieldType::Integer,
                FieldType::Double,
                FieldType::Double,
                FieldType::Double,
                FieldType::Double,
                FieldType::Double
            ]
        );
        // No case field: one row for the table.
        let all = layer(crate::run("summaryStatistics", &m, &serde_json::json!({"input": "parcels", "field": "value"})).unwrap());
        assert_eq!(
            (all.features.len(), column(&all, "FREQUENCY"), column(&all, "MAX_value")),
            (1, vec![Value::Int(6)], vec![Value::Float(500.0)])
        );
        // A text field still reads as numbers, as it did: names aren't numbers, so none count.
        let text = layer(crate::run("summaryStatistics", &m, &serde_json::json!({"input": "parcels", "field": "owner"})).unwrap());
        assert_eq!((column(&text, "COUNT_owner"), column(&text, "MEAN_owner")), (vec![Value::Int(0)], vec![Value::Null]));
        assert_eq!(text.fields.get(3).map(|f| f.ty), Some(FieldType::Double));
    }

    /// A statistics list over several case fields: groups for each (zone, district), the pairs'
    /// fields in the order given, text statistics on a text field, and the separator.
    #[test]
    fn summary_statistics_lists_and_several_case_fields() {
        let m = parcels();
        let stats = serde_json::json!([
            {"field": "value", "statistic": "median"},
            {"field": "value", "statistic": "P90"},
            {"field": "owner", "statistic": "MODE"},
            {"field": "owner", "statistic": "CONCATENATE"},
            {"field": "owner", "statistic": "NULLS"},
        ]);
        let p = serde_json::json!({"input": "parcels", "statistics": stats, "case": ["Zone", "district"], "separator": "/"});
        let out = layer(crate::run("summaryStatistics", &m, &p).unwrap());
        let names: Vec<&str> = out.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["zone", "district", "FREQUENCY", "MEDIAN_value", "P90_value", "MODE_owner", "CONCATENATE_owner", "NULLS_owner"]);
        assert_eq!(out.fields.get(5).map(|f| f.ty), Some(FieldType::Text));
        let r1 = [Value::Text("R".into()), Value::Int(1), Value::Int(2), Value::Float(400.0), Value::Float(480.0)];
        let row = |r: usize| (0..out.fields.len()).map(|i| out.value(r, i).clone()).collect::<Vec<_>>();
        assert_eq!(out.features.len(), 4);
        assert_eq!(row(2)[..5], r1[..]);
        assert_eq!(row(2)[5..], [Value::Text("Ali".into()), Value::Text("Ali/Ali".into()), Value::Int(0)]);
        assert_eq!(row(3)[5..], [Value::Text("Lee".into()), Value::Text("Lee".into()), Value::Int(1)]);
        // The statistics list as text works the same.
        let text = serde_json::json!({"input": "parcels", "statistics": stats.to_string(), "case": ["zone", "district"], "separator": "/"});
        assert_eq!(layer(crate::run("summaryStatistics", &m, &text).unwrap()).features, out.features);
        // COUNT and UNIQUE of a text field leave out its nulls.
        let owners = serde_json::json!({"input": "parcels", "statistics": [{"field": "owner", "statistic": "COUNT"}, {"field": "owner", "statistic": "UNIQUE"}]});
        let owners = layer(crate::run("summaryStatistics", &m, &owners).unwrap());
        assert_eq!((column(&owners, "COUNT_owner"), column(&owners, "UNIQUE_owner")), (vec![Value::Int(5)], vec![Value::Int(4)]));
        // The statistics list takes precedence over a field.
        let both = serde_json::json!({"input": "parcels", "statistics": [{"field": "value", "statistic": "SUM"}], "field": "district"});
        let names = layer(crate::run("summaryStatistics", &m, &both).unwrap()).fields.into_iter().map(|f| f.name).collect::<Vec<_>>();
        assert_eq!(names, ["FREQUENCY", "SUM_value"]);
    }

    /// Calls that can't be answered are refused with a message naming the problem.
    #[test]
    fn summary_statistics_rejects_bad_requests() {
        let m = parcels();
        for (p, says) in [
            (serde_json::json!({"input": "parcels"}), "give the statistics"),
            (serde_json::json!({"input": "parcels", "statistics": [{"field": "owner", "statistic": "MEAN"}]}), "needs a numeric field"),
            (serde_json::json!({"input": "parcels", "statistics": [{"field": "value", "statistic": "TOTAL"}]}), "must be one of"),
            (serde_json::json!({"input": "parcels", "field": "value", "case": ["zone", "ZONE"]}), "listed twice"),
            (serde_json::json!({"input": "parcels", "field": "value", "case": "zone, district"}), "no field"),
            (serde_json::json!({"input": "parcels", "field": "value", "case": "nope"}), "no field"),
            (
                serde_json::json!({"input": "parcels", "statistics": [{"field": "owner", "statistic": "CONCATENATE"}], "separator": 1}),
                "Separator must be text",
            ),
            (
                serde_json::json!({"input": "parcels", "statistics": [{"field": "owner", "statistic": "CONCATENATE"}], "separator": "x".repeat(257)}),
                "at most 256 bytes",
            ),
        ] {
            let err = crate::run("summaryStatistics", &m, &p).err().unwrap_or_default();
            assert!(err.contains(says), "{p}: {err}");
        }
        // A bad separator is refused without CONCATENATE too, as before Pivot Table shared it.
        for separator in [serde_json::json!(1), serde_json::json!("x".repeat(257))] {
            let p = serde_json::json!({"input": "parcels", "field": "value", "separator": separator});
            assert!(crate::run("summaryStatistics", &m, &p).is_err(), "{p}");
        }
        // A case field can't share a name with an output field.
        // Nor can output fields differ only in case.
        let fields = vec![
            Field::new("count_v", FieldType::Text),
            Field::new("v", FieldType::Double),
            Field::new("Frequency", FieldType::Text),
            Field::new("V", FieldType::Double),
        ];
        let mut t = Dataset::new("t", GeometryType::None, fields);
        t.push(None, vec![Value::Text("a".into()), Value::Float(1.0), Value::Text("x".into()), Value::Float(2.0)]);
        let m = Map(HashMap::from([("t".to_string(), t)]));
        let mean = |f: &str| serde_json::json!({"field": f, "statistic": "MEAN"});
        for (p, says) in [
            (serde_json::json!({"input": "t", "field": "v", "case": "count_v"}), "two fields named 'COUNT_v'"),
            (serde_json::json!({"input": "t", "field": "v", "case": "Frequency"}), "FREQUENCY field"),
            (serde_json::json!({"input": "t", "field": "count_v", "case": ["v", "V"]}), "differ only in case"),
            (serde_json::json!({"input": "t", "statistics": [mean("v"), mean("V")]}), "two fields named 'MEAN_V'"),
        ] {
            let err = crate::run("summaryStatistics", &m, &p).err().unwrap_or_default();
            assert!(err.contains(says), "{p}: {err}");
        }
    }

    /// The Tools pane shows Statistics Field only without a statistics list, and Separator only for
    /// a list with CONCATENATE.
    #[test]
    fn summary_statistics_parameters_apply() {
        let with = |list: &'static str| move |k: &str| if k == "statistics" { list.to_string() } else { String::new() };
        assert!(param_applies("summaryStatistics", "field", &with("")));
        assert!(param_applies("summaryStatistics", "field", &with("[]")));
        assert!(param_applies("summaryStatistics", "field", &with("[{")));
        assert!(!param_applies("summaryStatistics", "field", &with("[{\"field\": \"v\", \"statistic\": \"SUM\"}]")));
        assert!(!param_applies("summaryStatistics", "separator", &with("")));
        assert!(param_applies("summaryStatistics", "separator", &with("[{\"field\": \"v\", \"statistic\": \"concatenate\"}]")));
        assert!(!param_applies("summaryStatistics", "separator", &with("[{\"field\": \"concatenate_ids\", \"statistic\": \"SUM\"}]")));
        assert!(!param_applies("summaryStatistics", "separator", &with("[{\"field\": ")));
    }

    /// Dissolve and Summarize Within show their separator only for a list with CONCATENATE, as
    /// Summary Statistics does; their other parameters always apply.
    #[test]
    fn summary_field_parameters_apply() {
        let with = |list: &'static str| move |k: &str| if k == "statistics" { list.to_string() } else { String::new() };
        for tool in ["dissolve", "countPoints"] {
            assert!(!param_applies(tool, "separator", &with("")), "{tool}");
            assert!(!param_applies(tool, "separator", &with("[{\"field\": \"v\", \"statistic\": \"SUM\"}]")), "{tool}");
            assert!(param_applies(tool, "separator", &with("[{\"field\": \"v\", \"statistic\": \" Concatenate \"}]")), "{tool}");
            assert!(!param_applies(tool, "separator", &with("[{")), "{tool}");
            for param in ["statistics", "output", "field", "polygons", "points", "input"] {
                assert!(param_applies(tool, param, &with("")), "{tool} {param}");
            }
        }
        // Each statistics list reads the fields of the layer it summarises.
        let kind = |tool: &str| crate::tool(tool).and_then(|t| t.params.iter().find(|p| p.name == "statistics")).map(|p| p.kind);
        assert_eq!(kind("dissolve"), Some(K::FieldStatistics { input: "input", statistics: crate::StatisticList::Summary }));
        assert_eq!(kind("countPoints"), Some(K::FieldStatistics { input: "points", statistics: crate::StatisticList::Summary }));
        for tool in ["dissolve", "countPoints", "summaryStatistics"] {
            let separator = crate::tool(tool).and_then(|t| t.params.iter().find(|p| p.name == "separator"));
            assert_eq!(separator.map(|p| (p.kind, p.default)), Some((K::Text, "\", \"")), "{tool}");
        }
    }

    /// Summary field names clash with the tool's own fields and with each other in any case; the
    /// first clash is named.
    #[test]
    fn statistic_fields_refuse_case_insensitive_clashes() {
        use crate::stats::{Request, Stat};
        let ds = Dataset::new(
            "t",
            GeometryType::Point,
            vec![Field::new("pop", FieldType::Integer), Field::new("POP", FieldType::Double), Field::new("name", FieldType::Text)],
        );
        let req = |field, stat| Request { field, stat };
        let fields = statistic_fields(&ds, &[req(0, Stat::Sum), req(2, Stat::First), req(2, Stat::Count)], false, &[]).unwrap();
        let shape: Vec<(&str, FieldType)> = fields.iter().map(|f| (f.name.as_str(), f.ty)).collect();
        assert_eq!(shape, [("SUM_pop", FieldType::Double), ("FIRST_name", FieldType::Text), ("COUNT_name", FieldType::Integer)]);
        let err = statistic_fields(&ds, &[req(0, Stat::Sum), req(1, Stat::Sum)], false, &[]).unwrap_err();
        assert_eq!(err, "the output would have two fields named 'SUM_POP'");
        let taken = [Field::new("count", FieldType::Integer), Field::new("sum_Pop", FieldType::Text)];
        let err = statistic_fields(&ds, &[req(2, Stat::Max), req(0, Stat::Sum)], false, &taken).unwrap_err();
        assert_eq!(err, "the output would have two fields named 'SUM_pop'");
        assert!(statistic_fields(&ds, &[], false, &taken).unwrap().is_empty());
        // Read as numbers, a text field's statistics are numbers.
        let numbers = statistic_fields(&ds, &[req(2, Stat::Min)], true, &[]).unwrap();
        assert_eq!(numbers.first().map(|f| f.ty), Some(FieldType::Double));
    }

    /// A separator is read only with a statistics list, so a run without one is unchanged by a
    /// bad separator; with a list, a bad one is refused as Summary Statistics refuses it.
    #[test]
    fn summary_requests_read_the_separator_only_with_a_list() {
        let ds = Dataset::new("t", GeometryType::Point, vec![Field::new("v", FieldType::Double)]);
        let long = "x".repeat(MAX_SEPARATOR + 1);
        for p in [serde_json::json!({"separator": 5}), serde_json::json!({"separator": long, "statistics": ""})] {
            let (requests, separator) = summary_requests(&ds, &p).unwrap();
            assert!(requests.is_empty());
            assert_eq!(separator, ", ");
        }
        let list = serde_json::json!([{"field": "v", "statistic": "SUM"}]);
        assert!(summary_requests(&ds, &serde_json::json!({"separator": 5, "statistics": list})).is_err());
        assert!(summary_requests(&ds, &serde_json::json!({"separator": long, "statistics": list})).is_err());
        let p = serde_json::json!({"separator": " | ", "statistics": list});
        assert_eq!(summary_requests(&ds, &p).map(|(r, s)| (r.len(), s.to_string())), Ok((1, " | ".to_string())));
        let mut rows = vec![5, 2, 9, 2, 0];
        record_order(&mut rows);
        assert_eq!(rows, [0, 2, 5, 9]);
    }

    /// A selection's rows are taken once each, at their first place, whether they are listed out
    /// of order or sorted with a record repeated; rows in increasing order are already unique.
    #[test]
    fn each_once_keeps_the_first_of_a_repeated_row() {
        assert_eq!(each_once(vec![3, 1, 3, 0, 1]), [3, 1, 0]);
        assert_eq!(each_once(vec![0, 0, 2, 7, 7]), [0, 2, 7]);
        assert_eq!(each_once(vec![0, 1, 2]), [0, 1, 2]);
        assert_eq!(each_once(vec![4]), [4]);
        assert!(each_once(Vec::new()).is_empty());
    }

    /// Summarize Within writes its statistics in the fields it adds even for a polygon whose row
    /// holds fewer values than the layer has fields (a hand-built or hostile layer).
    #[test]
    fn summarize_within_statistics_fill_short_rows() {
        let mut polys =
            Dataset::new("polys", GeometryType::Polygon, vec![Field::new("a", FieldType::Text), Field::new("b", FieldType::Text)]);
        polys.features.push(Feature { id: 0, geometry: Some(sq(0.0, 0.0, 2.0)), attrs: vec![] });
        let mut pts = Dataset::new("pts", GeometryType::Point, vec![Field::new("v", FieldType::Integer)]);
        pts.push(Some(Geometry::Point(Coord::new(1.0, 1.0))), vec![Value::Int(4)]);
        pts.push(Some(Geometry::Point(Coord::new(1.5, 1.0))), vec![Value::Int(6)]);
        let m = Map(HashMap::from([("polys".to_string(), polys), ("pts".to_string(), pts)]));
        let p = serde_json::json!({"polygons": "polys", "points": "pts", "statistics": [{"field": "v", "statistic": "SUM"}]});
        let out = layer(crate::run("countPoints", &m, &p).unwrap());
        let names: Vec<&str> = out.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "point_count", "SUM_v"]);
        assert_eq!(out.value(0, 3), &Value::Float(10.0));
    }

    /// Incomes by county and year, one record for each, in long form; `rows` adds records.
    fn long_incomes(rows: &[(&str, i64, f64)]) -> Map {
        let fields =
            vec![Field::new("county", FieldType::Text), Field::new("year", FieldType::Integer), Field::new("income", FieldType::Double)];
        let mut ds = Dataset::new("long", GeometryType::None, fields);
        for &(county, year, income) in [("B", 2022, 22.0), ("A", 2021, 11.0), ("B", 2020, 20.0), ("A", 2020, 10.0)].iter().chain(rows) {
            ds.push(None, vec![Value::Text(county.into()), Value::Int(year), Value::Float(income)]);
        }
        Map(HashMap::from([("long".to_string(), ds)]))
    }

    /// Without a statistic, each value moves to the row of its row fields and the field of its
    /// pivot value, with the value field's type; a combination without a record is null.
    #[test]
    fn pivot_table_reshapes_a_long_table() {
        let m = long_incomes(&[]);
        let p = serde_json::json!({"input": "long", "fields": "county", "pivot": "year", "value": "income"});
        let out = layer(crate::run("pivotTable", &m, &p).unwrap());
        assert_eq!(out.name, "long_Pivot");
        assert_eq!(out.geometry_type, GeometryType::None);
        let names: Vec<&str> = out.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["county", "year2020", "year2021", "year2022"]);
        assert!(out.fields.iter().skip(1).all(|f| f.ty == FieldType::Double));
        let (a, b) = (Value::Text("A".into()), Value::Text("B".into()));
        assert_eq!(column(&out, "county"), [a, b]);
        assert_eq!(column(&out, "year2020"), [Value::Float(10.0), Value::Float(20.0)]);
        assert_eq!(column(&out, "year2021"), [Value::Float(11.0), Value::Null]);
        assert_eq!(column(&out, "year2022"), [Value::Null, Value::Float(22.0)]);
        // Without row fields there is one row, and `output` names the table.
        let p = serde_json::json!({"input": "long", "pivot": "county", "value": "income", "statistic": "sum", "output": "totals"});
        let out = layer(crate::run("pivotTable", &m, &p).unwrap());
        assert_eq!(out.name, "totals");
        assert_eq!(out.features.len(), 1);
        assert_eq!((column(&out, "A"), column(&out, "B")), (vec![Value::Float(21.0)], vec![Value::Float(42.0)]));
        // An empty table has no rows, even without row fields.
        let empty = Map(HashMap::from([("long".to_string(), Dataset::new("long", GeometryType::None, out.fields.clone()))]));
        let p = serde_json::json!({"input": "long", "pivot": "A", "value": "B"});
        assert_eq!(layer(crate::run("pivotTable", &empty, &p).unwrap()).features.len(), 0);
    }

    /// Two records for one cell are refused without a statistic, naming the cell; a statistic
    /// combines them in record order.
    #[test]
    fn pivot_table_combines_a_cell_only_with_a_statistic() {
        let m = long_incomes(&[("A", 2020, 30.0)]);
        let p = serde_json::json!({"input": "long", "fields": ["county"], "pivot": "year", "value": "income"});
        let err = crate::run("pivotTable", &m, &p).err().unwrap_or_default();
        assert!(err.contains("2 records have county = A, year = 2020"), "{err}");
        for (stat, want) in [("MEAN", Value::Float(20.0)), ("COUNT", Value::Int(2)), ("LAST", Value::Float(30.0))] {
            let p = serde_json::json!({"input": "long", "fields": "county", "pivot": "year", "value": "income", "statistic": stat});
            let out = layer(crate::run("pivotTable", &m, &p).unwrap());
            assert_eq!(column(&out, "year2020").first(), Some(&want), "{stat}");
            // A cell without records is null, even for COUNT.
            assert_eq!(column(&out, "year2022").first(), Some(&Value::Null), "{stat}");
        }
    }

    /// Aggregating parcels: groups and pivot columns are sorted with nulls first; a cell whose
    /// records have no value gets the statistic of no values (null, or a COUNT of 0); CONCATENATE
    /// uses the separator; text values take the statistics that fit text.
    #[test]
    fn pivot_table_aggregates_parcels() {
        let m = parcels();
        let run = |stat: &str, value: &str| {
            let p = serde_json::json!({"input": "parcels", "fields": "zone", "pivot": "district", "value": value, "statistic": stat, "separator": "/"});
            layer(crate::run("pivotTable", &m, &p).unwrap())
        };
        let sum = run("SUM", "value");
        // A bad separator is refused whatever the statistic, as in Summary Statistics.
        let junk = serde_json::json!({"input": "parcels", "fields": "zone", "pivot": "district", "value": "value", "statistic": "SUM", "separator": 1});
        assert!(crate::run("pivotTable", &m, &junk).is_err_and(|e| e.contains("Separator must be text")));
        assert_eq!(column(&sum, "zone"), [Value::Null, Value::Text("C".into()), Value::Text("R".into())]);
        assert_eq!(column(&sum, "district1"), [Value::Null, Value::Float(400.0), Value::Float(800.0)]);
        assert_eq!(column(&sum, "district2"), [Value::Null, Value::Null, Value::Float(300.0)]);
        assert_eq!(column(&sum, "district3"), [Value::Null, Value::Null, Value::Null]);
        let count = run("COUNT", "value");
        assert_eq!(count.fields.get(1).map(|f| f.ty), Some(FieldType::Integer));
        assert_eq!(column(&count, "district3"), [Value::Int(0), Value::Null, Value::Null]);
        let owners = run("CONCATENATE", "owner");
        assert_eq!(owners.fields.get(1).map(|f| f.ty), Some(FieldType::Text));
        assert_eq!(column(&owners, "district1").get(2), Some(&Value::Text("Ali/Ali".into())));
        assert_eq!(column(&owners, "district2").get(2), Some(&Value::Text("Lee".into())));
        let err = crate::run(
            "pivotTable",
            &m,
            &serde_json::json!({"input": "parcels", "pivot": "district", "value": "owner", "statistic": "MEAN"}),
        );
        assert!(err.is_err_and(|e| e.contains("needs a numeric value field")));
    }

    /// Field names made from pivot values: numbers follow the pivot field's name, text keeps its
    /// letters and digits, and names alike in all but case, or taken by a row field, get a suffix
    /// in the order of the pivot values.
    #[test]
    fn pivot_table_names_fields_from_pivot_values() {
        let fields = vec![Field::new("id", FieldType::Integer), Field::new("kind", FieldType::Text), Field::new("rate", FieldType::Double)];
        let mut ds = Dataset::new("t", GeometryType::None, fields);
        let kinds = ["north east", "North-east", "2nd", "!!", "_x_", "id", "é", &"y".repeat(100)];
        for (i, kind) in kinds.iter().enumerate() {
            ds.push(None, vec![Value::Int(1), Value::Text((*kind).into()), Value::Float(i as f64)]);
        }
        let m = Map(HashMap::from([("t".to_string(), ds)]));
        let p = serde_json::json!({"input": "t", "fields": "id", "pivot": "kind", "value": "rate"});
        let out = layer(crate::run("pivotTable", &m, &p).unwrap());
        let names: Vec<String> = out.fields.iter().map(|f| f.name.clone()).collect();
        let long = "y".repeat(MAX_PIVOT_NAME);
        assert_eq!(names, ["id", "kind", "kind2nd", "x", "id_2", "north_east", "North_east_2", &long, "é"]);
        // The value of "north east" lands under its name.
        assert_eq!(column(&out, "north_east"), [Value::Float(0.0)]);

        let fields = vec![Field::new("k", FieldType::Text), Field::new("rate", FieldType::Double), Field::new("on", FieldType::Boolean)];
        let mut ds = Dataset::new("n", GeometryType::None, fields);
        for (rate, on) in [(2.5, true), (-3.0, false), (1.0, true)] {
            ds.push(None, vec![Value::Text("a".into()), Value::Float(rate), Value::Bool(on)]);
        }
        let m = Map(HashMap::from([("n".to_string(), ds)]));
        let names = |pivot: &str, value: &str| {
            let p = serde_json::json!({"input": "n", "pivot": pivot, "value": value, "statistic": "COUNT"});
            layer(crate::run("pivotTable", &m, &p).unwrap()).fields.into_iter().map(|f| f.name).collect::<Vec<_>>()
        };
        assert_eq!(names("rate", "k"), ["rate_3", "rate1", "rate2_5"]);
        assert_eq!(names("on", "k"), ["onFalse", "onTrue"]);
        // A long field name keeps every value in its fields' names; a digit of any script starts
        // a number, so the field's name comes first.
        let long = "f".repeat(100);
        assert_eq!(pivot_column_name(&long, &Value::Int(7)), format!("{long}7"));
        assert_eq!(pivot_column_name("k", &Value::Text("٣rd".into())), "k٣rd");
        // Decimals are named in full, so values closer than a display's rounding keep their own.
        assert_eq!(pivot_column_name("rate", &Value::Float(0.000_000_1)), "rate0_0000001");
        assert_eq!(pivot_column_name("rate", &Value::Float(1.000_000_1)), "rate1_0000001");
        assert_eq!(pivot_column_name("rate", &Value::Float(1.0)), "rate1");
        // A run of other characters right after an underscore adds none.
        assert_eq!(pivot_column_name("k", &Value::Text("North_-east".into())), "North_east");
        // A cut at a run of other characters leaves no underscore at the end.
        let cut = format!("{}-b", "a".repeat(MAX_PIVOT_NAME - 1));
        assert_eq!(pivot_column_name("k", &Value::Text(cut)), "a".repeat(MAX_PIVOT_NAME - 1));
    }

    /// Calls that can't be answered are refused with a message naming the problem, including a
    /// pivot field with too many values and an output with too many cells.
    #[test]
    fn pivot_table_rejects_bad_requests() {
        let m = parcels();
        for (p, says) in [
            (serde_json::json!({"input": "parcels", "value": "value"}), "'Pivot Field' is required"),
            (serde_json::json!({"input": "parcels", "pivot": "district"}), "'Value Field' is required"),
            (serde_json::json!({"input": "parcels", "pivot": " ", "value": "value"}), "'Pivot Field' is required"),
            (serde_json::json!({"input": "parcels", "pivot": 2020, "value": "value"}), "'Pivot Field' must be a field name"),
            (serde_json::json!({"input": "parcels", "pivot": "district", "value": "value", "statistic": 7}), "Statistic must be text"),
            (serde_json::json!({"input": "parcels", "pivot": "district", "value": ["value"]}), "'Value Field' must be a field name"),
            (
                serde_json::json!({"input": "parcels", "fields": ["district"], "pivot": "DISTRICT", "value": "value"}),
                "can't also be a row field",
            ),
            (serde_json::json!({"input": "parcels", "pivot": "zone", "value": "value", "statistic": "SUM"}), "has no value in 1 record:"),
            (serde_json::json!({"input": "parcels", "pivot": "district", "value": "value", "statistic": "TOTAL"}), "must be one of"),
            (serde_json::json!({"input": "parcels", "pivot": "nope", "value": "value"}), "no field"),
            (serde_json::json!({"input": "parcels", "fields": ["zone", "Zone"], "pivot": "district", "value": "value"}), "listed twice"),
            (serde_json::json!({"input": "parcels", "fields": 3, "pivot": "district", "value": "value"}), "Row Field must be"),
            (
                serde_json::json!({"input": "parcels", "pivot": "district", "value": "owner", "statistic": "CONCATENATE", "separator": 1}),
                "Separator must be text",
            ),
        ] {
            let err = crate::run("pivotTable", &m, &p).err().unwrap_or_default();
            assert!(err.contains(says), "{p}: {err}");
        }
        let fields = vec![Field::new("k", FieldType::Integer), Field::new("p", FieldType::Integer)];
        let mut ds = Dataset::new("big", GeometryType::None, fields);
        for i in 0..=MAX_PIVOT_CELLS / MAX_PIVOT_COLUMNS {
            ds.push(None, vec![Value::Int(i as i64), Value::Int((i % MAX_PIVOT_COLUMNS) as i64)]);
        }
        let m = Map(HashMap::from([("big".to_string(), ds)]));
        let p = serde_json::json!({"input": "big", "pivot": "k", "value": "p", "statistic": "COUNT"});
        let err = crate::run("pivotTable", &m, &p).err().unwrap_or_default();
        assert!(err.contains("more than 1000 fields"), "{err}");
        let p = serde_json::json!({"input": "big", "fields": "k", "pivot": "p", "value": "k", "statistic": "COUNT"});
        let err = crate::run("pivotTable", &m, &p).err().unwrap_or_default();
        assert!(err.contains("10001 rows by 1001 fields would make more than 10000000 values"), "{err}");
    }

    /// The Tools pane shows Pivot Table's separator only for CONCATENATE.
    #[test]
    fn pivot_table_separator_applies_to_concatenate() {
        let with = |stat: &'static str| move |k: &str| if k == "statistic" { stat.to_string() } else { String::new() };
        assert!(param_applies("pivotTable", "separator", &with(" concatenate ")));
        assert!(!param_applies("pivotTable", "separator", &with("")));
        assert!(!param_applies("pivotTable", "separator", &with("COUNT")));
    }

    #[test]
    fn spatial_join_merge_rules() {
        let mut districts = Dataset::new("districts", GeometryType::Polygon, vec![Field::new("d", FieldType::Text)]);
        districts.push(Some(sq(0.0, 0.0, 1.0)), vec![Value::Text("north".into())]);
        districts.push(Some(sq(5.0, 5.0, 1.0)), vec![Value::Text("empty".into())]);
        let fields =
            vec![Field::new("units", FieldType::Integer), Field::new("value", FieldType::Double), Field::new("name", FieldType::Text)];
        let mut permits = Dataset::new("permits", GeometryType::Point, fields);
        for (x, units, value, name) in [
            (0.2, Value::Int(2), Value::Float(1.5), Value::Text("b".into())),
            (0.4, Value::Null, Value::Float(2.5), Value::Null),
            (0.6, Value::Int(5), Value::Null, Value::Text("a".into())),
        ] {
            permits.push(Some(Geometry::Point(Coord::new(x, 0.5))), vec![units, value, name]);
        }
        let m = Map(HashMap::from([("districts".to_string(), districts), ("permits".to_string(), permits)]));
        let run = |merge: serde_json::Value| {
            crate::run("spatialJoin", &m, &serde_json::json!({"target": "districts", "join": "permits", "merge": merge}))
        };
        let ok = |merge: serde_json::Value| layer(run(merge).unwrap_or_else(|e| panic!("{e}")));
        let at = |ds: &Dataset, f: &str| ds.value(0, ds.field_index(f).unwrap()).clone();
        let ty = |ds: &Dataset, f: &str| ds.fields[ds.field_index(f).unwrap()].ty;

        let a = ok(serde_json::json!([
            {"field": "units", "rule": "sum"},
            {"field": "value", "rule": "mean"},
            {"field": "name", "rule": "join", "delimiter": "; "}
        ]));
        assert_eq!(at(&a, "join_units"), Value::Int(7));
        assert_eq!(at(&a, "join_value"), Value::Float(2.0));
        assert_eq!(at(&a, "join_name"), Value::Text("b; a".into()));
        assert_eq!(
            (ty(&a, "join_units"), ty(&a, "join_value"), ty(&a, "join_name")),
            (FieldType::Integer, FieldType::Double, FieldType::Text)
        );
        // An unmatched target has nulls, as without rules, but no matches count 0.
        assert_eq!(a.value(1, a.field_index("join_units").unwrap()), &Value::Null);
        let counted = ok(serde_json::json!([{"field": "name", "rule": "count"}, {"field": "units", "rule": "sum"}]));
        assert_eq!(counted.value(1, counted.field_index("join_name").unwrap()), &Value::Int(0));
        assert_eq!(counted.value(1, counted.field_index("join_units").unwrap()), &Value::Null);

        // Given as text, as the Tools pane sends it; field and rule names in any case.
        let b = ok(serde_json::json!(
            r#"[{"field": "UNITS", "rule": "Max"}, {"field": "value", "rule": "count"}, {"field": "name", "rule": "min"}]"#
        ));
        assert_eq!(at(&b, "join_units"), Value::Int(5));
        assert_eq!((at(&b, "join_value"), ty(&b, "join_value")), (Value::Int(2), FieldType::Integer));
        assert_eq!(at(&b, "join_name"), Value::Text("a".into()));
        let c = ok(
            serde_json::json!([{"field": "units", "rule": "min"}, {"field": "value", "rule": "last"}, {"field": "name", "rule": "max"}]),
        );
        assert_eq!(
            (at(&c, "join_units"), at(&c, "join_value"), at(&c, "join_name")),
            (Value::Int(2), Value::Null, Value::Text("b".into()))
        );
        // Fields without a rule keep the first match.
        let d = ok(serde_json::json!([{"field": "units", "rule": "sum"}]));
        assert_eq!((at(&d, "join_value"), at(&d, "join_name")), (Value::Float(1.5), Value::Text("b".into())));
        assert_eq!(ok(serde_json::json!("")).features.len(), 2);

        for bad in [
            serde_json::json!([{"field": "nope", "rule": "sum"}]),
            serde_json::json!([{"field": "units", "rule": "median"}]),
            serde_json::json!([{"field": "name", "rule": "sum"}]),
            serde_json::json!([{"field": "units", "rule": "sum"}, {"field": "Units", "rule": "max"}]),
            serde_json::json!([{"field": "name", "rule": "join", "delimiter": 3}]),
            serde_json::json!([{"rule": "sum"}]),
            serde_json::json!({"field": "units"}),
            serde_json::json!("[{"),
        ] {
            assert!(run(bad.clone()).is_err(), "{bad}");
        }
        // One to many keeps each match, so it takes no rules.
        let many = serde_json::json!({"target": "districts", "join": "permits", "operation": "Join one to many", "merge": [{"field": "units", "rule": "sum"}]});
        assert!(crate::run("spatialJoin", &m, &many).is_err());

        // An integer field's sum takes a whole number held as a decimal, and skips one that isn't
        // whole.
        let mut mixed = m.0["permits"].clone();
        mixed.features[1].attrs[0] = Value::Float(4.0);
        mixed.features[2].attrs[0] = Value::Float(2.5);
        let m_mixed = Map(HashMap::from([("districts".to_string(), m.0["districts"].clone()), ("permits".to_string(), mixed)]));
        let summed = layer(
            crate::run(
                "spatialJoin",
                &m_mixed,
                &serde_json::json!({"target": "districts", "join": "permits", "merge": [{"field": "units", "rule": "sum"}]}),
            )
            .unwrap(),
        );
        assert_eq!(at(&summed, "join_units"), Value::Int(6));

        // An integer sum past 64 bits is null, not wrapped.
        let mut big = m.0["permits"].clone();
        for f in &mut big.features {
            f.attrs[0] = Value::Int(i64::MAX);
        }
        let m2 = Map(HashMap::from([("districts".to_string(), m.0["districts"].clone()), ("permits".to_string(), big)]));
        let r = layer(
            crate::run(
                "spatialJoin",
                &m2,
                &serde_json::json!({"target": "districts", "join": "permits", "merge": [{"field": "units", "rule": "sum"}]}),
            )
            .unwrap(),
        );
        assert_eq!(at(&r, "join_units"), Value::Null);
    }

    #[test]
    fn spatial_join_distance_options_show_only_where_they_apply() {
        let with = |m: &'static str, op: &'static str| {
            move |n: &str| match n {
                "match" => m.to_string(),
                "operation" => op.to_string(),
                _ => String::new(),
            }
        };
        for p in ["distance", "units", "distanceField"] {
            assert!(crate::param_applies("spatialJoin", p, &with("Closest", "")));
            assert!(crate::param_applies("spatialJoin", p, &with("Within a distance", "")));
            assert!(!crate::param_applies("spatialJoin", p, &with("", "")));
            assert!(!crate::param_applies("spatialJoin", p, &with("Largest overlap", "")));
        }
        assert!(crate::param_applies("spatialJoin", "merge", &with("", "")));
        assert!(!crate::param_applies("spatialJoin", "merge", &with("", "Join one to many")));
        assert!(crate::param_applies("buffer", "units", &with("", "")));
    }

    /// Zones worked out on several threads give the same statistics, bit for bit, as on one, in
    /// input order (#1252): zones without geometry, past the raster's edge and overlapping ones
    /// included.
    #[test]
    fn zonal_stats_on_several_threads_is_zonal_stats_on_one() {
        let (w, h) = (200u32, 150u32);
        let band = |k: f32| (0..w * h).map(|i| ((i % w) as f32 * 0.37 + (i / w) as f32 * 1.13 + k).sin() * 1e3 + 0.1).collect::<Vec<f32>>();
        let gt = rustgis_data::raster::GeoTransform { origin_x: 0.0, origin_y: 15.0, pixel_width: 0.1, pixel_height: -0.1 };
        let raster = rustgis_data::raster::Raster::new(
            "elev",
            w,
            h,
            vec![band(0.0), band(2.0)],
            gt,
            rustgis_data::raster::Crs::LonLat,
            None,
            rustgis_data::raster::ColorHint::Gray,
            "Float32",
        )
        .unwrap();
        let mut zones = Dataset::new("zones", GeometryType::Polygon, vec![Field::new("id", FieldType::Integer)]);
        for i in 0..600i64 {
            let (x, y, d) = ((i * 37 % 230) as f64 / 10.0 - 1.0, (i * 53 % 170) as f64 / 10.0 - 1.0, 0.3 + (i % 7) as f64 * 0.4);
            zones.push(if i % 61 == 0 { None } else { Some(sq(x, y, d)) }, vec![Value::Int(i)]);
        }
        let m =
            RasterMap { datasets: HashMap::from([("zones".to_string(), zones)]), rasters: HashMap::from([("elev".to_string(), &raster)]) };
        let p = serde_json::json!({"raster": "elev", "polygons": "zones", "bands": [1, 2], "stats": ["count", "sum", "mean", "min", "max", "stddev"]});
        let one = zonal_stats_on(&m, &p, Some(1)).unwrap();
        let attrs = |r: &ToolResult| match &r.output {
            Output::Layer(l) => l.features.iter().map(|f| format!("{:?}", f.attrs)).collect::<Vec<_>>(),
            _ => panic!("a layer"),
        };
        assert!(attrs(&one).iter().any(|a| a.contains("Float")), "some zones meet the raster");
        for threads in [2, 7, 16] {
            let many = zonal_stats_on(&m, &p, Some(threads)).unwrap();
            assert_eq!(attrs(&many), attrs(&one), "{threads} threads");
            assert_eq!(many.messages, one.messages, "{threads} threads");
        }
    }

    #[test]
    fn zonal_stats_tool_cases() {
        let (w, h) = (10u32, 10u32);
        let mut b1_data: Vec<f32> = Vec::with_capacity((w * h) as usize);
        let mut b2_data: Vec<f32> = Vec::with_capacity((w * h) as usize);
        for row in 0..h {
            for col in 0..w {
                b1_data.push((col * 10 + row) as f32);
                let v2 = 100.0 - (col * 10 + row) as f32;
                if col == 1 && row == 1 {
                    b2_data.push(-9999.0);
                } else {
                    b2_data.push(v2);
                }
            }
        }
        let gt = rustgis_data::raster::GeoTransform { origin_x: 0.0, origin_y: 10.0, pixel_width: 1.0, pixel_height: -1.0 };
        let raster = rustgis_data::raster::Raster::new(
            "elev",
            w,
            h,
            vec![b1_data, b2_data],
            gt,
            rustgis_data::raster::Crs::LonLat,
            Some(-9999.0),
            rustgis_data::raster::ColorHint::Gray,
            "Float32",
        )
        .unwrap();

        let mut poly_ds = Dataset::new(
            "zones",
            GeometryType::Polygon,
            vec![Field::new("zone_id", FieldType::Integer), Field::new("elev_mean", FieldType::Double)],
        );

        let sq_geom = |x0: f64, y0: f64, x1: f64, y1: f64| {
            Geometry::Polygon(vec![vec![vec![
                Coord::new(x0, y0),
                Coord::new(x1, y0),
                Coord::new(x1, y1),
                Coord::new(x0, y1),
                Coord::new(x0, y0),
            ]]])
        };

        poly_ds.push(Some(sq_geom(1.0, 7.0, 3.0, 9.0)), vec![Value::Int(101), Value::Float(999.0)]);
        poly_ds.push(Some(sq_geom(20.0, 20.0, 25.0, 25.0)), vec![Value::Int(102), Value::Float(888.0)]);

        let hole_geom = Geometry::Polygon(vec![vec![
            vec![Coord::new(0.0, 7.0), Coord::new(3.0, 7.0), Coord::new(3.0, 10.0), Coord::new(0.0, 10.0), Coord::new(0.0, 7.0)],
            vec![Coord::new(1.0, 8.0), Coord::new(2.0, 8.0), Coord::new(2.0, 9.0), Coord::new(1.0, 9.0), Coord::new(1.0, 8.0)],
        ]]);
        poly_ds.push(Some(hole_geom), vec![Value::Int(103), Value::Float(777.0)]);
        poly_ds.push(None, vec![Value::Int(104), Value::Float(666.0)]);

        let input_clone = poly_ds.clone();
        let m = RasterMap {
            datasets: HashMap::from([("zones".to_string(), poly_ds)]),
            rasters: HashMap::from([("elev".to_string(), &raster)]),
        };

        let res = crate::run(
            "zonalStats",
            &m,
            &serde_json::json!({
                "raster": "elev",
                "polygons": "zones",
                "bands": [1],
            }),
        )
        .unwrap();

        let out = layer(res);
        assert_eq!(out.features.len(), 4);

        assert_eq!(out.fields[0].name, "zone_id");
        assert_eq!(out.fields[1].name, "elev_mean");
        assert_eq!(out.value(0, 0), &Value::Int(101));
        assert_eq!(out.value(0, 1), &Value::Float(999.0));

        let mean_idx = out.field_index("elev_mean_2").expect("collision should produce elev_mean_2");
        let min_idx = out.field_index("elev_min").unwrap();
        let max_idx = out.field_index("elev_max").unwrap();
        let sum_idx = out.field_index("elev_sum").unwrap();
        let count_idx = out.field_index("elev_count").unwrap();
        let stddev_idx = out.field_index("elev_stddev").unwrap();

        assert_eq!(out.value(0, count_idx), &Value::Int(4));
        assert_eq!(out.value(0, sum_idx), &Value::Float(66.0));
        assert_eq!(out.value(0, min_idx), &Value::Float(11.0));
        assert_eq!(out.value(0, max_idx), &Value::Float(22.0));
        assert_eq!(out.value(0, mean_idx), &Value::Float(16.5));
        let std_val = out.value(0, stddev_idx).as_f64().unwrap();
        assert!((std_val - 25.25f64.sqrt()).abs() < 1e-10);

        assert_eq!(out.value(1, count_idx), &Value::Int(0));
        assert_eq!(out.value(1, sum_idx), &Value::Null);
        assert_eq!(out.value(1, mean_idx), &Value::Null);
        assert_eq!(out.value(1, min_idx), &Value::Null);
        assert_eq!(out.value(1, max_idx), &Value::Null);
        assert_eq!(out.value(1, stddev_idx), &Value::Null);

        assert_eq!(out.value(2, count_idx), &Value::Int(8));

        assert_eq!(out.value(3, count_idx), &Value::Int(0));
        assert_eq!(out.value(3, mean_idx), &Value::Null);

        let res_mb = crate::run(
            "zonalStats",
            &m,
            &serde_json::json!({
                "raster": "elev",
                "polygons": "zones",
                "bands": [1, 2],
                "stats": ["mean", "count"],
            }),
        )
        .unwrap();
        let out_mb = layer(res_mb);
        let b1_count = out_mb.field_index("elev_b1_count").unwrap();
        let b2_count = out_mb.field_index("elev_b2_count").unwrap();
        let b2_mean = out_mb.field_index("elev_b2_mean").unwrap();

        assert_eq!(out_mb.value(0, b1_count), &Value::Int(4));
        assert_eq!(out_mb.value(0, b2_count), &Value::Int(3));
        let b2_m = out_mb.value(0, b2_mean).as_f64().unwrap();
        assert!((b2_m - 245.0 / 3.0).abs() < 1e-10);

        let mut pt_ds = Dataset::new("points", GeometryType::Point, vec![]);
        pt_ds.push(Some(Geometry::Point(Coord::new(1.0, 8.0))), vec![]);
        let m_pts =
            RasterMap { datasets: HashMap::from([("points".to_string(), pt_ds)]), rasters: HashMap::from([("elev".to_string(), &raster)]) };
        let err_pt = crate::run(
            "zonalStats",
            &m_pts,
            &serde_json::json!({
                "raster": "elev",
                "polygons": "points",
            }),
        )
        .unwrap_err();
        assert!(err_pt.contains("Point data"));

        let err_band = crate::run(
            "zonalStats",
            &m,
            &serde_json::json!({
                "raster": "elev",
                "polygons": "zones",
                "bands": [5],
            }),
        )
        .unwrap_err();
        assert!(err_band.contains("band index 5 is invalid"));

        let empty_ds = Dataset::new("empty", GeometryType::Polygon, vec![Field::new("id", FieldType::Integer)]);
        let m_empty = RasterMap {
            datasets: HashMap::from([("empty".to_string(), empty_ds)]),
            rasters: HashMap::from([("elev".to_string(), &raster)]),
        };
        let res_empty = crate::run(
            "zonalStats",
            &m_empty,
            &serde_json::json!({
                "raster": "elev",
                "polygons": "empty",
            }),
        )
        .unwrap();
        let out_empty = layer(res_empty);
        assert_eq!(out_empty.features.len(), 0);
        assert!(out_empty.field_index("elev_mean").is_some());

        // 9. Bands validation: "all", integral float, decimal rejection, zero rejection, negative rejection
        let res_all_bands = crate::run(
            "zonalStats",
            &m,
            &serde_json::json!({
                "raster": "elev",
                "polygons": "zones",
                "bands": "all",
            }),
        )
        .unwrap();
        let out_all = layer(res_all_bands);
        assert!(out_all.field_index("elev_b1_mean").is_some());
        assert!(out_all.field_index("elev_b2_mean").is_some());

        let res_float_band = crate::run(
            "zonalStats",
            &m,
            &serde_json::json!({
                "raster": "elev",
                "polygons": "zones",
                "bands": [1.0],
            }),
        )
        .unwrap();
        let out_float = layer(res_float_band);
        assert!(out_float.field_index("elev_mean").is_some());

        assert!(crate::run("zonalStats", &m, &serde_json::json!({"raster": "elev", "polygons": "zones", "bands": [1.5]})).is_err());
        assert!(crate::run("zonalStats", &m, &serde_json::json!({"raster": "elev", "polygons": "zones", "bands": [0]})).is_err());
        assert!(crate::run("zonalStats", &m, &serde_json::json!({"raster": "elev", "polygons": "zones", "bands": [-1]})).is_err());

        // 10. A loaded projected layer keeps its source CRS, but its geometry is lon/lat.
        let p0 = Coord::new(1.0, 7.0);
        let p1 = Coord::new(3.0, 9.0);
        let merc_geom = Geometry::Polygon(vec![vec![vec![p0, Coord::new(p1.x, p0.y), p1, Coord::new(p0.x, p1.y), p0]]]);
        let mut merc_poly_ds = Dataset::new("merc_zones", GeometryType::Polygon, vec![Field::new("id", FieldType::Integer)]);
        merc_poly_ds.crs = Some(rustgis_data::raster::Crs::WebMercator);
        merc_poly_ds.push(Some(merc_geom.clone()), vec![Value::Int(1)]);

        let m_merc = RasterMap {
            datasets: HashMap::from([("merc_zones".to_string(), merc_poly_ds)]),
            rasters: HashMap::from([("elev".to_string(), &raster)]),
        };
        let res_merc = crate::run(
            "zonalStats",
            &m_merc,
            &serde_json::json!({
                "raster": "elev",
                "polygons": "merc_zones",
                "bands": [1],
            }),
        )
        .unwrap();
        let out_merc = layer(res_merc);
        assert_eq!(out_merc.crs, Some(rustgis_data::raster::Crs::WebMercator));
        assert_eq!(out_merc.features[0].geometry, Some(merc_geom));
        let merc_count = out_merc.field_index("elev_count").unwrap();
        let merc_mean = out_merc.field_index("elev_mean").unwrap();
        assert_eq!(out_merc.value(0, merc_count), &Value::Int(4));
        assert_eq!(out_merc.value(0, merc_mean), &Value::Float(16.5));

        // 11. Source CRS metadata does not change already-normalized geometry.
        let mut bad_crs_ds = Dataset::new("bad_crs", GeometryType::Polygon, vec![]);
        bad_crs_ds.crs = Some(rustgis_data::raster::Crs::Epsg(60000));
        bad_crs_ds.push(Some(sq_geom(1.0, 7.0, 3.0, 9.0)), vec![]);
        let m_bad_crs = RasterMap {
            datasets: HashMap::from([("bad_crs".to_string(), bad_crs_ds)]),
            rasters: HashMap::from([("elev".to_string(), &raster)]),
        };
        let out = layer(crate::run("zonalStats", &m_bad_crs, &serde_json::json!({"raster": "elev", "polygons": "bad_crs"})).unwrap());
        assert_eq!(out.value(0, out.field_index("elev_count").unwrap()), &Value::Int(4));

        // 12. An invalid exterior does not promote a valid hole.
        let hole_p0 = Coord::new(1.0, 8.0);
        let hole_p1 = Coord::new(2.0, 9.0);
        let bad_ext_geom = Geometry::Polygon(vec![vec![
            vec![p0, p1], // Only 2 points (< 3 points, invalid exterior ring)
            vec![hole_p0, Coord::new(hole_p1.x, hole_p0.y), hole_p1, Coord::new(hole_p0.x, hole_p1.y), hole_p0], // valid hole
        ]]);
        let mut drop_ext_ds = Dataset::new("drop_ext", GeometryType::Polygon, vec![Field::new("id", FieldType::Integer)]);
        drop_ext_ds.crs = Some(rustgis_data::raster::Crs::WebMercator);
        drop_ext_ds.push(Some(bad_ext_geom), vec![Value::Int(1)]);
        let m_drop_ext = RasterMap {
            datasets: HashMap::from([("drop_ext".to_string(), drop_ext_ds)]),
            rasters: HashMap::from([("elev".to_string(), &raster)]),
        };
        let res_drop_ext = crate::run("zonalStats", &m_drop_ext, &serde_json::json!({"raster": "elev", "polygons": "drop_ext"})).unwrap();
        assert!(res_drop_ext.messages.iter().any(|m| m.contains("failed coordinate transformation") || m.contains("invalid geometry")));
        let out_drop_ext = layer(res_drop_ext);
        let drop_count_idx = out_drop_ext.field_index("elev_count").unwrap();
        let drop_mean_idx = out_drop_ext.field_index("elev_mean").unwrap();
        assert_eq!(out_drop_ext.value(0, drop_count_idx), &Value::Int(0));
        assert_eq!(out_drop_ext.value(0, drop_mean_idx), &Value::Null);

        // 13. Reject a non-finite exterior vertex without silently dropping it.
        let nan_ext_geom = Geometry::Polygon(vec![vec![vec![
            Coord::new(f64::NAN, p0.y),
            Coord::new(p1.x, p0.y),
            p1,
            Coord::new(p0.x, p1.y),
            Coord::new(f64::NAN, p0.y),
        ]]]);
        let mut nan_ext_ds = Dataset::new("nan_ext", GeometryType::Polygon, vec![Field::new("id", FieldType::Integer)]);
        nan_ext_ds.crs = Some(rustgis_data::raster::Crs::WebMercator);
        nan_ext_ds.push(Some(nan_ext_geom), vec![Value::Int(1)]);
        let m_nan_ext = RasterMap {
            datasets: HashMap::from([("nan_ext".to_string(), nan_ext_ds)]),
            rasters: HashMap::from([("elev".to_string(), &raster)]),
        };
        let res_nan_ext = crate::run("zonalStats", &m_nan_ext, &serde_json::json!({"raster": "elev", "polygons": "nan_ext"})).unwrap();
        assert!(res_nan_ext.messages.iter().any(|m| m.contains("failed coordinate transformation") || m.contains("invalid geometry")));
        let out_nan_ext = layer(res_nan_ext);
        let nan_count_idx = out_nan_ext.field_index("elev_count").unwrap();
        let nan_mean_idx = out_nan_ext.field_index("elev_mean").unwrap();
        assert_eq!(out_nan_ext.value(0, nan_count_idx), &Value::Int(0));
        assert_eq!(out_nan_ext.value(0, nan_mean_idx), &Value::Null);

        // 14. A loaded projected polygon retains its hole.
        let merc_outer0 = Coord::new(0.0, 7.0);
        let merc_outer1 = Coord::new(3.0, 10.0);
        let merc_hole0 = Coord::new(1.0, 8.0);
        let merc_hole1 = Coord::new(2.0, 9.0);
        let valid_hole_geom = Geometry::Polygon(vec![vec![
            vec![merc_outer0, Coord::new(merc_outer1.x, merc_outer0.y), merc_outer1, Coord::new(merc_outer0.x, merc_outer1.y), merc_outer0],
            vec![merc_hole0, Coord::new(merc_hole1.x, merc_hole0.y), merc_hole1, Coord::new(merc_hole0.x, merc_hole1.y), merc_hole0],
        ]]);
        let mut hole_ds = Dataset::new("merc_hole", GeometryType::Polygon, vec![Field::new("id", FieldType::Integer)]);
        hole_ds.crs = Some(rustgis_data::raster::Crs::WebMercator);
        hole_ds.push(Some(valid_hole_geom), vec![Value::Int(1)]);
        let m_hole = RasterMap {
            datasets: HashMap::from([("merc_hole".to_string(), hole_ds)]),
            rasters: HashMap::from([("elev".to_string(), &raster)]),
        };
        let res_hole = crate::run("zonalStats", &m_hole, &serde_json::json!({"raster": "elev", "polygons": "merc_hole"})).unwrap();
        let out_hole = layer(res_hole);
        let hole_cnt_idx = out_hole.field_index("elev_count").unwrap();
        // 9 outer cells - 1 hole cell = 8 cells
        assert_eq!(out_hole.value(0, hole_cnt_idx), &Value::Int(8));

        assert_eq!(m.datasets["zones"], input_clone);
    }

    #[test]
    fn zonal_stats_attribute_padding_and_alignment_regression() {
        use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};

        let (w, h) = (4u32, 4u32);
        let data: Vec<f32> = (1..=16).map(|v| v as f32).collect();
        let gt = GeoTransform { origin_x: 0.0, origin_y: 4.0, pixel_width: 1.0, pixel_height: -1.0 };
        let raster = Raster::new("elev", w, h, vec![data], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();

        let fields =
            vec![Field::new("name", FieldType::Text), Field::new("category", FieldType::Text), Field::new("score", FieldType::Double)];
        let mut ds = Dataset::new("zones_pad", GeometryType::Polygon, fields);
        let sq_geom = |x0: f64, y0: f64, x1: f64, y1: f64| {
            Geometry::Polygon(vec![vec![vec![
                Coord::new(x0, y0),
                Coord::new(x1, y0),
                Coord::new(x1, y1),
                Coord::new(x0, y1),
                Coord::new(x0, y0),
            ]]])
        };
        let geom = Some(sq_geom(0.0, 3.0, 2.0, 4.0)); // 2 cells: 1.0, 2.0

        // Feature 1: shorter attribute array (only 1 attribute instead of 3)
        ds.features.push(Feature { id: 1, geometry: geom.clone(), attrs: vec![Value::Text("FeatureA".into())] });
        // Feature 2: full attribute array (3 attributes)
        ds.features.push(Feature {
            id: 2,
            geometry: geom.clone(),
            attrs: vec![Value::Text("FeatureB".into()), Value::Text("Cat1".into()), Value::Float(88.5)],
        });
        // Feature 3: empty attribute array (0 attributes)
        ds.features.push(Feature { id: 3, geometry: geom, attrs: vec![] });

        let m =
            RasterMap { datasets: HashMap::from([("zones_pad".to_string(), ds)]), rasters: HashMap::from([("elev".to_string(), &raster)]) };

        let res = crate::run("zonalStats", &m, &serde_json::json!({"raster": "elev", "polygons": "zones_pad"})).unwrap();
        let out = layer(res);

        // Schema must contain 3 original fields + 6 stat fields
        assert_eq!(out.fields.len(), 9);
        assert_eq!(out.fields[0].name, "name");
        assert_eq!(out.fields[1].name, "category");
        assert_eq!(out.fields[2].name, "score");
        assert_eq!(out.fields[3].name, "elev_mean");
        assert_eq!(out.fields[4].name, "elev_min");
        assert_eq!(out.fields[5].name, "elev_max");
        assert_eq!(out.fields[6].name, "elev_sum");
        assert_eq!(out.fields[7].name, "elev_count");
        assert_eq!(out.fields[8].name, "elev_stddev");

        // Feature 1: attrs must be padded to 3 before stats fields!
        let f1 = &out.features[0];
        assert_eq!(f1.attrs.len(), 9);
        assert_eq!(f1.attrs[0], Value::Text("FeatureA".into()));
        assert_eq!(f1.attrs[1], Value::Null, "missing category attribute must be padded with Null");
        assert_eq!(f1.attrs[2], Value::Null, "missing score attribute must be padded with Null");
        assert_eq!(f1.attrs[3], Value::Float(1.5), "elev_mean must be at index 3, not in column 1!");
        assert_eq!(f1.attrs[7], Value::Int(2), "elev_count must be at index 7");

        // Feature 2: complete attributes
        let f2 = &out.features[1];
        assert_eq!(f2.attrs.len(), 9);
        assert_eq!(f2.attrs[0], Value::Text("FeatureB".into()));
        assert_eq!(f2.attrs[1], Value::Text("Cat1".into()));
        assert_eq!(f2.attrs[2], Value::Float(88.5));
        assert_eq!(f2.attrs[3], Value::Float(1.5));

        // Feature 3: empty attributes padded completely
        let f3 = &out.features[2];
        assert_eq!(f3.attrs.len(), 9);
        assert_eq!(f3.attrs[0], Value::Null);
        assert_eq!(f3.attrs[1], Value::Null);
        assert_eq!(f3.attrs[2], Value::Null);
        assert_eq!(f3.attrs[3], Value::Float(1.5));
    }

    #[test]
    fn field_name_case_insensitive_and_collision_regression() {
        let fields = vec![Field::new("Population", FieldType::Double), Field::new("Area_KM2", FieldType::Double)];
        let mut ds = Dataset::new("cities", GeometryType::None, fields);
        ds.push(None, vec![Value::Float(100.0), Value::Float(10.0)]);
        ds.push(None, vec![Value::Float(200.0), Value::Float(20.0)]);

        // 1. Case-insensitive lookup succeeds when casing differs
        assert_eq!(find_field_case_insensitive(&ds, "population").unwrap(), 0);
        assert_eq!(find_field_case_insensitive(&ds, "POPULATION").unwrap(), 0);
        assert_eq!(find_field_case_insensitive(&ds, "area_km2").unwrap(), 1);
        assert_eq!(find_field_case_insensitive(&ds, "AREA_km2").unwrap(), 1);

        // 2. Missing field
        assert!(find_field_case_insensitive(&ds, "unknown").is_err());

        // 3. Summary statistics with lowercase lookup preserves original casing in output fields
        let m = Map(HashMap::from([("cities".to_string(), ds.clone())]));
        let res = crate::run("summaryStatistics", &m, &serde_json::json!({"input": "cities", "field": "population"})).unwrap();
        let out = layer(res);
        assert!(out.field_index("COUNT_Population").is_some(), "output field must preserve original casing");
        assert!(out.field_index("SUM_Population").is_some());
        assert!(out.field_index("MEAN_Population").is_some());

        // 4. Exact match precedence and case-insensitive collision detection
        let colliding_fields = vec![Field::new("Population", FieldType::Double), Field::new("population", FieldType::Double)];
        let ds_collision = Dataset::new("colliding", GeometryType::None, colliding_fields);
        // Exact matches must resolve directly:
        assert_eq!(find_field_case_insensitive(&ds_collision, "population").unwrap(), 1);
        assert_eq!(find_field_case_insensitive(&ds_collision, "Population").unwrap(), 0);
        // Inexact matches with multiple candidates are ambiguous:
        let err = find_field_case_insensitive(&ds_collision, "POPULATION").unwrap_err();
        assert!(err.contains("ambiguous") && err.contains("collision"), "collision must be explicitly reported: {err}");
        let err2 = find_field_case_insensitive(&ds_collision, "pOpUlAtIoN").unwrap_err();
        assert!(err2.contains("collision"));
    }

    #[test]
    fn zonal_stats_partial_feature_failures_regression() {
        use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};

        let (w, h) = (4u32, 4u32);
        let data: Vec<f32> = (1..=16).map(|v| v as f32).collect();
        let gt = GeoTransform { origin_x: 0.0, origin_y: 4.0, pixel_width: 1.0, pixel_height: -1.0 };
        let raster = Raster::new("elev", w, h, vec![data], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();

        let fields = vec![Field::new("zone_id", FieldType::Integer)];
        let mut ds = Dataset::new("mixed_zones", GeometryType::Polygon, fields);

        let sq_ring = |x0: f64, y0: f64, x1: f64, y1: f64| {
            vec![Coord::new(x0, y0), Coord::new(x1, y0), Coord::new(x1, y1), Coord::new(x0, y1), Coord::new(x0, y0)]
        };

        // Feature 1: Valid polygon with hole (outer square 0..3, inner hole 1..2)
        let f1_geom = Geometry::Polygon(vec![vec![
            sq_ring(0.0, 1.0, 3.0, 4.0), // outer 3x3 = 9 cells
            sq_ring(1.0, 2.0, 2.0, 3.0), // hole 1x1 = 1 cell
        ]]);
        ds.features.push(Feature { id: 101, geometry: Some(f1_geom), attrs: vec![Value::Int(1)] });

        // Feature 2: Defective polygon: hole has fewer than 3 vertices!
        let f2_geom = Geometry::Polygon(vec![vec![
            sq_ring(0.0, 1.0, 3.0, 4.0),
            vec![Coord::new(1.0, 2.0), Coord::new(2.0, 3.0)], // only 2 points!
        ]]);
        ds.features.push(Feature { id: 102, geometry: Some(f2_geom), attrs: vec![Value::Int(2)] });

        // Feature 3: Valid simple polygon
        let f3_geom = Geometry::Polygon(vec![vec![sq_ring(0.0, 3.0, 1.0, 4.0)]]); // 1 cell
        ds.features.push(Feature { id: 103, geometry: Some(f3_geom), attrs: vec![Value::Int(3)] });

        let m = RasterMap {
            datasets: HashMap::from([("mixed_zones".to_string(), ds)]),
            rasters: HashMap::from([("elev".to_string(), &raster)]),
        };

        let res = crate::run("zonalStats", &m, &serde_json::json!({"raster": "elev", "polygons": "mixed_zones"})).unwrap();
        let messages = res.messages.clone();
        let out = layer(res);

        assert_eq!(out.features.len(), 3);
        let count_idx = out.field_index("elev_count").unwrap();
        let mean_idx = out.field_index("elev_mean").unwrap();

        // Feature 1 succeeded (9 - 1 = 8 cells)
        assert_eq!(out.value(0, count_idx), &Value::Int(8));
        assert!(out.value(0, mean_idx).as_f64().is_some());

        // Feature 2 failed (hole was defective -> polygon rejected -> default stats)
        assert_eq!(out.value(1, count_idx), &Value::Int(0));
        assert_eq!(out.value(1, mean_idx), &Value::Null);

        // Feature 3 succeeded (1 cell)
        assert_eq!(out.value(2, count_idx), &Value::Int(1));
        assert!(out.value(2, mean_idx).as_f64().is_some());

        // Messages must explicitly report feature 102 failure
        assert!(messages.iter().any(|m| m.contains("1 feature(s) had invalid geometry")));
        assert!(messages.iter().any(|m| m.contains("feature 102") && m.contains("fewer than 3 vertices")));
    }

    #[test]
    fn parse_bands_and_stats_validation_regression() {
        use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};

        let (w, h) = (2u32, 2u32);
        let data: Vec<f32> = vec![1.0, 2.0, 3.0, 4.0];
        let gt = GeoTransform { origin_x: 0.0, origin_y: 2.0, pixel_width: 1.0, pixel_height: -1.0 };
        let raster = Raster::new("elev", w, h, vec![data], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();

        let mut ds = Dataset::new("zones", GeometryType::Polygon, vec![Field::new("id", FieldType::Integer)]);
        let sq_ring = vec![Coord::new(0.0, 0.0), Coord::new(2.0, 0.0), Coord::new(2.0, 2.0), Coord::new(0.0, 2.0), Coord::new(0.0, 0.0)];
        ds.push(Some(Geometry::Polygon(vec![vec![sq_ring]])), vec![Value::Int(1)]);

        let m = RasterMap { datasets: HashMap::from([("zones".to_string(), ds)]), rasters: HashMap::from([("elev".to_string(), &raster)]) };

        // 1. Invalid stats inputs
        assert!(crate::run("zonalStats", &m, &serde_json::json!({"raster": "elev", "polygons": "zones", "stats": []})).is_err());
        assert!(crate::run("zonalStats", &m, &serde_json::json!({"raster": "elev", "polygons": "zones", "stats": ""})).is_err());
        assert!(crate::run("zonalStats", &m, &serde_json::json!({"raster": "elev", "polygons": "zones", "stats": ["median"]})).is_err());
        assert!(crate::run("zonalStats", &m, &serde_json::json!({"raster": "elev", "polygons": "zones", "stats": [123]})).is_err());

        // 2. An empty band string defaults to band 1; malformed inputs fail.
        assert!(crate::run("zonalStats", &m, &serde_json::json!({"raster": "elev", "polygons": "zones", "bands": []})).is_err());
        assert!(crate::run("zonalStats", &m, &serde_json::json!({"raster": "elev", "polygons": "zones", "bands": ""})).is_ok());
        assert!(crate::run("zonalStats", &m, &serde_json::json!({"raster": "elev", "polygons": "zones", "bands": 1.5})).is_err());
        assert!(crate::run("zonalStats", &m, &serde_json::json!({"raster": "elev", "polygons": "zones", "bands": 99})).is_err());
        assert!(crate::run("zonalStats", &m, &serde_json::json!({"raster": "elev", "polygons": "zones", "bands": -1})).is_err());
        assert!(crate::run("zonalStats", &m, &serde_json::json!({"raster": "elev", "polygons": "zones", "bands": [""]})).is_err());
        assert!(crate::run("zonalStats", &m, &serde_json::json!({"raster": "elev", "polygons": "zones", "bands": ["  "]})).is_err());
    }

    #[test]
    fn join_field_integration_tests() {
        // 1. Vector layer joined with standalone table
        let mut target_pts = Dataset::new(
            "stations",
            GeometryType::Point,
            vec![Field::new("st_id", FieldType::Integer), Field::new("name", FieldType::Text)],
        );
        target_pts.push(Some(Geometry::Point(Coord::new(10.0, 20.0))), vec![Value::Int(101), Value::Text("North".into())]);
        target_pts.push(Some(Geometry::Point(Coord::new(11.0, 21.0))), vec![Value::Int(102), Value::Text("South".into())]);
        target_pts.push(Some(Geometry::Point(Coord::new(12.0, 22.0))), vec![Value::Int(103), Value::Text("East".into())]);

        let mut source_tab = Dataset::new(
            "climate",
            GeometryType::None,
            vec![Field::new("station", FieldType::Integer), Field::new("temp", FieldType::Double), Field::new("rain", FieldType::Double)],
        );
        source_tab.push(None, vec![Value::Int(101), Value::Float(22.5), Value::Float(120.0)]);
        source_tab.push(None, vec![Value::Int(102), Value::Float(25.0), Value::Float(80.0)]);
        // 103 has no entry in source_tab

        let m = Map(HashMap::from([("stations".to_string(), target_pts), ("climate".to_string(), source_tab)]));

        let res = crate::run(
            "joinField",
            &m,
            &serde_json::json!({
                "target": "stations",
                "targetField": "st_id",
                "joinTable": "climate",
                "joinField": "station",
                "output": "stations_with_climate"
            }),
        )
        .unwrap();

        let Output::Layer(out) = res.output else { panic!("expected Output::Layer") };
        assert_eq!(out.name, "stations_with_climate");
        assert_eq!(out.geometry_type, GeometryType::Point);
        assert_eq!(out.features.len(), 3);
        assert_eq!(out.fields.len(), 4); // st_id, name, temp, rain (station key omitted by default)
        assert_eq!(out.fields[2].name, "temp");
        assert_eq!(out.fields[3].name, "rain");

        // Feature 0: 101 matched
        assert_eq!(out.features[0].attrs[2], Value::Float(22.5));
        assert_eq!(out.features[0].attrs[3], Value::Float(120.0));
        assert_eq!(out.features[0].geometry, Some(Geometry::Point(Coord::new(10.0, 20.0))));

        // Feature 1: 102 matched
        assert_eq!(out.features[1].attrs[2], Value::Float(25.0));
        assert_eq!(out.features[1].attrs[3], Value::Float(80.0));

        // Feature 2: 103 unmatched -> padded with Null
        assert_eq!(out.features[2].attrs[2], Value::Null);
        assert_eq!(out.features[2].attrs[3], Value::Null);

        // 2. Standalone table target + explicit fields subset with prefix
        let mut target_tab = Dataset::new("inventory", GeometryType::None, vec![Field::new("sku", FieldType::Text)]);
        target_tab.push(None, vec![Value::Text("SKU-A".into())]);
        target_tab.push(None, vec![Value::Text("SKU-B".into())]);

        let mut prices = Dataset::new(
            "prices",
            GeometryType::None,
            vec![
                Field::new("item_sku", FieldType::Text),
                Field::new("price", FieldType::Double),
                Field::new("discount", FieldType::Double),
            ],
        );
        prices.push(None, vec![Value::Text("SKU-A".into()), Value::Float(19.99), Value::Float(0.1)]);
        prices.push(None, vec![Value::Text("SKU-B".into()), Value::Float(29.99), Value::Float(0.2)]);

        let m2 = Map(HashMap::from([("inventory".to_string(), target_tab), ("prices".to_string(), prices)]));

        let res2 = crate::run(
            "joinField",
            &m2,
            &serde_json::json!({
                "target": "inventory",
                "targetField": "sku",
                "join": "prices",
                "joinField": "item_sku",
                "fields": ["price"],
                "prefix": "p_"
            }),
        )
        .unwrap();

        let Output::Table(out2) = res2.output else { panic!("expected Output::Table") };
        assert_eq!(out2.name, "inventory_JoinField");
        assert_eq!(out2.fields.len(), 2);
        assert_eq!(out2.fields[1].name, "p_price");
        assert_eq!(out2.features[0].attrs[1], Value::Float(19.99));
        assert_eq!(out2.features[1].attrs[1], Value::Float(29.99));

        // 3. Duplicate strategy Error rejects duplicate source keys
        let mut dup_prices = Dataset::new(
            "dup_prices",
            GeometryType::None,
            vec![Field::new("item_sku", FieldType::Text), Field::new("price", FieldType::Double)],
        );
        dup_prices.push(None, vec![Value::Text("SKU-A".into()), Value::Float(10.0)]);
        dup_prices.push(None, vec![Value::Text("SKU-A".into()), Value::Float(15.0)]);
        let m3 = Map(HashMap::from([("inventory".to_string(), out2), ("dup_prices".to_string(), dup_prices)]));

        let err_dup = crate::run(
            "joinField",
            &m3,
            &serde_json::json!({
                "target": "inventory",
                "targetField": "sku",
                "join": "dup_prices",
                "joinField": "item_sku",
                "duplicates": "Error"
            }),
        )
        .unwrap_err();
        assert!(err_dup.contains("duplicate key"));

        // 4. Incompatible key type error (a text key matches an integer key, not a boolean one)
        let mut bool_source = Dataset::new("bool_source", GeometryType::None, vec![Field::new("flag_key", FieldType::Boolean)]);
        bool_source.push(None, vec![Value::Bool(true)]);
        let m4 = Map(HashMap::from([
            ("inventory".to_string(), m2.0.get("inventory").unwrap().clone()),
            ("bool_source".to_string(), bool_source),
        ]));
        let err_type = crate::run(
            "joinField",
            &m4,
            &serde_json::json!({
                "target": "inventory",
                "targetField": "sku",
                "join": "bool_source",
                "joinField": "flag_key"
            }),
        )
        .unwrap_err();
        assert!(err_type.contains("type mismatch"));

        // 5. Default joinField to targetField when omitted
        let mut matching_source = Dataset::new(
            "matching_source",
            GeometryType::None,
            vec![Field::new("sku", FieldType::Text), Field::new("weight", FieldType::Double)],
        );
        matching_source.push(None, vec![Value::Text("SKU-A".into()), Value::Float(1.25)]);
        let m5 = Map(HashMap::from([
            ("inventory".to_string(), m2.0.get("inventory").unwrap().clone()),
            ("matching_source".to_string(), matching_source),
        ]));
        let res5 = crate::run(
            "joinField",
            &m5,
            &serde_json::json!({
                "target": "inventory",
                "targetField": "sku",
                "join": "matching_source"
            }),
        )
        .unwrap();
        let Output::Table(out5) = res5.output else { panic!("expected Output::Table") };
        assert_eq!(out5.fields.len(), 2);
        assert_eq!(out5.fields[1].name, "weight");
        assert_eq!(out5.features[0].attrs[1], Value::Float(1.25));

        // 6. JSON array text in fields and duplicateStrategy alias
        let res6 = crate::run(
            "joinField",
            &m5,
            &serde_json::json!({
                "target": "inventory",
                "targetField": "sku",
                "join": "matching_source",
                "fields": "[\"weight\"]",
                "duplicateStrategy": "First"
            }),
        )
        .unwrap();
        let Output::Table(out6) = res6.output else { panic!("expected Output::Table") };
        assert_eq!(out6.fields.len(), 2);
        assert_eq!(out6.fields[1].name, "weight");
        assert_eq!(out6.features[0].attrs[1], Value::Float(1.25));

        // 7. duplicateStrategy "Last" selects the last matching row
        let mut dup_prices_last = Dataset::new(
            "dup_prices_last",
            GeometryType::None,
            vec![Field::new("item_sku", FieldType::Text), Field::new("price", FieldType::Double)],
        );
        dup_prices_last.push(None, vec![Value::Text("SKU-A".into()), Value::Float(10.0)]);
        dup_prices_last.push(None, vec![Value::Text("SKU-A".into()), Value::Float(20.0)]);
        let m7 = Map(HashMap::from([
            ("inventory".to_string(), m2.0.get("inventory").unwrap().clone()),
            ("dup_prices_last".to_string(), dup_prices_last),
        ]));
        let res7 = crate::run(
            "joinField",
            &m7,
            &serde_json::json!({
                "target": "inventory",
                "targetField": "sku",
                "join": "dup_prices_last",
                "joinField": "item_sku",
                "duplicateStrategy": "Last"
            }),
        )
        .unwrap();
        let Output::Table(out7) = res7.output else { panic!("expected Output::Table") };
        assert_eq!(out7.features[0].attrs[1], Value::Float(20.0));

        // 8. Whitespace-only joinField falls back to targetField
        let res8 = crate::run(
            "joinField",
            &m5,
            &serde_json::json!({
                "target": "inventory",
                "targetField": "sku",
                "join": "matching_source",
                "joinField": "   "
            }),
        )
        .unwrap();
        let Output::Table(out8) = res8.output else { panic!("expected Output::Table") };
        assert_eq!(out8.fields.len(), 2);
        assert_eq!(out8.fields[1].name, "weight");
        assert_eq!(out8.features[0].attrs[1], Value::Float(1.25));

        // 9. Reject non-string entries in fields array
        let err_non_string = crate::run(
            "joinField",
            &m5,
            &serde_json::json!({
                "target": "inventory",
                "targetField": "sku",
                "join": "matching_source",
                "fields": [42]
            }),
        )
        .unwrap_err();
        assert!(err_non_string.contains("Transfer Fields: array entries must be strings"));

        let err_mixed = crate::run(
            "joinField",
            &m5,
            &serde_json::json!({
                "target": "inventory",
                "targetField": "sku",
                "join": "matching_source",
                "fields": ["weight", true]
            }),
        )
        .unwrap_err();
        assert!(err_mixed.contains("Transfer Fields: array entries must be strings"));

        // 10. Test numeric table ID in input() and row selections
        struct TestInputs {
            datasets: HashMap<String, Dataset>,
            tables: HashMap<u32, Dataset>,
            selections: HashMap<String, Vec<usize>>,
        }
        impl Inputs for TestInputs {
            fn dataset(&self, layer: &str) -> Option<&Dataset> {
                self.datasets.get(layer)
            }
            fn table(&self, id: u32) -> Option<&Dataset> {
                self.tables.get(&id)
            }
            fn selection(&self, layer: &str) -> &[usize] {
                self.selections.get(layer).map(|v| v.as_slice()).unwrap_or(&[])
            }
        }

        let mut target_ds = Dataset::new(
            "cities",
            GeometryType::None,
            vec![Field::new("city_id", FieldType::Integer), Field::new("name", FieldType::Text)],
        );
        target_ds.push(None, vec![Value::Int(1), Value::Text("Paris".into())]);
        target_ds.push(None, vec![Value::Int(2), Value::Text("Tokyo".into())]);
        target_ds.push(None, vec![Value::Int(3), Value::Text("New York".into())]);

        let mut source_table = Dataset::new(
            "table_99",
            GeometryType::None,
            vec![Field::new("c_id", FieldType::Integer), Field::new("pop", FieldType::Integer)],
        );
        source_table.push(None, vec![Value::Int(1), Value::Int(2_100_000)]);
        source_table.push(None, vec![Value::Int(2), Value::Int(14_000_000)]);
        source_table.push(None, vec![Value::Int(3), Value::Int(8_300_000)]);

        let inp_numeric = TestInputs {
            datasets: HashMap::from([("cities".to_string(), target_ds)]),
            tables: HashMap::from([(99u32, source_table)]),
            selections: HashMap::new(),
        };

        let res_num = crate::run(
            "joinField",
            &inp_numeric,
            &serde_json::json!({
                "target": "cities",
                "targetField": "city_id",
                "joinTable": 99,
                "joinField": "c_id"
            }),
        )
        .unwrap();
        let Output::Table(out_num) = res_num.output else { panic!("expected Output::Table") };
        assert_eq!(out_num.fields.len(), 3);
        assert_eq!(out_num.fields[2].name, "pop");
        assert_eq!(out_num.features[0].attrs[2], Value::Int(2_100_000));
        assert_eq!(out_num.features[1].attrs[2], Value::Int(14_000_000));

        // 11. Row selections on target and source
        let mut target_sel =
            Dataset::new("parcels", GeometryType::None, vec![Field::new("id", FieldType::Integer), Field::new("code", FieldType::Text)]);
        target_sel.push(None, vec![Value::Int(10), Value::Text("P10".into())]);
        target_sel.push(None, vec![Value::Int(20), Value::Text("P20".into())]);
        target_sel.push(None, vec![Value::Int(30), Value::Text("P30".into())]);

        let mut source_sel =
            Dataset::new("owners", GeometryType::None, vec![Field::new("id", FieldType::Integer), Field::new("owner", FieldType::Text)]);
        // Two rows for id 10: unselected row has "Alice", selected row has "Bob"
        source_sel.push(None, vec![Value::Int(10), Value::Text("Alice".into())]);
        source_sel.push(None, vec![Value::Int(10), Value::Text("Bob".into())]);
        source_sel.push(None, vec![Value::Int(20), Value::Text("Carol".into())]);

        let inp_sel = TestInputs {
            datasets: HashMap::from([("parcels".to_string(), target_sel.clone()), ("owners".to_string(), source_sel.clone())]),
            tables: HashMap::new(),
            selections: HashMap::from([
                ("parcels".to_string(), vec![0, 1]), // Only parcels 10 and 20 selected
                ("owners".to_string(), vec![1, 2]),  // Only second row of 10 ("Bob") and 20 ("Carol") selected
            ]),
        };

        let res_sel = crate::run(
            "joinField",
            &inp_sel,
            &serde_json::json!({
                "target": "parcels",
                "targetField": "id",
                "joinTable": "owners",
                "joinField": "id"
            }),
        )
        .unwrap();
        let Output::Table(out_sel) = res_sel.output else { panic!("expected Output::Table") };
        assert_eq!(out_sel.features.len(), 2);
        assert_eq!(out_sel.features[0].attrs[0], Value::Int(10));
        assert_eq!(out_sel.features[0].attrs[2], Value::Text("Bob".into()));
        assert_eq!(out_sel.features[1].attrs[0], Value::Int(20));
        assert_eq!(out_sel.features[1].attrs[2], Value::Text("Carol".into()));
        assert_eq!(res_sel.messages.len(), 3);
        assert!(res_sel.messages[0].contains("Processing 2 selected feature(s) from target 'parcels' (out of 3)"));
        assert!(res_sel.messages[1].contains("Joining against 2 selected record(s) from source 'owners' (out of 3)"));
        assert!(res_sel.messages[2].contains("Join Field on 'parcels': 2/2 records matched"));

        // 11b. Stale selections emit informative warning messages
        let inp_stale = TestInputs {
            datasets: HashMap::from([("parcels".to_string(), target_sel.clone()), ("owners".to_string(), source_sel.clone())]),
            tables: HashMap::new(),
            selections: HashMap::from([("parcels".to_string(), vec![99, 100]), ("owners".to_string(), vec![50, 51])]),
        };
        let res_stale = crate::run(
            "joinField",
            &inp_stale,
            &serde_json::json!({
                "target": "parcels",
                "targetField": "id",
                "joinTable": "owners",
                "joinField": "id"
            }),
        )
        .unwrap();
        assert!(res_stale.messages.iter().any(|m| m.contains("selection on target 'parcels' contains only stale indices")));
        assert!(res_stale.messages.iter().any(|m| m.contains("selection on source 'owners' contains only stale indices")));

        // 12. Aliases and collision suffixes
        let mut target_col = Dataset::new(
            "items",
            GeometryType::None,
            vec![Field::new("item_id", FieldType::Integer), Field::new("price", FieldType::Double)],
        );
        target_col.push(None, vec![Value::Int(1), Value::Float(10.0)]);

        let mut source_col = Dataset::new(
            "extra",
            GeometryType::None,
            vec![Field::new("item_id", FieldType::Integer), Field::new("price", FieldType::Double)],
        );
        source_col.push(None, vec![Value::Int(1), Value::Float(15.0)]);

        let m_col = Map(HashMap::from([("items".to_string(), target_col), ("extra".to_string(), source_col)]));
        let res_col = crate::run(
            "joinField",
            &m_col,
            &serde_json::json!({
                "target": "items",
                "target_field": "item_id",
                "join": "extra",
                "join_field": "item_id",
                "fields": ["price", "item_id"]
            }),
        )
        .unwrap();
        let Output::Table(out_col) = res_col.output else { panic!("expected Output::Table") };
        assert_eq!(out_col.fields.len(), 4);
        assert_eq!(out_col.fields[1].name, "price");
        assert_eq!(out_col.fields[2].name, "price_2");
        assert_eq!(out_col.fields[3].name, "item_id_2");
        assert_eq!(out_col.features[0].attrs[1], Value::Float(10.0));
        assert_eq!(out_col.features[0].attrs[2], Value::Float(15.0));
        assert_eq!(out_col.features[0].attrs[3], Value::Int(1));

        // 13. "input" alias for target through complete run() lifecycle
        let res13 = crate::run(
            "joinField",
            &m5,
            &serde_json::json!({
                "input": "inventory",
                "key": "sku",
                "source": "matching_source",
            }),
        )
        .unwrap();
        let Output::Table(out13) = res13.output else { panic!("expected Output::Table") };
        assert_eq!(out13.name, "inventory_JoinField");
        assert_eq!(out13.fields.len(), 2);
        assert_eq!(out13.fields[1].name, "weight");
        assert_eq!(out13.features[0].attrs[1], Value::Float(1.25));

        // 14. Explicitly empty array and blank-only array in fields transfers zero fields
        let res14_empty = crate::run(
            "joinField",
            &m5,
            &serde_json::json!({
                "target": "inventory",
                "targetField": "sku",
                "joinTable": "matching_source",
                "fields": []
            }),
        )
        .unwrap();
        let Output::Table(out14_empty) = res14_empty.output else { panic!("expected Output::Table") };
        assert_eq!(out14_empty.fields.len(), 1);
        assert_eq!(out14_empty.features[0].attrs.len(), 1);

        let res14_blank = crate::run(
            "joinField",
            &m5,
            &serde_json::json!({
                "target": "inventory",
                "targetField": "sku",
                "joinTable": "matching_source",
                "fields": ["   ", ""]
            }),
        )
        .unwrap();
        let Output::Table(out14_blank) = res14_blank.output else { panic!("expected Output::Table") };
        assert_eq!(out14_blank.fields.len(), 1);
        assert_eq!(out14_blank.features[0].attrs.len(), 1);

        // 15. Repeated field names in fields are deduplicated
        let res15 = crate::run(
            "joinField",
            &m5,
            &serde_json::json!({
                "target": "inventory",
                "targetField": "sku",
                "joinTable": "matching_source",
                "fields": ["weight", "weight"]
            }),
        )
        .unwrap();
        let Output::Table(out15) = res15.output else { panic!("expected Output::Table") };
        assert_eq!(out15.fields.len(), 2);
        assert_eq!(out15.fields[1].name, "weight");
        assert_eq!(out15.features[0].attrs[1], Value::Float(1.25));

        // 16. Duplicate index selection [0, 0] dedupes to 1 selected row in table order
        let inp_dup_sel = TestInputs {
            datasets: HashMap::from([("parcels".to_string(), target_sel.clone()), ("owners".to_string(), source_sel.clone())]),
            tables: HashMap::new(),
            selections: HashMap::from([("parcels".to_string(), vec![0, 0])]),
        };
        let res16 = crate::run(
            "joinField",
            &inp_dup_sel,
            &serde_json::json!({
                "target": "parcels",
                "targetField": "id",
                "joinTable": "owners",
                "joinField": "id"
            }),
        )
        .unwrap();
        let Output::Table(out16) = res16.output else { panic!("expected Output::Table") };
        assert_eq!(out16.features.len(), 1);
        assert_eq!(out16.features[0].attrs[0], Value::Int(10));

        // 17. Blank join_field falls back to join_key
        let res17 = crate::run(
            "joinField",
            &m5,
            &serde_json::json!({
                "target": "inventory",
                "targetField": "sku",
                "joinTable": "matching_source",
                "join_field": "",
                "join_key": "sku"
            }),
        )
        .unwrap();
        let Output::Table(out17) = res17.output else { panic!("expected Output::Table") };
        assert_eq!(out17.features[0].attrs[1], Value::Float(1.25));

        // 18. Invalid fields type (e.g. integer 5) is rejected
        let res18 = crate::run(
            "joinField",
            &m5,
            &serde_json::json!({
                "target": "inventory",
                "targetField": "sku",
                "joinTable": "matching_source",
                "joinField": "sku",
                "fields": 5
            }),
        );
        assert!(res18.is_err());
        assert_eq!(res18.unwrap_err(), "Transfer Fields must be a string or JSON array");
    }
}
