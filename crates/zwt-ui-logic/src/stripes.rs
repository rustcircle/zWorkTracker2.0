//! Diagonal stripes cut to a shape, as polygons a Slint `Path` can fill: how 1.x draws a combination of tags.
//!
//! 1.x fills a combination's slice of the Share of time donut with an SVG pattern (`features/analysis/charts.tsx`
//! `StripePattern`: one rect [`DONUT_STRIPE`] px wide per colour, `patternTransform="rotate(45)"`, in the user space
//! of the chart's `<svg>`, so the stripes start at its top-left corner) and the legend swatch with a CSS
//! `repeating-linear-gradient(135deg, ...)` of [`SWATCH_STRIPE`] px a colour (`stripesBackground`, started at the
//! corner of the swatch's padding box). Both give bands between the lines `x + y = c`, rising to the right, colour
//! `k mod n` in band `k`.
//!
//! Slint cannot clip a pattern to an arc, so the bands are cut here: the shape is split into small convex cells, each
//! cell is clipped to each band it crosses (Sutherland-Hodgman against the band's two lines), and the piece goes to
//! its band's colour. The pieces tile the shape: they do not overlap, and their areas add up to the shape's.

use std::f64::consts::FRAC_1_SQRT_2;
use std::fmt::Write as _;

/// A point in pixels, y down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// A closed polygon; the last point joins the first.
pub type Polygon = Vec<Point>;

/// The width of one colour's stripe on the donut, across the stripe (charts.tsx `STRIPE`).
pub const DONUT_STRIPE: f64 = 4.0;
/// The width of one colour's stripe in a legend swatch, across the stripe (charts.tsx `stripesBackground`).
pub const SWATCH_STRIPE: f64 = 3.0;
/// The legend swatch: 12 px with a 1 px border (analysis.css `.a-share-swatch`, `box-sizing: border-box`).
pub const SWATCH_SIZE: f64 = 12.0;
pub const SWATCH_BORDER: f64 = 1.0;

/// Bands of equal width between the lines `x + y = c`: band `k` holds the points whose distance from `origin`, along
/// (1, 1) / sqrt 2, is in `[k * width, (k + 1) * width)`, and is drawn in colour `k mod colors`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stripes {
    pub origin: Point,
    pub width: f64,
    pub colors: usize,
}

impl Stripes {
    /// The donut's stripes: `rotate(45)` turns the pattern's vertical rects about the `<svg>`'s origin, which is the
    /// top-left corner of the ring's box ([`Ring::DONUT`] uses the same box).
    #[must_use]
    pub const fn donut(colors: usize) -> Self {
        Self {
            origin: Point::new(0.0, 0.0),
            width: DONUT_STRIPE,
            colors,
        }
    }

    /// A swatch's stripes: a 135deg gradient starts on the line through the corner of the gradient's box, the
    /// swatch's padding box inside its 1 px border.
    #[must_use]
    pub const fn swatch(colors: usize) -> Self {
        Self {
            origin: Point::new(SWATCH_BORDER, SWATCH_BORDER),
            width: SWATCH_STRIPE,
            colors,
        }
    }

    /// The distance of `p` from the origin's line, across the stripes.
    #[must_use]
    pub fn offset(&self, p: Point) -> f64 {
        ((p.x - self.origin.x) + (p.y - self.origin.y)) * FRAC_1_SQRT_2
    }

    /// The band `p` is in.
    #[must_use]
    pub fn band(&self, p: Point) -> i64 {
        // Pixel coordinates are far inside i64.
        (self.offset(p) / self.width).floor() as i64
    }

    /// The colour of band `band`: `band mod colors`, also for bands before the origin.
    #[must_use]
    pub fn color_of_band(&self, band: i64) -> usize {
        let n = i64::try_from(self.colors.max(1)).unwrap_or(1);
        usize::try_from(band.rem_euclid(n)).unwrap_or(0)
    }
}

/// A ring around `centre`, between two radii.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ring {
    pub centre: Point,
    pub outer: f64,
    pub inner: f64,
}

impl Ring {
    /// The Share of time donut (charts.tsx `Donut` in a 252 px box: R = 120, r = 0.64 R; ui/share.slint `Ring`).
    pub const DONUT: Self = Self {
        centre: Point::new(126.0, 126.0),
        outer: 120.0,
        inner: 76.8,
    };

    /// The point at `radius` and `deg` degrees clockwise from the top (as `ShareArc` counts).
    #[must_use]
    pub fn at(&self, radius: f64, deg: f64) -> Point {
        let a = (deg - 90.0).to_radians();
        Point::new(
            self.centre.x + radius * a.cos(),
            self.centre.y + radius * a.sin(),
        )
    }
}

/// The widest cell of a sector, in degrees. Each cell's inner edge is one chord: it reaches into the hole by at most
/// `inner * (1 - cos 2deg)` = 0.05 px on the donut.
pub const MAX_CELL_DEG: f64 = 4.0;
/// The widest step along the outer arc, in degrees: the chord stays within 0.02 px of the donut's outer circle.
pub const MAX_ARC_STEP_DEG: f64 = 1.0;

fn steps(span: f64, max: f64) -> usize {
    // A sector is at most a turn, so a few hundred steps at most.
    (span / max).ceil().clamp(1.0, 100_000.0) as usize
}

/// The sector of `ring` from `from_deg` to `to_deg` (clockwise from the top) as convex cells, in order. Each cell runs
/// from its inner start point out to the outer arc, along it, and back in: clockwise on screen. Empty when the sector
/// is empty.
#[must_use]
pub fn sector_cells(ring: &Ring, from_deg: f64, to_deg: f64) -> Vec<Polygon> {
    // `f64::min` would turn a NaN span into a whole turn: test it first.
    let span = to_deg - from_deg;
    if span.is_nan()
        || span <= 0.0
        || ring.outer.is_nan()
        || ring.outer <= ring.inner
        || ring.inner < 0.0
    {
        return Vec::new();
    }
    let span = span.min(360.0);
    let cells = steps(span, MAX_CELL_DEG);
    let cell_span = span / cells as f64;
    (0..cells)
        .map(|c| {
            let a0 = from_deg + cell_span * c as f64;
            let a1 = if c + 1 == cells {
                from_deg + span
            } else {
                a0 + cell_span
            };
            let arc = steps(a1 - a0, MAX_ARC_STEP_DEG);
            let mut cell = Vec::with_capacity(arc + 3);
            cell.push(ring.at(ring.inner, a0));
            for k in 0..=arc {
                let a = if k == arc {
                    a1
                } else {
                    a0 + (a1 - a0) * k as f64 / arc as f64
                };
                cell.push(ring.at(ring.outer, a));
            }
            cell.push(ring.at(ring.inner, a1));
            cell
        })
        .collect()
}

/// The part of `poly` (convex) where `side(p) >= 0`, for an affine `side`: one Sutherland-Hodgman step.
fn clip_half_plane(poly: &[Point], side: impl Fn(Point) -> f64) -> Polygon {
    let mut out = Vec::with_capacity(poly.len() + 2);
    let Some(&last) = poly.last() else {
        return out;
    };
    let mut prev = last;
    let mut prev_side = side(prev);
    for &p in poly {
        let s = side(p);
        if s >= 0.0 {
            // A point on the line is kept as it is: no second copy of it as a crossing.
            if prev_side < 0.0 && s > 0.0 {
                out.push(crossing(prev, prev_side, p, s));
            }
            out.push(p);
        } else if prev_side > 0.0 {
            out.push(crossing(prev, prev_side, p, s));
        }
        prev = p;
        prev_side = s;
    }
    out
}

/// Where the segment from `a` (side `sa`) to `b` (side `sb`, of the other sign) crosses the line `side = 0`.
fn crossing(a: Point, sa: f64, b: Point, sb: f64) -> Point {
    let t = sa / (sa - sb);
    Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

/// Twice the signed area of `poly` (positive when it turns clockwise on screen, y down).
#[must_use]
pub fn twice_signed_area(poly: &[Point]) -> f64 {
    poly.iter()
        .zip(poly.iter().cycle().skip(1))
        .map(|(a, b)| a.x * b.y - b.x * a.y)
        .sum()
}

/// The area of `poly`, whatever its orientation.
#[must_use]
pub fn area(poly: &[Point]) -> f64 {
    twice_signed_area(poly).abs() / 2.0
}

/// Pieces smaller than this (px squared) are dropped: slivers where a band only touches a cell's corner.
const MIN_PIECE_AREA: f64 = 1e-6;

/// `cells` (convex polygons) cut by `stripes`: one list of pieces per colour, colour 0 first.
#[must_use]
pub fn stripe_pieces(cells: &[Polygon], stripes: &Stripes) -> Vec<Vec<Polygon>> {
    let mut out: Vec<Vec<Polygon>> = vec![Vec::new(); stripes.colors.max(1)];
    if stripes.width.is_nan() || stripes.width <= 0.0 {
        return out;
    }
    for cell in cells {
        let offsets = cell.iter().map(|&p| stripes.offset(p));
        let (lo, hi) = offsets.fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), t| {
            (lo.min(t), hi.max(t))
        });
        if lo.is_nan() || hi.is_nan() || lo > hi {
            continue;
        }
        let first = (lo / stripes.width).floor() as i64;
        let last = (hi / stripes.width).floor() as i64;
        for band in first..=last {
            let start = band as f64 * stripes.width;
            let end = start + stripes.width;
            let piece = clip_half_plane(cell, |p| stripes.offset(p) - start);
            let piece = clip_half_plane(&piece, |p| end - stripes.offset(p));
            if piece.len() >= 3
                && area(&piece) > MIN_PIECE_AREA
                && let Some(list) = out.get_mut(stripes.color_of_band(band))
            {
                list.push(piece);
            }
        }
    }
    out
}

/// The striped pieces of a combination's slice of the donut, per colour (colour 0 first).
#[must_use]
pub fn sector_stripes(ring: &Ring, from_deg: f64, to_deg: f64, colors: usize) -> Vec<Vec<Polygon>> {
    stripe_pieces(
        &sector_cells(ring, from_deg, to_deg),
        &Stripes::donut(colors),
    )
}

/// The striped pieces of a combination's legend swatch, per colour (colour 0 first): its padding box, the 10 px inside
/// the border, which is all the gradient shows (the border is opaque).
#[must_use]
pub fn swatch_stripes(colors: usize) -> Vec<Vec<Polygon>> {
    let (a, b) = (SWATCH_BORDER, SWATCH_SIZE - SWATCH_BORDER);
    let square = vec![
        Point::new(a, a),
        Point::new(b, a),
        Point::new(b, b),
        Point::new(a, b),
    ];
    stripe_pieces(&[square], &Stripes::swatch(colors))
}

/// SVG path commands for `polygons`, in pixels: one closed subpath each, coordinates to 1/1000 px.
#[must_use]
pub fn svg_path(polygons: &[Polygon]) -> String {
    let mut out = String::new();
    for poly in polygons {
        for (i, p) in poly.iter().enumerate() {
            let cmd = if i == 0 { 'M' } else { 'L' };
            let _ = write!(out, "{cmd}{} {}", num(p.x), num(p.y));
        }
        if !poly.is_empty() {
            out.push('Z');
        }
    }
    out
}

/// A coordinate to 1/1000 px without trailing zeros ("12.5", "-3", "0.125").
fn num(v: f64) -> String {
    let mut s = format!("{v:.3}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" { "0".to_owned() } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_short() {
        assert_eq!(num(12.5), "12.5");
        assert_eq!(num(-3.0), "-3");
        assert_eq!(num(0.1254), "0.125");
        assert_eq!(num(-0.0001), "0");
    }

    #[test]
    fn a_square_cut_in_two_bands() {
        // A 4 x 4 square at the origin is 4 sqrt 2 across the stripes: two bands of 2 sqrt 2 split it along its
        // other diagonal into two triangles of 8 px each.
        let square = vec![
            Point::new(0.0, 0.0),
            Point::new(4.0, 0.0),
            Point::new(4.0, 4.0),
            Point::new(0.0, 4.0),
        ];
        let stripes = Stripes {
            origin: Point::new(0.0, 0.0),
            width: 2.0 * std::f64::consts::SQRT_2,
            colors: 2,
        };
        let pieces = stripe_pieces(&[square], &stripes);
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0].len(), 1);
        assert_eq!(pieces[1].len(), 1);
        assert!((area(&pieces[0][0]) - 8.0).abs() < 1e-9);
        assert!((area(&pieces[1][0]) - 8.0).abs() < 1e-9);
        assert_eq!(
            svg_path(&pieces[0]),
            "M0 0L4 0L0 4Z",
            "the upper-left triangle"
        );
    }
}
