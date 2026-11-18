"""Generated polygon overlays for generate.py's pinned GEOS environment."""

import argparse
import hashlib
import json
import os
import random
import subprocess
import tempfile
from pathlib import Path

import shapely
from shapely.affinity import affine_transform
from shapely.geometry import MultiPolygon, Polygon, box, mapping, shape

TOOLS = ("clip", "intersect", "union", "erase")
FIXED_SEEDS = (1, 2, 3, 4, 5, 6)
REGRESSIONS = Path(__file__).with_name("overlay-regressions")
MASK_SEAM = "gap between rounded clip masks"
# The fixed corpus allows 0.1 ppm of the answer for the overlay's rounding. Scale by input
# area here: the rounding is driven by the inputs even when an intersection is empty or tiny.
AREA_TOLERANCE = 1e-7


def jagged_polygon(rng, x, width, height):
    """Two disjoint chains with increasing x form a simple ring, often concave."""
    xs = sorted(rng.sample(range(x + 1, x + width), rng.randint(3, 9)))
    bottom = [(i, rng.randrange(height // 3)) for i in xs]
    top = [(i, rng.randrange(2 * height // 3, height)) for i in reversed(xs)]
    return Polygon([(x, height // 2), *bottom, (x + width, height // 2), *top])


def generated(seed, count):
    """Cycle through the interactions on each seed, varying position, sizes and exact vertices."""
    rng = random.Random(seed)
    for i in range(count):
        w, h = rng.randint(16, 36), rng.randint(16, 36)
        x, y = rng.randrange(3, w - 3), rng.randrange(3, h - 3)
        a = [box(0, 0, w, h)]
        match i % 8:
            case 0:  # A strip crosses a hole, splitting the intersection.
                hole = box(rng.randint(1, x - 1), rng.randint(1, 4), rng.randint(x + 3, w - 1), h - rng.randint(1, 4))
                a = [Polygon(a[0].exterior.coords, [hole.exterior.coords])]
                b = [box(x, -2, x + 2, h + 2)]
            case 1:  # An oblique sliver or a narrow overlap/gap along a shared side.
                delta = rng.choice((-1, 1)) * 2 ** -rng.randint(8, 15)
                if delta > 0 and x % 2 == 0:
                    b = [Polygon([(w - x, y), (w - x + delta, y), (w + x + delta, h + 3), (w + x, h + 3)])]
                else:
                    b = [box(w - delta, y, w + x, h + 3)]
            case 2:  # Corners that touch or nearly touch on either side.
                delta = rng.choice((-1, 0, 1)) * 2 ** -rng.randint(5, 8)
                b = [box(w + delta, h + delta, w + x, h + y)]
            case 3:  # Partial, exactly collinear shared edges, with an extra vertex.
                b = [Polygon([(w, y), (w + x, y), (w + x, h + y), (w, h + y), (w, h)])]
            case 4:  # The boundary of the overlapping area also contains shared edges.
                b = [box(x, 0, w + x, h)]
            case 5:  # Several disjoint parts in each feature.
                a = [MultiPolygon([a[0], box(w + 2, 0, 2 * w, h)])]
                b = [MultiPolygon([box(x, y, w + x, y + 1), box(2 * w + 2, y, 2 * w + x, h)])]
            case 6:  # Layers with overlapping features and several candidates per feature.
                a.append(box(-x, y, w - x, h + y))
                b = [box(x, -y, w + x, h - y), box(-x, -y, w - x, h + y)]
            case 7:  # Arbitrary non-axis-aligned polygons, independently sampled.
                a, b = ([jagged_polygon(rng, offset, w, h)] for offset in (0, x))

        # Powers of two preserve coincident coordinates after translation. All shapes stay
        # away from the antimeridian and Mercator's latitude limit.
        scale = 2 ** -rng.randint(5, 10)
        sx, sy = (scale * rng.choice((-1, 1)) for _ in range(2))
        matrix = [sx, 0, 0, sy] if rng.randrange(2) else [0, sx, sy, 0]
        matrix += [rng.randint(-160, 160), rng.randint(-72, 72)]
        yield {
            "a": [mapping(affine_transform(g, matrix)) for g in a],
            "b": [mapping(affine_transform(g, matrix)) for g in b],
            "tools": TOOLS,
        }


def polygon_parts(g):
    """Discard point and line contacts, even in a mixed GEOS GeometryCollection."""
    if g.geom_type == "Polygon":
        return [] if g.is_empty else [g]
    if g.geom_type in ("GeometryCollection", "MultiPolygon"):
        return [p for member in g.geoms for p in polygon_parts(member)]
    return []


def answers(tool, a, b):
    """Compare each attribute partition, including the uncovered pieces from both Union layers."""
    if tool in ("clip", "erase"):
        mask = shapely.union_all(b)
        op = shapely.intersection if tool == "clip" else shapely.difference
        return {str(i): op(g, mask) for i, g in enumerate(a)}

    rows = {f"{i}|{j}": g.intersection(h) for i, g in enumerate(a) for j, h in enumerate(b)}
    if tool == "union":
        cover_a, cover_b = shapely.union_all(a), shapely.union_all(b)
        rows.update({f"{i}|": g.difference(cover_b) for i, g in enumerate(a)})
        rows.update({f"|{j}": h.difference(cover_a) for j, h in enumerate(b)})
    return rows


def clip_mask_seam(parts, mask, count):
    """Identify the known Clip split where independently rounded masks open a tiny gap."""
    if len(mask) < 2 or len(parts) <= count:
        return False
    x0, y0, x1, y1 = shapely.total_bounds(mask)
    # Use a few steps of the overlay integer grid, as Clip does to search for nearby masks.
    step = max(x1 - x0, y1 - y0) / 2 ** 27
    return len(polygon_parts(shapely.union_all([p.buffer(step) for p in parts]))) == count


def compare(tool, reply, a, b):
    """Return the first difference per tool. Area ignores ring direction, start and subdivisions."""
    rows = reply[tool]
    if isinstance(rows, str):
        return "tool-error", rows

    limit = AREA_TOLERANCE * sum(g.area for g in a + b)
    want = answers(tool, a, b)
    got = {}
    for key, coordinates in rows:
        if coordinates == "non-polygon":
            return "non-polygon", repr(key)
        if not coordinates:
            continue  # The empty polygon may be encoded as no row, null or no rings.
        if key in got:
            return "duplicate-row", repr(key)
        g = from_rings(coordinates)
        if not g.is_valid:
            reason = shapely.is_valid_reason(g)
            return f"invalid {reason.partition('[')[0].lower()}", f"{key}: {reason}"
        got[key] = g

    for key in sorted(got.keys() | want.keys()):
        expected = MultiPolygon(polygon_parts(want[key])) if key in want else MultiPolygon()
        actual = got.get(key, MultiPolygon())
        off = actual.symmetric_difference(expected).area
        if off > limit:
            return "area", f"{key}: {off:.9g} m² unshared, limit {limit:.9g} m²"
        # Count only parts large enough to resolve at the area tolerance. Counting features
        # across keys would conflate real splits with overlapping features from different inputs.
        parts = [p for p in actual.geoms if p.area > limit]
        n, m = len(parts), sum(p.area > limit for p in expected.geoms)
        if n != m:
            kind = MASK_SEAM if tool == "clip" and clip_mask_seam(parts, b, m) else "parts"
            return kind, f"{key}: {n} polygon parts, GEOS {m} (above {limit:.9g} m²)"
    return None


def from_rings(polygons):
    return MultiPolygon([Polygon(rings[0], rings[1:]) for rings in polygons])


class Runner:
    """Use the compiled reference test as a batch adapter, without compiling during shrinking."""

    def __init__(self, executable, reply):
        self.executable = executable.resolve()
        self.reply = reply

    def check(self, cases):
        self.reply.unlink(missing_ok=True)  # A stale reply must never look like a successful batch.
        completed = subprocess.run(
            [self.executable, "generated_overlay_batch", "--exact", "--ignored"],
            input=json.dumps(cases), text=True, stdout=subprocess.PIPE,
            env={**os.environ, "RUSTGIS_OVERLAY_REPLY": str(self.reply)}, timeout=60,
        )
        if completed.returncode:
            raise RuntimeError(f"RustGIS reference adapter failed:\n{completed.stdout}")
        replies = json.loads(self.reply.read_text())
        return [self.differences(case, reply) for case, reply in zip(cases, replies, strict=True)]

    @staticmethod
    def differences(case, reply):
        a, b = ([from_rings(g) for g in reply[side]] for side in ("a", "b"))
        if any(not g.is_valid or g.is_empty for g in a + b):
            return None  # A vertex deletion can invalidate a projected polygon.
        return {tool: diff for tool in case["tools"] if (diff := compare(tool, reply, a, b))}


def reductions(case):
    """Every single deletion, leaving both layers nonempty and each exterior a closed ring."""
    for side in ("a", "b"):
        layer = case[side]
        for i, g in enumerate(layer):
            if len(layer) > 1:
                yield {**case, side: layer[:i] + layer[i + 1:]}
            parts = list(polygon_parts(shape(g)))
            for j, p in enumerate(parts):
                if len(parts) > 1:
                    yield replace(case, side, i, parts[:j] + parts[j + 1:])
                rings = [list(r.coords)[:-1] for r in (p.exterior, *p.interiors)]
                for k, ring in enumerate(rings):
                    candidates = [rings[:k] + rings[k + 1:]] if k else []
                    if len(ring) > 3:
                        candidates += [
                            rings[:k] + [ring[:v] + ring[v + 1:]] + rings[k + 1:]
                            for v in range(len(ring))
                        ]
                    for new in candidates:
                        polygon = Polygon(new[0], new[1:])
                        yield replace(case, side, i, parts[:j] + [polygon] + parts[j + 1:])


def replace(case, side, i, parts):
    g = parts[0] if len(parts) == 1 else MultiPolygon(parts)
    layer = case[side]
    return {**case, side: [*layer[:i], mapping(g), *layer[i + 1:]]}


def shrink(runner, case, tool, kind):
    """Keep the failure class and tool fixed. Restart after each deletion to a local minimum."""
    current = {**case, "tools": [tool]}
    while candidates := list(reductions(current)):
        results = runner.check(candidates)
        for candidate, differences in zip(candidates, results, strict=True):
            if differences and differences.get(tool, (None,))[0] == kind:
                current = candidate
                break
        else:
            break
    return current


def fingerprint(case, tool, kind):
    """Identify repeated minimized inputs independent of feature, part and vertex order."""
    sides = [
        sorted(shapely.to_wkb(MultiPolygon(polygon_parts(shape(g))).normalize(), hex=True, byte_order=1) for g in case[side])
        for side in ("a", "b")
    ]
    return hashlib.sha256(json.dumps([tool, kind, sides]).encode()).hexdigest()


def check_regressions(runner):
    """Check fixed reduced cases with the same oracle, allowing only the recorded Clip split."""
    paths = sorted(REGRESSIONS.glob("*.json"))
    fixtures = [json.loads(path.read_text()) for path in paths]
    failures = 0
    for path, fixture, differences in zip(paths, fixtures, runner.check([f["case"] for f in fixtures]), strict=True):
        if differences is None:
            raise ValueError(f"{path.name}: invalid fixed overlay input")
        for tool, (kind, detail) in differences.items():
            known = kind == fixture.get("known")
            failures += not known
            status = "known disagreement" if known else "disagreement"
            print(f"{path.name}: {status} in {tool}: {kind}: {detail}", flush=True)
        # As in reference.rs, a known case that agrees again fails until its mark is removed.
        if fixture.get("known") and not any(kind == fixture["known"] for kind, _ in differences.values()):
            failures += 1
            print(f"{path.name}: the known disagreement is gone; remove the fixture's known mark", flush=True)
    return failures


def run(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("runner", type=Path, help="compiled rustgis-analysis reference test")
    parser.add_argument("--case", type=Path, help="replay one minimized failure JSON file")
    parser.add_argument("--findings", type=Path, required=True, help="new directory for minimized disagreements")
    args = parser.parse_args(argv)
    if args.findings.exists():
        parser.error("--findings must be a new path")
    count = int(os.environ.get("PROPERTY_CASES", "16"))
    if count < 1:
        parser.error("PROPERTY_CASES must be positive")
    seed = os.environ.get("PROPERTY_SEED")
    seeds = FIXED_SEEDS if seed is None else (int(seed),)
    if args.case:
        replay = json.loads(args.case.read_text())
        series = [(replay["seed"], [(replay["index"], replay["case"])])]
    else:
        series = [(seed, enumerate(generated(seed, count))) for seed in seeds]

    failures = 0
    with tempfile.TemporaryDirectory() as tmp:
        runner = Runner(args.runner, Path(tmp) / "reply.json")
        if not args.case and seed is None:
            failures += check_regressions(runner)
        for seed, indexed in series:
            cases = list(indexed)
            print(f"Overlay seed {seed}: {len(cases)} cases", flush=True)
            results = runner.check([case for _, case in cases])
            for (index, case), differences in zip(cases, results, strict=True):
                if differences is None:
                    raise ValueError(f"seed {seed}, case {index}: invalid generated input")
                for tool, (kind, detail) in differences.items():
                    failures += 1
                    small = shrink(runner, case, tool, kind)
                    minimized = runner.check([small])[0][tool][1]
                    digest = fingerprint(small, tool, kind)
                    args.findings.mkdir(parents=True, exist_ok=True)
                    file = args.findings / f"{tool}-{digest[:16]}.json"
                    finding = {
                        # Keep all 64 bits when tools such as jq read the issue data.
                        "seed": str(seed), "index": index, "kind": kind, "detail": minimized,
                        "fingerprint": "clip-mask-seam" if kind == MASK_SEAM else digest,
                        "case": small,
                    }
                    if not file.exists():
                        file.write_text(json.dumps(finding, separators=(",", ":")) + "\n")
                    print(f"{tool} seed {seed} case {index}: {kind}: {detail}; minimized at {file}", flush=True)
    print(f"{failures} overlay disagreements")
    return int(failures != 0)
