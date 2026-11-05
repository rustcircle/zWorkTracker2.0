//! The stripes of a combination of tags (`zwt_ui_logic::stripes`), against 1.x's geometry (`features/analysis/charts.tsx`
//! `StripePattern` and `stripesBackground`): the pieces tile the slice, stay inside it and inside their band, and take
//! the colour of their band as the SVG pattern and the CSS gradient do. Properties are checked over a seeded sweep of
//! slices (the workspace has no property-testing crate; the generator below is a plain xorshift).

use std::f64::consts::{FRAC_1_SQRT_2, PI};

use zwt_ui_logic::stripes::{
    DONUT_STRIPE, MAX_CELL_DEG, Point, Polygon, Ring, SWATCH_STRIPE, Stripes, area, sector_cells,
    sector_stripes, stripe_pieces, svg_path, swatch_stripes, twice_signed_area,
};

/// xorshift64*: a fixed seed, the same slices on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [lo, hi).
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        let unit = (self.next() >> 11) as f64 / (1u64 << 53) as f64;
        lo + (hi - lo) * unit
    }
}

/// Random slices of the donut: start, end (degrees clockwise from the top), colours. Includes tiny and whole-ring
/// slices.
fn slices() -> Vec<(f64, f64, usize)> {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut out = vec![
        (0.0, 360.0, 2),
        (0.0, 360.0, 3),
        (300.0, 300.25, 2),
        (12.5, 13.0, 4),
        (88.0, 272.0, 2),
    ];
    for _ in 0..200 {
        let from = rng.range(0.0, 359.0);
        let span = rng.range(0.05, 360.0 - from);
        let colors = 2 + (rng.next() % 4) as usize;
        out.push((from, from + span, colors));
    }
    out
}

fn sector_area(ring: &Ring, from: f64, to: f64) -> f64 {
    PI * (ring.outer * ring.outer - ring.inner * ring.inner) * (to - from) / 360.0
}

/// Degrees clockwise from the top of `p` around the ring's centre, in [0, 360).
fn angle_of(ring: &Ring, p: Point) -> f64 {
    let a = (p.y - ring.centre.y)
        .atan2(p.x - ring.centre.x)
        .to_degrees()
        + 90.0;
    a.rem_euclid(360.0)
}

fn radius_of(ring: &Ring, p: Point) -> f64 {
    (p.x - ring.centre.x).hypot(p.y - ring.centre.y)
}

fn is_convex(poly: &Polygon) -> bool {
    let n = poly.len();
    let mut sign = 0.0_f64;
    for i in 0..n {
        let (a, b, c) = (poly[i], poly[(i + 1) % n], poly[(i + 2) % n]);
        let cross = (b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x);
        if cross.abs() < 1e-9 {
            continue;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return false;
        }
    }
    true
}

fn contains(poly: &Polygon, p: Point) -> bool {
    let orientation = twice_signed_area(poly).signum();
    let n = poly.len();
    (0..n).all(|i| {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        ((b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)) * orientation >= -1e-9
    })
}

#[test]
fn the_pieces_tile_the_slice() {
    let ring = Ring::DONUT;
    for (from, to, colors) in slices() {
        let cells = sector_cells(&ring, from, to);
        let cells_area: f64 = cells.iter().map(|c| area(c)).sum();
        let pieces = sector_stripes(&ring, from, to, colors);
        assert_eq!(pieces.len(), colors, "one list per colour");
        let pieces_area: f64 = pieces.iter().flatten().map(|p| area(p)).sum();
        // The bands split every cell without gaps or overlaps.
        assert!(
            (pieces_area - cells_area).abs() <= 1e-6 * cells_area.max(1.0),
            "{from}..{to} x{colors}: pieces {pieces_area} vs cells {cells_area}"
        );
        // The cells follow the true sector: the outer arc in chords of 1 degree, the inner one in chords of 4.
        let exact = sector_area(&ring, from, to);
        assert!(
            (cells_area - exact).abs() <= 0.002 * exact + 0.01,
            "{from}..{to}: cells {cells_area} vs sector {exact}"
        );
    }
}

#[test]
fn no_piece_leaves_its_slice_or_its_band() {
    let ring = Ring::DONUT;
    // The inner edge of a cell is a chord: it reaches into the hole by at most inner * (1 - cos(cell / 2)).
    let sagitta = ring.inner * (1.0 - (MAX_CELL_DEG.to_radians() / 2.0).cos());
    for (from, to, colors) in slices() {
        let stripes = Stripes::donut(colors);
        for (colour, list) in sector_stripes(&ring, from, to, colors).iter().enumerate() {
            for piece in list {
                assert!(piece.len() >= 3);
                assert!(
                    is_convex(piece),
                    "{from}..{to}: piece not convex: {piece:?}"
                );
                // Inside the slice.
                for &p in piece {
                    let r = radius_of(&ring, p);
                    assert!(
                        r <= ring.outer + 1e-9 && r >= ring.inner - sagitta - 1e-9,
                        "{from}..{to}: radius {r} at {p:?}"
                    );
                    let a = angle_of(&ring, p);
                    let span = to - from;
                    if span < 360.0 - 1e-9 {
                        let along = (a - from).rem_euclid(360.0);
                        assert!(
                            along <= span + 1e-6 || along >= 360.0 - 1e-6,
                            "{from}..{to}: angle {a} at {p:?}"
                        );
                    }
                }
                // Inside one band, of this colour.
                let mid = piece.iter().fold(Point::new(0.0, 0.0), |m, p| {
                    Point::new(
                        m.x + p.x / piece.len() as f64,
                        m.y + p.y / piece.len() as f64,
                    )
                });
                let band = stripes.band(mid);
                assert_eq!(
                    stripes.color_of_band(band),
                    colour,
                    "{from}..{to}: band {band}"
                );
                let (lo, hi) = (band as f64 * DONUT_STRIPE, (band + 1) as f64 * DONUT_STRIPE);
                for &p in piece {
                    let t = stripes.offset(p);
                    assert!(
                        t >= lo - 1e-9 && t <= hi + 1e-9,
                        "{from}..{to}: offset {t} outside [{lo}, {hi}]"
                    );
                }
            }
        }
    }
}

/// charts.tsx: the pattern's rects are `x = i * STRIPE` wide `STRIPE`, turned 45 degrees about the `<svg>`'s origin,
/// which is the corner of the ring's 252 px box. So at the point that lies `(k + 1/2) * 4` px across the stripes from
/// that corner, the colour is `k mod n`: checked at points of the ring all the way round.
#[test]
fn the_colour_of_a_point_is_the_1x_patterns() {
    let ring = Ring::DONUT;
    for colors in 2..=4 {
        let pieces = sector_stripes(&ring, 0.0, 360.0, colors);
        for k in 0..120 {
            // Points along the ring's middle radius, then moved to the middle of their band.
            let deg = f64::from(k) * 3.0 + 0.7;
            let p = ring.at((ring.outer + ring.inner) / 2.0, deg);
            let t = (p.x + p.y) * FRAC_1_SQRT_2;
            let band = (t / DONUT_STRIPE).floor();
            let centre_t = (band + 0.5) * DONUT_STRIPE;
            let shift = (centre_t - t) * FRAC_1_SQRT_2;
            let q = Point::new(p.x + shift, p.y + shift);
            let expected = (band as usize) % colors;
            let found: Vec<usize> = pieces
                .iter()
                .enumerate()
                .filter(|(_, list)| list.iter().any(|piece| contains(piece, q)))
                .map(|(c, _)| c)
                .collect();
            assert_eq!(
                found,
                [expected],
                "{colors} colours, {deg} deg, point {q:?}"
            );
        }
    }
}

/// The swatch's gradient (`repeating-linear-gradient(135deg, c0 0 3px, c1 3px 6px, ...)`) over the 10 px inside its
/// border: 14.1 px across, so 5 bands, colour by colour from the top-left corner.
#[test]
fn the_swatch_has_the_1x_bands() {
    for (colors, counts) in [(2, vec![3, 2]), (3, vec![2, 2, 1]), (4, vec![2, 1, 1, 1])] {
        let pieces = swatch_stripes(colors);
        let found: Vec<usize> = pieces.iter().map(Vec::len).collect();
        assert_eq!(found, counts, "{colors} colours");
        let total: f64 = pieces.iter().flatten().map(|p| area(p)).sum();
        assert!(
            (total - 100.0).abs() < 1e-9,
            "the 10 x 10 inside the border: {total}"
        );
        // The top-left corner is the first colour, the second band the second colour.
        let stripes = Stripes::swatch(colors);
        let corner = Point::new(1.2, 1.2);
        assert!(pieces[0].iter().any(|p| contains(p, corner)));
        // On the diagonal from the corner, 4.5 px across the stripes: the middle of the second band.
        let d = 1.5 * SWATCH_STRIPE * FRAC_1_SQRT_2;
        let second = Point::new(1.0 + d, 1.0 + d);
        assert_eq!(stripes.band(second), 1);
        assert!(pieces[1].iter().any(|p| contains(p, second)));
    }
}

#[test]
fn every_piece_is_one_closed_subpath() {
    let pieces = sector_stripes(&Ring::DONUT, 10.0, 46.0, 2);
    for list in &pieces {
        let path = svg_path(list);
        assert_eq!(path.matches('M').count(), list.len());
        assert_eq!(path.matches('Z').count(), list.len());
        assert!(path.is_empty() || path.starts_with('M') && path.ends_with('Z'));
    }
    assert_eq!(svg_path(&[]), "");
}

#[test]
fn an_empty_slice_has_no_stripes() {
    let ring = Ring::DONUT;
    for (from, to) in [(10.0, 10.0), (20.0, 10.0), (f64::NAN, 10.0)] {
        assert!(sector_cells(&ring, from, to).is_empty(), "{from}..{to}");
        assert!(sector_stripes(&ring, from, to, 2).iter().all(Vec::is_empty));
    }
    let whole = sector_stripes(&ring, 0.0, 360.0, 2);
    let total: f64 = whole.iter().flatten().map(|p| area(p)).sum();
    let exact = sector_area(&ring, 0.0, 360.0);
    assert!((total - exact).abs() < 0.002 * exact, "{total} vs {exact}");
    // Cells are convex, so clipping them never leaves a bridge across the hole.
    assert!(sector_cells(&ring, 0.0, 360.0).iter().all(is_convex));
    // Bands of zero width cut nothing.
    let none = stripe_pieces(
        &sector_cells(&ring, 0.0, 90.0),
        &Stripes {
            width: 0.0,
            ..Stripes::donut(2)
        },
    );
    assert!(none.iter().all(Vec::is_empty));
}
