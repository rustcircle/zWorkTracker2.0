//! What the software renderer draws, checked pixel by pixel against 1.x's geometry: the filled timer icons with their
//! stroke (V-T1), the cards' shadows (V-T9, V-D7; owner decision 6), the diagonal stripes of a combination of tags on
//! the donut (V-ST1; owner decision 1) and in its legend swatch (V-ST2), and the rail's thumb colour (V-T10).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use std::f32::consts::{FRAC_1_SQRT_2, PI};
use std::time::Duration;

use common::{Frame, Harness, RUNNING_NAME, hex, rect_of};
use i_slint_backend_testing::AccessibleRole;
use slint::platform::PointerEventButton;
use slint::{LogicalSize, Model};

/// The Tauri e2e viewport the screenshots use.
fn screen() -> LogicalSize {
    LogicalSize::new(1360.0, 900.0)
}

/// Light theme tokens (theme.slint `tokens-light`).
const SURFACE: &str = "#ffffff";
const BG: &str = "#f3f4f6";
const TEXT: &str = "#111827";
const BORDER_STRONG: &str = "#6b7280";

/// The ink of the timer icon in the button `label` of `task`: area in px squared covered by the glyph colour.
fn icon_ink(h: &Harness, frame: &Frame, task: &str, label: &str) -> f32 {
    let button = h.button(task, label);
    let icon = button
        .query_descendants()
        .match_type_name("Icon")
        .find_first()
        .unwrap();
    let (x, y, w, height) = rect_of(&icon);
    assert_eq!((w, height), (16.0, 16.0));
    // One pixel of margin: the anti-aliased edge of a stroke may reach just outside the icon's box.
    let r = (x as u32 - 1, y as u32 - 1, w as u32 + 2, height as u32 + 2);
    frame.ink(r, hex(SURFACE), hex(TEXT))
}

/// V-T1: icons.tsx draws every glyph with `stroke="currentColor" strokeWidth={2}` and round joins, the filled ones
/// too, at 16 px for a 24-unit view box. So IconPause is two 4 x 14 rects (rx 1) grown by 1 unit all round: two
/// 6 x 16 rounded rects of corner radius 2, 82.3 px squared; IconStop one 14 x 14 of radius 3, 83.7 px squared.
#[test]
fn filled_icons_are_as_big_as_1x() {
    let h = Harness::rendered(screen());
    let frame = h.snapshot();
    let unit = 16.0_f32 / 24.0;
    let rounded = |w: f32, height: f32, r: f32| (w * height - (4.0 - PI) * r * r) * unit * unit;
    let pause = 2.0 * rounded(6.0, 16.0, 2.0);
    let stop = rounded(14.0, 14.0, 3.0);
    for (label, expected) in [("Pause", pause), ("Stop", stop)] {
        let ink = icon_ink(&h, &frame, RUNNING_NAME, label);
        assert!(
            (ink - expected).abs() <= 0.08 * expected,
            "{label}: {ink:.1} px2 of ink, 1.x {expected:.1}"
        );
    }
}

/// The shadow under the card at `x`, the `rows` rows below its bottom edge `bottom`, on the rail's background.
fn below(frame: &Frame, x: u32, bottom: u32, rows: u32) -> Vec<[u8; 3]> {
    (0..rows).map(|r| frame.rgb(x, bottom + r)).collect()
}

/// V-T9, V-D7 (owner decision 6): ui.css `--shadow` (0 1px 2px text 10 %, 0 2px 8px text 6 %) under every task card
/// and Timeline card. 1.x paints the rows under a card in Light #dadbde, #e7e8ea, #edeef0, #eeeff1 over the background
/// #f3f4f6; the software renderer draws no `drop-shadow-*`, so the app draws the shadow itself.
#[test]
fn cards_cast_the_1x_shadow() {
    let h = Harness::rendered(screen());
    let frame = h.snapshot();
    let bg = hex(BG);
    // A task card: the rows under its bottom border.
    let card = h.find(AccessibleRole::Groupbox, RUNNING_NAME);
    let (x, y, w, height) = rect_of(&card);
    let rows = below(&frame, (x + w / 2.0) as u32, (y + height) as u32, 4);
    let expected = ["#dadbde", "#e7e8ea", "#edeef0", "#eeeff1"];
    for (row, (got, want)) in rows.iter().zip(expected).enumerate() {
        let want = hex(want);
        let off = (0..3).map(|i| got[i].abs_diff(want[i])).max().unwrap();
        assert!(off <= 6, "row {row} under the card: {got:?}, 1.x {want:?}");
        assert!(got[1] < bg[1], "row {row} is darker than the background");
    }
    // The sides too (blur 8 px), fainter.
    let side = frame.rgb((x + w) as u32 + 1, (y + height / 2.0) as u32);
    assert!(
        side[1] < bg[1] && side[1] >= bg[1] - 12,
        "beside the card: {side:?}"
    );

    // A Timeline card (the day's card) on the panel's background.
    let day = h.find(AccessibleRole::Groupbox, "Today, Wed 4 Nov 2026");
    let (x, y, w, height) = rect_of(&day);
    let rows = below(&frame, (x + w / 2.0) as u32, (y + height) as u32, 2);
    assert!(
        rows.iter().all(|c| c[1] < bg[1] - 4),
        "under the day card: {rows:?}"
    );

    // Dark themes: --shadow is the text colour, light there, so the edge is lighter than the background.
    zwt_app::set_theme(&h.app, "solarizedDark");
    let frame = h.snapshot();
    let card = h.find(AccessibleRole::Groupbox, RUNNING_NAME);
    let (x, y, w, height) = rect_of(&card);
    let under = frame.rgb((x + w / 2.0) as u32, (y + height) as u32);
    let dark_bg = hex("#002b36");
    assert!(
        under[1] > dark_bg[1] + 4,
        "Solarized Dark: {under:?} over {dark_bg:?}"
    );
}

/// V-ST1 (owner decision 1): charts.tsx `StripePattern`: 4 px stripes of each colour, `rotate(45)` about the corner
/// of the donut's 252 px box, cut to the slice. At points well inside a stripe and a slice, the pixel is the colour
/// of that stripe, colour `k mod n` for stripe `k`.
#[test]
fn combination_slices_have_the_1x_diagonal_stripes() {
    let mut h = Harness::rendered(screen());
    h.find(AccessibleRole::RadioButton, "Tags")
        .mock_single_click(PointerEventButton::Left);
    h.pump();
    let frame = h.snapshot();
    let rows = h.state().get_share_rows();
    let arcs = h.state().get_share_arcs();
    let donut = h.find(AccessibleRole::Image, "Share of time per tag");
    let (dx, dy, _, _) = rect_of(&donut);
    let (cx, cy, outer, inner) = (126.0_f32, 126.0_f32, 120.0_f32, 76.8_f32);
    let mut checked = 0;
    for (i, row) in rows.iter().enumerate() {
        let colors: Vec<[u8; 3]> = row
            .stripes
            .iter()
            .map(|c| [c.red(), c.green(), c.blue()])
            .collect();
        if colors.len() < 2 {
            continue;
        }
        let arc = arcs.row_data(i).unwrap();
        // Points every 0.25 degree across the slice, at 5 radii, away from its edges.
        let (from, to) = (arc.from + 2.0, arc.to - 2.0);
        let mut a = from;
        while a <= to {
            for k in 1..=5 {
                let r = inner + 4.0 + (outer - inner - 8.0) * k as f32 / 6.0;
                let t = (a - 90.0).to_radians();
                let (px, py) = (cx + r * t.cos(), cy + r * t.sin());
                let (ix, iy) = (px.floor(), py.floor());
                // The pixel's centre, across the stripes from the box's corner.
                let s = ((ix + 0.5) + (iy + 0.5)) * FRAC_1_SQRT_2;
                let band = (s / 4.0).floor();
                let edge = (s - band * 4.0).min((band + 1.0) * 4.0 - s);
                if edge < 1.2 {
                    continue;
                }
                let want = colors[(band as usize) % colors.len()];
                let got = frame.rgb((dx + ix) as u32, (dy + iy) as u32);
                let off = (0..3).map(|c| got[c].abs_diff(want[c])).max().unwrap();
                assert!(
                    off <= 10,
                    "{}: at {a} deg, radius {r}: {got:?}, the stripe {band} is {want:?}",
                    row.label
                );
                checked += 1;
            }
            a += 0.25;
        }
    }
    assert!(checked > 200, "{checked} points on combination slices");
}

/// V-ST2: charts.tsx `stripesBackground`: `repeating-linear-gradient(135deg, c0 0 3px, c1 3px 6px, ...)` inside the
/// swatch's 1 px border: bands 3 px across from the top-left corner, c0, c1, c0, c1 (two colours) or c0, c1, c2, c0
/// (three). Pixels in the middle of the first four bands.
#[test]
fn combination_swatches_have_the_1x_stripes() {
    let mut h = Harness::rendered(screen());
    h.find(AccessibleRole::RadioButton, "Tags")
        .mock_single_click(PointerEventButton::Left);
    h.pump();
    let frame = h.snapshot();
    let rows = h.state().get_share_rows();
    let swatches = h.of_type("Swatch");
    assert_eq!(swatches.len(), rows.row_count());
    let mut striped = 0;
    for (row, swatch) in rows.iter().zip(&swatches) {
        let colors: Vec<[u8; 3]> = row
            .stripes
            .iter()
            .map(|c| [c.red(), c.green(), c.blue()])
            .collect();
        if colors.len() < 2 {
            continue;
        }
        striped += 1;
        let (sx, sy, w, height) = rect_of(swatch);
        assert_eq!((w, height), (12.0, 12.0));
        // Whole pixels inside one band each (across the stripes 1.4-2.8, 3.5-5.0, 7.1-8.5 and 9.9-11.3 px from the
        // corner: bands 0, 1, 2, 3), clear of the rounded corners.
        for (band, (px, py)) in [(2, 2), (3, 4), (6, 6), (8, 8)].into_iter().enumerate() {
            let want = colors[band % colors.len()];
            let got = frame.rgb(sx as u32 + px, sy as u32 + py);
            let off = (0..3).map(|c| got[c].abs_diff(want[c])).max().unwrap();
            assert!(
                off <= 10,
                "{}: band {band} at ({px}, {py}): {got:?}, want {want:?}",
                row.label
            );
        }
    }
    assert!(striped >= 1, "the sample has combinations");
}

/// V-T10, V-D8 (owner decision 5): the thumb is drawn in the theme's border-strong; at rest nothing is drawn at the
/// rail's right edge but the background.
#[test]
fn the_rail_thumb_is_drawn_in_a_theme_colour() {
    let h = Harness::rendered(screen()).relaunch_at(screen());
    let rail = h.find(AccessibleRole::Groupbox, "Tasks");
    let (rx, ry, rw, rh) = rect_of(&rail);
    // The strip between the cards and the rail's border, below the title.
    let strip_x = (rx + rw) as u32 - 6;
    let rest = h.snapshot();
    for y in (ry as u32 + 120..(ry + rh) as u32 - 10).step_by(7) {
        let c = rest.rgb(strip_x, y);
        assert_ne!(c, hex("#858587"), "no std-widgets thumb at {y}");
        assert_ne!(c, hex(BORDER_STRONG), "no thumb at rest at {y}");
    }
    h.find(AccessibleRole::Groupbox, "Logo concepts")
        .scroll(0.0, -60.0);
    i_slint_backend_testing::mock_elapsed_time(Duration::ZERO);
    let thumb = h.of_type("RailThumb");
    assert_eq!(thumb.len(), 1);
    let (tx, ty, tw, th) = rect_of(&thumb[0]);
    let frame = h.snapshot();
    let centre = frame.rgb((tx + tw / 2.0) as u32, (ty + th / 2.0) as u32);
    assert_eq!(
        centre,
        hex(BORDER_STRONG),
        "the thumb in Theme.border-strong"
    );
}
