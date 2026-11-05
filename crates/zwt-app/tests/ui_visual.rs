//! The 1.x sizes of the Timeline's parts (docs/visual/REVIEW.md): chips and badges (V-T2), the boxed pause note
//! (V-T3), the timer buttons (V-T4), the tabs (V-T5), the Share legend's rows (V-SP1, V-ST3) and width (V-SP5), the
//! Share card around its legend at every window width (fix loop V2), and the line height of wrapped text (V-T13,
//! V-A5). Sizes follow the 1.x CSS: line-height times font size, plus padding and border, painted on whole pixels as a
//! browser paints boxes.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a failed lookup is a failed test"
)]

mod common;

use common::{DRAFT, Harness, encloses, rect_of};
use i_slint_backend_testing::{AccessibleRole, ElementHandle};
use slint::platform::PointerEventButton;
use slint::{ComponentHandle, Model};

/// base.css `body { line-height: 1.45 }`.
const BODY_LINE_HEIGHT: f32 = 1.45;

fn heights(elements: &[ElementHandle]) -> Vec<f32> {
    elements.iter().map(|e| e.size().height).collect()
}

/// V-T2: ui.css `.chip` is 12.5 px x 1.5 + 2 x 2 px = 22.75 px, `.badge` 12 px x 1.45 + 2 x (1 + 1) px = 21.4 px,
/// tasksP2.css `.tag-chip` 12 px x 1.5 + 2 x 1 px = 20 px; on whole pixels, as 1.x paints them.
#[test]
fn chips_and_badges_are_as_tall_as_in_1x() {
    let h = Harness::new();
    let chips = h.of_type("ProjectChip");
    assert!(!chips.is_empty());
    assert!(
        heights(&chips).iter().all(|&v| v == 23.0),
        "project chips {:?}",
        heights(&chips)
    );
    let tags = h.of_type("TagChip");
    assert!(!tags.is_empty());
    assert!(
        heights(&tags).iter().all(|&v| v == 20.0),
        "tag chips {:?}",
        heights(&tags)
    );
    let badges = h.of_type("Badge");
    assert!(!badges.is_empty());
    assert!(
        heights(&badges).iter().all(|&v| v == 21.0),
        "badges {:?}",
        heights(&badges)
    );
}

/// V-T3: tasks.css `.task-note-pause`: one line of 13 px x 1.45 (18.85 px), 6 px padding and a 1 px border above
/// and below: 33 px on whole pixels (1.x's screenshot: 32 rows, its line box rounded down).
#[test]
fn a_one_line_pause_note_is_as_tall_as_in_1x() {
    let h = Harness::new();
    let texts = h.find_all(AccessibleRole::Text, "Paused by you.");
    assert!(!texts.is_empty());
    let notes = h.of_type("Note");
    for text in &texts {
        let note = notes
            .iter()
            .find(|n| encloses(n, text))
            .expect("the note around the text");
        assert_eq!(
            note.size().height,
            (13.0 * BODY_LINE_HEIGHT).ceil() + 2.0 * 6.0 + 2.0,
            "note at {:?}",
            rect_of(note)
        );
    }
}

/// V-T4: ui.css `.btn-small { min-height: 28px }`, and the buttons sit on whole pixels, so their 1 px borders are
/// sharp (the review's "26 px" was measured inside a rounded corner).
#[test]
fn the_timer_buttons_are_28_px_tall_on_whole_pixels() {
    let h = Harness::new();
    let buttons = h.of_type("TimerButton");
    assert_eq!(buttons.len(), 4 * h.tasks().len());
    for b in &buttons {
        let (_, y, _, height) = rect_of(b);
        assert_eq!(height, 28.0);
        assert_eq!(y.fract(), 0.0, "button at y {y}");
    }
}

/// V-T5: shell.css `.tab { padding: 8px 16px 10px }` around a line box of 14 px x 1.45, the label centred in it; the
/// underline 12 px in from each side, 3 px tall, 1 px over the bar's bottom border (`bottom: -1px`); the bar 10 px of
/// padding above the tabs and its 1 px border below.
#[test]
fn the_tabs_have_the_1x_geometry() {
    let h = Harness::new();
    let tab = h.find(AccessibleRole::Tab, "Timeline");
    let (tx, ty, tw, th) = rect_of(&tab);
    assert_eq!(th, 38.0, "8 + 20.3 + 10 px, on whole pixels");
    let label = tab
        .query_descendants()
        .match_type_name("Text")
        .find_first()
        .unwrap();
    let (_, ly, _, lh) = rect_of(&label);
    assert_eq!(ly - ty, 8.0, "padding-top");
    assert!((lh - 14.0 * BODY_LINE_HEIGHT).abs() < 0.01, "line box {lh}");
    let underline = tab
        .query_descendants()
        .match_type_name("TabUnderline")
        .find_first()
        .expect("the picked tab's underline");
    let (ux, uy, uw, uh) = rect_of(&underline);
    assert_eq!((ux - tx, uw, uh), (12.0, tw - 24.0, 3.0));
    assert_eq!(
        uy + uh,
        ty + th + 1.0,
        "1 px below the tab, over the bar's border"
    );
    let bar = h.find(AccessibleRole::TabList, "Sections");
    let (_, by, _, bh) = rect_of(&bar);
    assert_eq!(ty - by, 10.0);
    assert_eq!(bh, 10.0 + th + 1.0);
}

/// V-SP1, V-ST3: analysis.css `.a-share-legend li`: 13 px x 1.45 and 4 px padding above and below (26.85 px), 1 px
/// apart: a row every 28 px, as in 1.x's screenshot.
#[test]
fn share_legend_rows_are_28_px_apart() {
    let mut h = Harness::new();
    for level in ["Projects", "Tags"] {
        h.find(AccessibleRole::RadioButton, level)
            .mock_single_click(PointerEventButton::Left);
        h.pump();
        let rows = h.of_type("LegendRow");
        assert!(rows.len() >= 3, "{level}: {} rows", rows.len());
        for pair in rows.windows(2) {
            let pitch = pair[1].absolute_position().y - pair[0].absolute_position().y;
            assert_eq!(pitch, 28.0, "{level}");
        }
        assert!(
            heights(&rows).iter().all(|&v| v == 27.0),
            "{level}: {:?}",
            heights(&rows)
        );
    }
}

/// V-SP5 (owner decision 8): on the wide Timeline card the legend beside the donut is no wider than 1.x's (the 420 px
/// Analysis card less its 16 px padding each side: 388 px), so the figures stay near the names.
#[test]
fn the_share_legend_stays_as_wide_as_in_1x() {
    let h = Harness::new();
    let legend = h.find(AccessibleRole::List, "Share of time legend, projects");
    let donut = h.find(AccessibleRole::Image, "Share of time per project");
    let (lx, _, lw, _) = rect_of(&legend);
    let (dx, _, dw, _) = rect_of(&donut);
    assert!(lw <= 388.0, "legend {lw} px wide");
    assert!(lw >= 280.0, "and at least its 1.x basis");
    assert!(lx >= dx + dw, "beside the donut");
}

/// On a narrow window the legend wraps below the donut (analysis.css `.a-share` wraps): the donut centred in its line
/// (`margin-inline: auto`), the legend as wide as the card's content, not capped.
#[test]
fn on_a_narrow_card_the_legend_wraps_below_the_donut() {
    i_slint_backend_testing::init_no_event_loop();
    let h = Harness::open(slint::LogicalSize::new(960.0, 3600.0));
    let card = h.find(AccessibleRole::Groupbox, "Share of time");
    let legend = h.find(AccessibleRole::List, "Share of time legend, projects");
    let donut = h.find(AccessibleRole::Image, "Share of time per project");
    let (cx, _, cw, _) = rect_of(&card);
    let (lx, ly, lw, _) = rect_of(&legend);
    let (dx, dy, dw, dh) = rect_of(&donut);
    assert!(ly >= dy + dh, "the legend is below the donut");
    assert_eq!(lx, cx + 16.0);
    assert_eq!(lw, cw - 32.0, "the legend takes the card's content width");
    let free = cw - 32.0 - dw;
    assert!(
        ((dx - cx - 16.0) - free / 2.0).abs() <= 1.0,
        "the donut is centred"
    );
}

/// The card's padding below its content (share.slint `ShareCard`) and the Timeline panel's below the last card
/// (timeline.slint `TimelinePanel`).
const CARD_PADDING_BOTTOM: f32 = 14.0;
const PANEL_PADDING_BOTTOM: f32 = 40.0;

/// The window resized to `width` x `height`, laid out as the event loop would before its next frame.
fn resize(h: &Harness, width: f32, height: f32) {
    h.app
        .window()
        .set_size(slint::LogicalSize::new(width, height));
    h.settle();
}

/// The Share of time card on screen: the card, its donut, its legend, and what the legend draws (its rows, then the
/// footnote or note under them, if any).
struct ShareCard {
    card: ElementHandle,
    donut: ElementHandle,
    legend: ElementHandle,
    lines: Vec<ElementHandle>,
}

fn share_card(h: &Harness) -> ShareCard {
    let card = h.find(AccessibleRole::Groupbox, "Share of time");
    let donut = card
        .query_descendants()
        .match_accessible_role(AccessibleRole::Image)
        .match_predicate(|e| {
            e.accessible_label()
                .is_some_and(|l| l.starts_with("Share of time per "))
        })
        .find_first()
        .expect("the donut");
    let legend = card
        .query_descendants()
        .match_accessible_role(AccessibleRole::List)
        .find_first()
        .expect("the legend");
    let mut lines = legend
        .query_descendants()
        .match_accessible_role(AccessibleRole::ListItem)
        .find_all();
    lines.extend(
        legend
            .query_descendants()
            .match_type_name("Para")
            .find_all(),
    );
    // The queries skip what is clipped away (out of the window, or of the panel's view): a row drawn there would not be
    // checked at all, so every row and note of the legend must be found.
    let state = h.state();
    let notes = [state.get_share_footnote(), state.get_share_note()]
        .iter()
        .filter(|n| !n.is_empty())
        .count();
    assert_eq!(
        lines.len(),
        state.get_share_rows().row_count() + notes,
        "every row and note of the legend is in view: {:?}",
        lines.iter().map(rect_of).collect::<Vec<_>>()
    );
    ShareCard {
        card,
        donut,
        legend,
        lines,
    }
}

/// What of the legend lies outside the card (`at` names the level and the width): a row, or the note under the rows,
/// beyond the card's border, the legend past the card's content on the right, or the card ending less than its
/// padding below the legend.
fn outside_the_card(share: &ShareCard, at: &str) -> Vec<String> {
    let (cx, cy, cw, ch) = rect_of(&share.card);
    let mut faults = Vec::new();
    for line in &share.lines {
        if !encloses(&share.card, line) {
            faults.push(format!(
                "{at}: {:?} at {:?} is outside the card at {:?}",
                line.accessible_label().unwrap_or_default(),
                rect_of(line),
                rect_of(&share.card)
            ));
        }
    }
    let (lx, ly, lw, lh) = rect_of(&share.legend);
    if lx + lw > cx + cw - 16.0 {
        faults.push(format!(
            "{at}: the legend ends at x {}, past the card's content (x {})",
            lx + lw,
            cx + cw - 16.0
        ));
    }
    let end = share
        .lines
        .iter()
        .map(|e| {
            let (_, y, _, height) = rect_of(e);
            y + height
        })
        .fold(ly + lh, f32::max);
    if cy + ch < end + CARD_PADDING_BOTTOM {
        faults.push(format!(
            "{at}: the card ends at y {}, the legend at y {end}: less than the card's {CARD_PADDING_BOTTOM} px of \
             padding below it",
            cy + ch
        ));
    }
    faults
}

/// The Share card shows `level` (Projects, Tasks, Subtasks, Tags).
fn show_level(h: &mut Harness, level: &str) {
    h.find(AccessibleRole::RadioButton, level)
        .mock_single_click(PointerEventButton::Left);
    h.pump();
}

/// Fix loop V2 (the 960 x 900 screenshot): where the legend wraps below the donut, from the narrowest window (app.slint
/// `min-width`) to the last width before both fit side by side (the card's content 560 px wide), the card grows to hold
/// it: every row and the Tags level's footnote lie inside the card, which ends its padding below them, and shrinks
/// back with a shorter legend. Before, the card was as tall as with the legend beside the donut, and the legend was
/// drawn below its border.
#[test]
fn a_wrapped_legend_lies_inside_the_card() {
    i_slint_backend_testing::init_no_event_loop();
    let mut h = Harness::open(slint::LogicalSize::new(960.0, 900.0));
    let mut faults = Vec::new();
    // Projects: three rows; Tags: five rows and the footnote under them, the tallest legend of the sample; then
    // Projects again, where the card is shorter again.
    for level in ["Projects", "Tags", "Projects"] {
        show_level(&mut h, level);
        for width in [960.0, 1000.0, 1059.0] {
            resize(&h, width, 900.0);
            let share = share_card(&h);
            let at = format!("{level} at {width} px");
            let (_, dy, _, dh) = rect_of(&share.donut);
            let (_, ly, _, lh) = rect_of(&share.legend);
            assert!(ly >= dy + dh, "{at}: the legend wraps below the donut");
            faults.extend(outside_the_card(&share, &at));
            let (_, cy, _, ch) = rect_of(&share.card);
            if cy + ch > ly + lh + CARD_PADDING_BOTTOM + 0.5 {
                faults.push(format!(
                    "{at}: the card ends at y {}, more than its {CARD_PADDING_BOTTOM} px of padding below the \
                     legend (y {})",
                    cy + ch,
                    ly + lh
                ));
            }
        }
    }
    assert!(faults.is_empty(), "{}", faults.join("\n"));
}

/// Where the donut and the legend fit side by side (the card's content at least 252 + 28 + 280 px wide, share.slint
/// `side-by-side`) the legend is beside the donut, as wide as is left of the content up to 388 px (owner decision 8),
/// and the donut sets the card's height. Before, from 1,060 to 1,167 px the legend wrapped below the donut all the
/// same (it was laid out at 388 px). At 1,360 px this is the layout of the screenshots, unchanged.
#[test]
fn where_both_fit_the_legend_is_beside_the_donut() {
    i_slint_backend_testing::init_no_event_loop();
    let mut h = Harness::open(slint::LogicalSize::new(1360.0, 900.0));
    let mut faults = Vec::new();
    for level in ["Projects", "Tags"] {
        show_level(&mut h, level);
        for width in [1060.0, 1100.0, 1167.0, 1168.0, 1360.0] {
            resize(&h, width, 900.0);
            let share = share_card(&h);
            let at = format!("{level} at {width} px");
            let (cx, cy, cw, ch) = rect_of(&share.card);
            let (dx, dy, dw, dh) = rect_of(&share.donut);
            let (lx, ly, lw, lh) = rect_of(&share.legend);
            if (dx, lx) != (cx + 16.0, dx + dw + 28.0) || ly < dy || ly + lh > dy + dh {
                faults.push(format!(
                    "{at}: the legend at {:?} is not beside the donut at {:?}",
                    rect_of(&share.legend),
                    rect_of(&share.donut)
                ));
            }
            let room = (cw - 32.0 - dw - 28.0).min(388.0);
            if lw != room {
                faults.push(format!("{at}: the legend is {lw} px wide, not {room}"));
            }
            if cy + ch != dy + dh + CARD_PADDING_BOTTOM {
                faults.push(format!(
                    "{at}: the card ends at y {}, not {CARD_PADDING_BOTTOM} px below the donut (y {})",
                    cy + ch,
                    dy + dh
                ));
            }
            faults.extend(outside_the_card(&share, &at));
        }
    }
    assert!(faults.is_empty(), "{}", faults.join("\n"));
}

/// An empty period in the narrowest window: the card's message wraps to two lines, and its box and the card grow with
/// it. Before, both were as tall as for one line, and the box was drawn past the card's border.
#[test]
fn an_empty_share_card_holds_its_wrapped_message() {
    i_slint_backend_testing::init_no_event_loop();
    let h = Harness::open_empty(slint::LogicalSize::new(960.0, 900.0));
    let card = h.find(AccessibleRole::Groupbox, "Share of time");
    let text = h.find(
        AccessibleRole::Text,
        "No time logged in this period. Try another range or clear the filters.",
    );
    assert_lines(&text, 14.0, 2.0);
    // The message's box: the smallest rectangle of the card around the text.
    let area = |e: &ElementHandle| e.size().width * e.size().height;
    let message = card
        .query_descendants()
        .match_type_name("Rectangle")
        .find_all()
        .into_iter()
        .filter(|r| encloses(r, &text))
        .min_by(|a, b| area(a).total_cmp(&area(b)))
        .expect("the message's box");
    let (_, my, _, mh) = rect_of(&message);
    let (_, cy, _, ch) = rect_of(&card);
    assert!(
        encloses(&card, &message) && cy + ch >= my + mh + CARD_PADDING_BOTTOM,
        "the box at {:?} around the text at {:?}, in the card at {:?}",
        rect_of(&message),
        rect_of(&text),
        rect_of(&card)
    );
}

/// The Timeline panel scrolls to the end of the grown card: in a 960 x 600 window (app.slint `min-height`) the wrapped
/// legend is below the fold, and scrolled to the end every row is in view, the card's bottom the panel's padding above
/// the panel's. Before, the panel stopped scrolling where the card's short border ended, with the last row out of
/// view.
#[test]
fn the_timeline_scrolls_to_the_end_of_a_wrapped_legend() {
    i_slint_backend_testing::init_no_event_loop();
    let h = Harness::open(slint::LogicalSize::new(960.0, 600.0));
    let panel = h
        .of_type("TimelinePanel")
        .into_iter()
        .next()
        .expect("the Timeline panel");
    // A wheel turned far down: the panel stops at its end.
    panel.scroll(0.0, -10_000.0);
    h.settle();
    // Elements out of view are not found: every row found is in view.
    let rows = h.of_type("LegendRow");
    assert_eq!(
        rows.len(),
        h.state().get_share_rows().row_count(),
        "every row in view, scrolled to the end: {:?} in the panel at {:?}",
        rows.iter().map(rect_of).collect::<Vec<_>>(),
        rect_of(&panel)
    );
    for row in &rows {
        assert!(
            encloses(&panel, row),
            "the row at {:?} is in the panel at {:?}",
            rect_of(row),
            rect_of(&panel)
        );
    }
    let (_, cy, _, ch) = rect_of(&h.find(AccessibleRole::Groupbox, "Share of time"));
    let (_, py, _, ph) = rect_of(&panel);
    assert_eq!(cy + ch, py + ph - PANEL_PADDING_BOTTOM);
}

/// The text drawn at `font_size` with the 1.x line height is `lines` lines of `font_size` x 1.45 px.
fn assert_lines(element: &ElementHandle, font_size: f32, at_least: f32) {
    let pitch = font_size * BODY_LINE_HEIGHT;
    let height = element.size().height;
    let lines = (height / pitch).round();
    assert!(lines >= at_least, "{lines} lines in {height} px");
    assert!(
        (height - (lines * pitch).ceil()).abs() <= 1.0,
        "{height} px for {lines} lines of {pitch} px ({:?})",
        element.accessible_label()
    );
}

/// V-T13, V-D9, V-A5 (owner decision 4): wrapped text keeps 1.x's line height, 1.45 times its size: a two-line note
/// in the rail (13 px), the About paragraphs (14 px).
#[test]
fn wrapped_text_has_the_1x_line_height() {
    let h = Harness::new();
    let note = h.find(
        AccessibleRole::Text,
        "Paused when the app was closed. Time stopped at that moment.",
    );
    assert_lines(&note, 13.0, 2.0);

    h.find(AccessibleRole::Tab, "About")
        .mock_single_click(PointerEventButton::Left);
    for text in [
        "Time zone rules ship inside the app. When governments change daylight saving rules, the fix arrives with an app update, so keep the app up to date for correct hours on future dates. Past dates keep the offsets they had.",
        "This app collects no telemetry and makes no network requests of its own. Time you log stays on this computer unless you turn on sync.",
    ] {
        assert_lines(&h.find(AccessibleRole::Text, text), 14.0, 1.0);
    }
}

/// The notice (ui.css `.toast`, drawn over the window, out of any layout) is as tall as its wrapped text and padding,
/// its text at the body's line height; its shadow (`--shadow-lg`) is drawn around it, not as part of its size.
#[test]
fn the_notice_is_as_tall_as_its_text() {
    let mut h = Harness::new();
    h.state().invoke_start_task(DRAFT.into());
    h.pump();
    let text = h.state().get_notice_text().to_string();
    let found = h.find_all(AccessibleRole::Text, &text);
    assert_eq!(found.len(), 2, "the notice and its paragraph");
    let (notice, para) = if found[0].size().height >= found[1].size().height {
        (&found[0], &found[1])
    } else {
        (&found[1], &found[0])
    };
    assert_lines(para, 14.0, 2.0);
    let (_, ny, nw, nh) = rect_of(notice);
    assert_eq!(nw, 440.0);
    assert_eq!(
        nh,
        para.size().height + 2.0 * 12.0,
        "12 px of padding above and below"
    );
    let window = h.app.window().size().height as f32;
    assert_eq!(ny + nh, window - 20.0, "20 px above the window's bottom");
}
