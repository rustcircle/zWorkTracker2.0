//! A client-ready timesheet PDF, generated locally. Pure Rust, no network. Text is printed with a
//! bundled font cut down to this document, so it is selectable and copyable, and nothing about the
//! file depends on the OS. What the font lacks prints as an empty box and is reported: see `pdffont`.

use std::collections::BTreeMap;

use chrono_tz::Tz;
use pdf_writer::{Content, Filter, Finish, Name, Pdf, Rect, Ref, Str};
use serde::{Deserialize, Serialize};

use crate::billing::{ROUNDED_ROWS_NOTE, money, rows_differ_from_total, total_cents};
use crate::error::CoreError;
use crate::pdffont::{Fonts, Weight};
use crate::report::{ExportSource, hms};
use crate::validate::parse_zone;

const WIDTH: f32 = 595.28;
const HEIGHT: f32 = 841.89;
const MARGIN: f32 = 48.0;
const ROW: f32 = 16.0;
const MAX_LOGO_PIXELS: u32 = 4096;
pub const MAX_LOGO_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfRequest {
    #[serde(default)]
    pub project_id: Option<String>,
    /// Inclusive days, `YYYY-MM-DD`, in `zone`.
    pub from: String,
    pub to: String,
    /// The client's zone, for days and totals.
    pub zone: String,
    #[serde(default)]
    pub title: String,
    pub include_prices: bool,
    #[serde(default)]
    pub use_logo: bool,
    /// Only these Timeline entries. Their days are in `zone`.
    #[serde(default)]
    pub selection: Option<Vec<crate::report::SelectedEntry>>,
}

/// A decoded logo ready for embedding.
#[derive(Clone, Debug)]
pub struct Logo {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

/// Checks and decodes a PNG. Transparent parts are flattened onto white.
pub fn decode_logo(bytes: &[u8]) -> Result<Logo, CoreError> {
    let bad = || CoreError::Validation("the logo must be a PNG image".into());
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|_| bad())?;
    let (w, h) = {
        let info = reader.info();
        (info.width, info.height)
    };
    if w == 0 || h == 0 || w > MAX_LOGO_PIXELS || h > MAX_LOGO_PIXELS {
        return Err(CoreError::Validation(format!(
            "the logo must be at most {MAX_LOGO_PIXELS} pixels wide and tall"
        )));
    }
    let size = reader.output_buffer_size().ok_or_else(bad)?;
    let mut buf = vec![0; size];
    let frame = reader.next_frame(&mut buf).map_err(|_| bad())?;
    let channels = frame.color_type.samples();
    let mut rgb = Vec::with_capacity(w as usize * h as usize * 3);
    for px in buf
        .get(..frame.buffer_size())
        .ok_or_else(bad)?
        .chunks_exact(channels)
    {
        let (r, g, b, a) = match *px {
            [v] => (v, v, v, 255),
            [v, a] => (v, v, v, a),
            [r, g, b] => (r, g, b, 255),
            [r, g, b, a] => (r, g, b, a),
            _ => return Err(bad()),
        };
        let over = |c: u8| {
            u8::try_from((u16::from(c) * u16::from(a) + 255 * (255 - u16::from(a))) / 255)
                .unwrap_or(255)
        };
        rgb.extend([over(r), over(g), over(b)]);
    }
    Ok(Logo {
        width: w,
        height: h,
        rgb,
    })
}

struct Writer {
    pages: Vec<Content>,
    y: f32,
    fonts: Fonts,
    /// What was printed, as typed, so tests read the document's text the way a reader would.
    #[cfg(test)]
    shown: Vec<String>,
}

impl Writer {
    fn new() -> Result<Self, CoreError> {
        Ok(Self {
            pages: vec![Content::new()],
            y: HEIGHT - MARGIN,
            fonts: Fonts::new()?,
            #[cfg(test)]
            shown: Vec::new(),
        })
    }
    #[expect(
        clippy::expect_used,
        reason = "the list starts with a page and only grows"
    )]
    fn page(&mut self) -> &mut Content {
        self.pages
            .last_mut()
            .expect("invariant: starts with a page")
    }
    fn text(&mut self, x: f32, y: f32, weight: Weight, size: f32, text: &str) {
        #[cfg(test)]
        self.shown.push(text.to_owned());
        let bytes = self.fonts.encode(weight, text);
        let c = self.page();
        show(c, x, y, weight, size, &bytes);
    }
    fn fit(&self, weight: Weight, text: &str, width: f32, size: f32) -> String {
        self.fonts.fit(weight, text, width, size)
    }
    fn line(&mut self, y: f32) {
        let c = self.page();
        c.set_line_width(0.5);
        c.move_to(MARGIN, y);
        c.line_to(WIDTH - MARGIN, y);
        c.stroke();
    }
    fn ensure(&mut self, needed: f32) -> bool {
        if self.y - needed < MARGIN + ROW {
            self.pages.push(Content::new());
            self.y = HEIGHT - MARGIN;
            return true;
        }
        false
    }
}

fn show(c: &mut Content, x: f32, y: f32, weight: Weight, size: f32, encoded: &[u8]) {
    let font: &[u8] = match weight {
        Weight::Regular => b"Reg",
        Weight::Bold => b"Bold",
    };
    c.begin_text();
    c.set_font(Name(font), size);
    c.next_line(x, y);
    c.show(Str(encoded));
    c.end_text();
}

struct Row {
    day: String,
    project: String,
    task: String,
    hours: String,
    amount: String,
}

/// A finished timesheet, and the characters in it that the font could not draw.
#[derive(Clone, Debug)]
pub struct Timesheet {
    pub bytes: Vec<u8>,
    pub unprintable: Vec<char>,
}

/// What the export tells the UI after saving.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PdfSaved {
    /// `None` when the user cancelled the save.
    pub path: Option<String>,
    /// Each printed as an empty box. They still copy as typed.
    pub unprintable: Vec<String>,
}

/// The line printed on a timesheet with characters the font lacks, and shown at export.
#[must_use]
pub fn unprintable_note(chars: &[char]) -> Option<String> {
    const SHOWN: usize = 5;
    if chars.is_empty() {
        return None;
    }
    let mut codes: Vec<String> = chars
        .iter()
        .take(SHOWN)
        .map(|c| format!("U+{:04X}", u32::from(*c)))
        .collect();
    if chars.len() > SHOWN {
        codes.push(format!("and {} more", chars.len() - SHOWN));
    }
    let count = chars.len();
    let (noun, verb) = if count == 1 {
        ("character", "prints")
    } else {
        ("characters", "print")
    };
    Some(format!(
        "{count} {noun} {verb} as an empty box, as this font has no glyph: {}.",
        codes.join(", ")
    ))
}

/// Builds the timesheet. With `include_prices` off, no price appears anywhere in the file.
pub fn build(
    source: &ExportSource,
    request: &PdfRequest,
    logo: Option<&Logo>,
) -> Result<Timesheet, CoreError> {
    Ok(render(source, request, logo)?.0)
}

/// The timesheet, with its printed text in tests.
fn render(
    source: &ExportSource,
    request: &PdfRequest,
    logo: Option<&Logo>,
) -> Result<(Timesheet, Writer), CoreError> {
    let tz: Tz = parse_zone(&request.zone)?;
    let narrowed;
    let source = match &request.selection {
        Some(entries) => {
            narrowed = source.narrowed(tz, entries)?;
            &narrowed
        }
        None => source,
    };
    crate::validate::check_date(&request.from, "the start date")?;
    crate::validate::check_date(&request.to, "the end date")?;
    let mut task_days = Vec::new();
    let mut total_secs = 0i64;
    let mut pause_secs = 0i64;
    let mut rows = Vec::new();
    let mut amounts: Vec<BTreeMap<String, i64>> = Vec::new();
    for r in source.rows(tz) {
        let day = r.row.day.format("%Y-%m-%d").to_string();
        if day < request.from || day > request.to {
            continue;
        }
        if request
            .project_id
            .as_ref()
            .is_some_and(|p| *p != r.row.project_id)
        {
            continue;
        }
        total_secs += r.row.billable_secs;
        pause_secs += r.row.pause_secs;
        let cents = total_cents([&r.row]);
        let amount: Vec<String> = cents.iter().map(|(cur, c)| money(cur, *c)).collect();
        rows.push(Row {
            day,
            project: r.project_name,
            task: r.task_name,
            hours: hms(r.row.billable_secs),
            amount: amount.join(" + "),
        });
        amounts.push(cents);
        task_days.push(r.row);
    }
    let totals = total_cents(&task_days);

    let mut w = Writer::new()?;
    let title = if request.title.trim().is_empty() {
        "Timesheet"
    } else {
        request.title.trim()
    };
    let mut top = HEIGHT - MARGIN;
    if logo.is_some() {
        top -= 60.0;
    }
    let fitted = w.fit(Weight::Bold, title, WIDTH - 2.0 * MARGIN, 18.0);
    w.text(MARGIN, top - 18.0, Weight::Bold, 18.0, &fitted);
    w.text(
        MARGIN,
        top - 36.0,
        Weight::Regular,
        10.0,
        &format!(
            "Period {} to {}. Days and times in {}.",
            request.from, request.to, request.zone
        ),
    );
    if let Some(count) = source.selected_count() {
        w.text(
            MARGIN,
            top - 50.0,
            Weight::Regular,
            10.0,
            &format!("Selection of {count} entries. Totals cover those only."),
        );
    }
    w.y = top - 66.0;

    let (col_day, col_project, col_task, col_hours, col_amount) = (
        MARGIN,
        MARGIN + 66.0,
        MARGIN + 170.0,
        MARGIN + 340.0,
        MARGIN + 400.0,
    );
    let header = |w: &mut Writer, prices: bool| {
        let y = w.y;
        w.text(col_day, y, Weight::Bold, 9.0, "Day");
        w.text(col_project, y, Weight::Bold, 9.0, "Project");
        w.text(col_task, y, Weight::Bold, 9.0, "Task");
        w.text(col_hours, y, Weight::Bold, 9.0, "Time");
        if prices {
            w.text(col_amount, y, Weight::Bold, 9.0, "Amount");
        }
        w.line(y - 4.0);
        w.y -= ROW + 2.0;
    };
    header(&mut w, request.include_prices);
    for r in &rows {
        if w.ensure(ROW) {
            header(&mut w, request.include_prices);
        }
        let y = w.y;
        w.text(col_day, y, Weight::Regular, 9.0, &r.day);
        let project = w.fit(Weight::Regular, &r.project, 98.0, 9.0);
        w.text(col_project, y, Weight::Regular, 9.0, &project);
        let task = w.fit(Weight::Regular, &r.task, 164.0, 9.0);
        w.text(col_task, y, Weight::Regular, 9.0, &task);
        w.text(col_hours, y, Weight::Regular, 9.0, &r.hours);
        if request.include_prices {
            let amount = w.fit(Weight::Regular, &r.amount, 100.0, 9.0);
            w.text(col_amount, y, Weight::Regular, 9.0, &amount);
        }
        w.y -= ROW;
    }
    if rows.is_empty() {
        let y = w.y;
        w.text(MARGIN, y, Weight::Regular, 10.0, "No time in this period.");
        w.y -= ROW;
    }
    w.ensure(ROW * 4.0);
    let y = w.y;
    w.line(y + 8.0);
    w.text(
        MARGIN,
        y - 6.0,
        Weight::Bold,
        10.0,
        &format!("Total time: {}", hms(total_secs)),
    );
    w.y -= ROW + 6.0;
    if pause_secs > 0 {
        let y = w.y;
        w.text(
            MARGIN,
            y,
            Weight::Regular,
            9.0,
            &format!("Includes pauses billed as work: {}", hms(pause_secs)),
        );
        w.y -= ROW;
    }
    if request.include_prices && !totals.is_empty() {
        let text: Vec<String> = totals.iter().map(|(c, v)| money(c, *v)).collect();
        let y = w.y;
        w.text(
            MARGIN,
            y,
            Weight::Bold,
            10.0,
            &format!("Total: {}", text.join(" + ")),
        );
        w.y -= ROW;
        if rows_differ_from_total(&amounts, &totals) {
            let y = w.y;
            w.text(MARGIN, y, Weight::Regular, 8.0, ROUNDED_ROWS_NOTE);
            w.y -= ROW;
        }
    }

    // Every name has been printed by now, so this says all there is to say.
    let unprintable = w.fonts.missing();
    if let Some(note) = unprintable_note(&unprintable) {
        w.ensure(ROW * 2.0);
        let y = w.y;
        let note = w.fit(Weight::Regular, &note, WIDTH - 2.0 * MARGIN, 8.0);
        w.text(MARGIN, y, Weight::Regular, 8.0, &note);
        w.y -= ROW - 4.0;
        let y = w.y;
        w.text(
            MARGIN,
            y,
            Weight::Regular,
            8.0,
            "Copying the text from this file gives them as typed, and the Excel and CSV exports show them.",
        );
        w.y -= ROW;
    }

    let bytes = assemble(&mut w, logo)?;
    Ok((Timesheet { bytes, unprintable }, w))
}

fn assemble(w: &mut Writer, logo: Option<&Logo>) -> Result<Vec<u8>, CoreError> {
    let pages = std::mem::take(&mut w.pages);
    let mut pdf = Pdf::new();
    let catalog = Ref::new(1);
    let tree = Ref::new(2);
    let regular = Ref::new(3);
    let bold = Ref::new(4);
    let image = Ref::new(5);
    let mut next = 6;
    let mut alloc = || {
        let r = Ref::new(next);
        next += 1;
        r
    };
    let ids: Vec<(Ref, Ref)> = pages.iter().map(|_| (alloc(), alloc())).collect();

    pdf.catalog(catalog).pages(tree);
    pdf.pages(tree)
        .kids(ids.iter().map(|p| p.0))
        .count(i32::try_from(ids.len()).unwrap_or(i32::MAX));
    if let Some(l) = logo {
        let packed = miniz_oxide::deflate::compress_to_vec_zlib(&l.rgb, 6);
        let mut x = pdf.image_xobject(image, &packed);
        x.filter(Filter::FlateDecode);
        x.width(i32::try_from(l.width).unwrap_or(1));
        x.height(i32::try_from(l.height).unwrap_or(1));
        x.color_space().device_rgb();
        x.bits_per_component(8);
        x.finish();
    }

    let page_count = ids.len();
    for (i, (mut content, (page_ref, content_ref))) in pages.into_iter().zip(ids).enumerate() {
        if i == 0
            && let Some(l) = logo
        {
            #[allow(clippy::cast_precision_loss)]
            let (w, h) = {
                let scale = (160.0 / l.width as f32).min(50.0 / l.height as f32);
                (l.width as f32 * scale, l.height as f32 * scale)
            };
            content.save_state();
            content.transform([w, 0.0, 0.0, h, MARGIN, HEIGHT - MARGIN - h]);
            content.x_object(Name(b"Logo"));
            content.restore_state();
        }
        let footer = format!("Generated by zWorkTracker. Page {} of {page_count}.", i + 1);
        #[cfg(test)]
        w.shown.push(footer.clone());
        let encoded = w.fonts.encode(Weight::Regular, &footer);
        show(
            &mut content,
            MARGIN,
            MARGIN - 18.0,
            Weight::Regular,
            8.0,
            &encoded,
        );
        let stream = content.finish();
        pdf.stream(content_ref, stream.as_slice());
        let mut page = pdf.page(page_ref);
        page.media_box(Rect::new(0.0, 0.0, WIDTH, HEIGHT));
        page.parent(tree);
        page.contents(content_ref);
        let mut res = page.resources();
        let mut fonts = res.fonts();
        fonts.pair(Name(b"Reg"), regular);
        fonts.pair(Name(b"Bold"), bold);
        fonts.finish();
        if logo.is_some() {
            res.x_objects().pair(Name(b"Logo"), image);
        }
    }
    // Last, since each page's footer can still add to what the fonts must hold.
    w.fonts.embed(&mut pdf, regular, bold, &mut alloc)?;
    Ok(pdf.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::billing::tests::{fixture, seg};
    use crate::report::tests::source;

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }
    /// Everything the document prints, in order.
    fn text(source: &ExportSource, request: &PdfRequest) -> String {
        render(source, request, None).unwrap().1.shown.join("\n")
    }
    /// The code points of every `ToUnicode` map: what a reader can get out of the file by copying.
    fn copyable(bytes: &[u8]) -> Vec<u32> {
        let raw = String::from_utf8_lossy(bytes);
        let mut out = Vec::new();
        for block in raw.split("beginbfchar").skip(1) {
            let body = block.split("endbfchar").next().unwrap();
            for line in body.lines() {
                let hex = line.rsplit(['<', ' ']).next().unwrap_or("");
                let hex = hex.trim_end_matches('>');
                if let Ok(v) = u32::from_str_radix(hex, 16) {
                    out.push(v);
                }
            }
        }
        out
    }
    fn src(n: usize) -> ExportSource {
        let (mut s, _, t) = fixture(None);
        for i in 0..n {
            let day = 1 + i % 28;
            let month = 1 + i / 28;
            seg(
                &mut s,
                &t,
                &format!("s{i}"),
                &format!("2026-{month:02}-{day:02}T09:00:00Z"),
                &format!("2026-{month:02}-{day:02}T10:00:00Z"),
            );
        }
        source(s, "2026-12-31T00:00:00Z")
    }
    fn req(prices: bool) -> PdfRequest {
        PdfRequest {
            project_id: None,
            from: "2026-01-01".into(),
            to: "2026-12-31".into(),
            zone: "America/New_York".into(),
            title: "March".into(),
            include_prices: prices,
            use_logo: false,
            selection: None,
        }
    }

    #[test]
    fn it_is_a_pdf_naming_the_zone_with_a_total() {
        let b = build(&src(2), &req(true), None).unwrap().bytes;
        assert!(b.starts_with(b"%PDF-"));
        let t = text(&src(2), &req(true));
        assert!(t.contains("America/New_York"));
        assert!(t.contains("Total time"));
        if let Ok(path) = std::env::var("ZWT_SAMPLE_PDF") {
            std::fs::write(path, &b).unwrap();
        }
    }

    #[test]
    fn with_prices_off_no_money_appears() {
        let t = text(&src(2), &req(false));
        assert!(!t.contains("Amount"));
        assert!(!t.contains("Total:"));
        for mark in ["\u{20ac}", "$", "EUR", "USD"] {
            assert!(!t.contains(mark), "{mark}");
        }
        // Nor can a reader copy one out of the file: the font holds only what was printed.
        let b = build(&src(2), &req(false), None).unwrap().bytes;
        let codes = copyable(&b);
        assert!(!codes.is_empty(), "the map of copyable text was read");
        assert!(
            !codes.contains(&0x24) && !codes.contains(&0x20ac),
            "{codes:?}"
        );
    }

    #[test]
    fn long_timesheets_paginate() {
        assert!(text(&src(120), &req(true)).contains("Page 3 of"));
    }

    fn named(task: &str, project: &str) -> ExportSource {
        let (mut s, p, t) = fixture(None);
        s.projects.get_mut(&p).unwrap().name = project.to_owned();
        s.tasks.get_mut(&t).unwrap().name = task.to_owned();
        seg(
            &mut s,
            &t,
            "a",
            "2026-03-01T09:00:00Z",
            "2026-03-01T10:00:00Z",
        );
        source(s, "2026-04-01T00:00:00Z")
    }

    #[test]
    fn greek_cyrillic_and_accents_print_with_no_warning_and_can_be_copied() {
        let src = named("Ωμέγα Привет", "Ação Łódź");
        let doc = build(&src, &req(true), None).unwrap();
        assert!(doc.unprintable.is_empty());
        let t = text(&src, &req(true));
        assert!(t.contains("Ωμέγα Привет") && t.contains("Ação Łódź"));
        assert!(!t.contains("empty box"));
        let codes = copyable(&doc.bytes);
        for c in "ΩПçŁ".chars() {
            assert!(codes.contains(&u32::from(c)), "{c}");
        }
    }

    #[test]
    fn what_the_font_lacks_is_named_on_the_page_and_returned_and_still_copies() {
        let src = named("日本語 שלום", "😀");
        let doc = build(&src, &req(true), None).unwrap();
        for c in ['日', '本', 'ש', '😀'] {
            assert!(doc.unprintable.contains(&c), "{c}");
        }
        let t = text(&src, &req(true));
        assert!(t.contains("print as an empty box"), "{t}");
        assert!(t.contains("U+05E9"), "{t}");
        let widest = Fonts::new().unwrap();
        for line in t.lines().filter(|l| l.contains("empty box")) {
            assert!(widest.measure(Weight::Regular, line, 8.0) <= WIDTH - 2.0 * MARGIN);
        }
        assert!(!t.contains('?'), "never a question mark: {t}");
        let codes = copyable(&doc.bytes);
        assert!(codes.contains(&0x65e5), "日 copies as typed");
        // An emoji is outside the 16-bit range, so its map entry is a surrogate pair.
        assert!(
            codes.contains(&0xd83d_de00),
            "😀 copies as typed: {codes:?}"
        );
    }

    /// Real extraction and a rendered page are in `scripts/verify_pdf.py`, which reads this file.
    #[test]
    fn the_mixed_script_sample_builds_and_is_written_when_asked() {
        let (mut s, p, _) = fixture(None);
        let c = crate::machine::tests::ctx("2026-01-01T00:00:00Z");
        s.projects.get_mut(&p).unwrap().name = "Ελληνικά Client".into();
        for (i, (name, day)) in [
            ("Ωμέγα αρχείο", 2),
            ("Отчёт за март", 3),
            ("Ação Łódź naïve Škoda", 4),
            ("Cafe\u{301} decomposed", 5),
            ("日本語の作業", 6),
            ("한국어 中文", 7),
            ("שלום Hebrew", 8),
            ("مرحبا Arabic", 9),
            ("شلوم, عولم", 12),
            ("אב 12 גד", 13),
            ("Launch 😀👍🏽 prep", 10),
            (
                "Long name Ωμέγα Привет Ação that must be cut to fit its column on the page",
                11,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let t = s
                .create_task(&c, &crate::machine::tests::task_input(name, &p))
                .unwrap();
            seg(
                &mut s,
                &t,
                &format!("m{i}"),
                &format!("2026-03-{day:02}T09:00:00Z"),
                &format!("2026-03-{day:02}T10:00:00Z"),
            );
        }
        let src = source(s, "2026-04-01T00:00:00Z");
        let request = PdfRequest {
            title: "Timesheet Ωμέγα Привет Ação 日本語".into(),
            ..req(true)
        };
        let doc = build(&src, &request, None).unwrap();
        assert!(doc.unprintable.contains(&'日'));
        assert!(!doc.unprintable.contains(&'Ω'));
        if let Ok(path) = std::env::var("ZWT_SAMPLE_UNICODE_PDF") {
            std::fs::write(path, &doc.bytes).unwrap();
        }
    }

    #[test]
    fn the_warning_counts_and_cuts_a_long_list() {
        assert_eq!(unprintable_note(&[]), None);
        assert_eq!(
            unprintable_note(&['日']).unwrap(),
            "1 character prints as an empty box, as this font has no glyph: U+65E5."
        );
        let many: Vec<char> = ('一'..='十').take(10).collect();
        assert!(unprintable_note(&many).unwrap().ends_with("and 5 more."));
    }

    #[test]
    fn the_same_data_gives_the_same_file() {
        let a = build(&src(3), &req(true), None).unwrap().bytes;
        let b = build(&src(3), &req(true), None).unwrap().bytes;
        assert_eq!(a, b);
    }

    #[test]
    fn a_png_logo_is_embedded_and_a_non_png_is_refused() {
        let mut bytes = Vec::new();
        {
            let mut e = png::Encoder::new(&mut bytes, 2, 1);
            e.set_color(png::ColorType::Rgba);
            e.set_depth(png::BitDepth::Eight);
            let mut w = e.write_header().unwrap();
            w.write_image_data(&[255, 0, 0, 255, 0, 0, 0, 0]).unwrap();
        }
        let logo = decode_logo(&bytes).unwrap();
        assert_eq!(
            logo.rgb,
            [255, 0, 0, 255, 255, 255],
            "transparent flattens onto white"
        );
        let b = build(
            &src(1),
            &PdfRequest {
                use_logo: true,
                ..req(true)
            },
            Some(&logo),
        )
        .unwrap()
        .bytes;
        assert!(contains(&b, b"/Logo"));
        assert!(decode_logo(b"not a png").is_err());
    }

    #[test]
    fn a_selection_prints_only_its_own_task_day_and_says_so() {
        let (mut s, p, t) = fixture(None);
        let c = crate::machine::tests::ctx("2026-01-01T00:00:00Z");
        let other = s
            .create_task(&c, &crate::machine::tests::task_input("Secret", &p))
            .unwrap();
        seg(
            &mut s,
            &t,
            "a",
            "2026-03-01T09:00:00Z",
            "2026-03-01T10:00:00Z",
        );
        seg(
            &mut s,
            &t,
            "b",
            "2026-03-02T09:00:00Z",
            "2026-03-02T10:00:00Z",
        );
        seg(
            &mut s,
            &other,
            "z",
            "2026-03-01T11:00:00Z",
            "2026-03-01T12:00:00Z",
        );
        // The dialog and README say only Excel has a Subtasks column. If this prints one, they must change.
        s.tasks.get_mut(&t).unwrap().description = serde_json::json!({"type":"doc","content":[
            {"type":"taskList","content":[
                {"type":"taskItem","attrs":{"checked":true},"content":[
                    {"type":"paragraph","content":[{"type":"text","text":"Subtask of another day"}]}
                ]}
            ]}
        ]});
        let src = source(s, "2026-04-01T00:00:00Z");
        let request = PdfRequest {
            selection: Some(vec![crate::report::SelectedEntry {
                task_id: t.clone(),
                day: "2026-03-01".to_owned(),
            }]),
            ..req(true)
        };
        let t = text(&src, &request);
        assert!(t.contains("Selection of 1 entries"));
        assert!(!t.contains("Secret"));
        assert!(!t.contains("Subtask of another day"));
        assert!(!t.contains("2026-03-02"));
        let bytes = build(&src, &request, None).unwrap().bytes;
        assert!(!contains(&bytes, b"Secret"));
        let empty = PdfRequest {
            selection: Some(Vec::new()),
            ..req(true)
        };
        assert!(build(&src, &empty, None).is_err());
    }

    const NOTE: &str = "Row amounts are rounded; the total is worked out from the exact amounts.";

    fn priced(s: &mut crate::model::Shared, p: &str, cents: i64, cur: &str, from: &str) {
        s.set_rate(
            &crate::machine::tests::ctx("2026-06-01T00:00:00Z"),
            &crate::machine::RateInput {
                project_id: p.to_owned(),
                rate_cents: Some(cents),
                currency: cur.into(),
                effective_from: Some(from.into()),
                confirm_retroactive: true,
            },
        )
        .unwrap();
    }

    /// A row a day from 2 January, `minutes` long each.
    fn daily(s: &mut crate::model::Shared, t: &str, first: usize, rows: usize, minutes: i64) {
        for i in first..first + rows {
            let start = chrono::NaiveDate::from_ymd_opt(2026, 1, 2)
                .unwrap()
                .and_hms_opt(9, 0, 0)
                .unwrap()
                .and_utc()
                + chrono::Duration::days(i as i64);
            let end = start + chrono::Duration::minutes(minutes);
            seg(
                s,
                t,
                &format!("d{i}"),
                &start.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
                &end.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            );
        }
    }

    /// The total the Earnings sheet holds for a currency: the value saved with its formula.
    fn excel_total(sheets: &str, cur: &str) -> f64 {
        let sheets = sheets.replace("&quot;", "\"");
        let cell = sheets
            .split(&format!("\"{cur}\",D3:D"))
            .nth(1)
            .unwrap_or_else(|| panic!("no {cur} total on the Earnings sheet"));
        let value = cell.split("<v>").nth(1).unwrap().split("</v>").next();
        value.unwrap().parse().unwrap()
    }

    /// Runs one data set through the summary, the PDF and the workbook, and holds each to `want`.
    fn assert_pdf_summary_and_excel_say(
        mut s: crate::model::Shared,
        p: &str,
        want: &[(&str, i64)],
    ) -> String {
        let wanted: BTreeMap<String, i64> =
            want.iter().map(|(c, v)| ((*c).to_owned(), *v)).collect();
        let src = source(s.clone(), "2026-12-31T00:00:00Z");

        let request = PdfRequest {
            zone: "UTC".into(),
            ..req(true)
        };
        let printed = text(&src, &request);
        let shown: Vec<String> = want.iter().map(|(c, v)| money(c, *v)).collect();
        assert!(
            printed.contains(&format!("Total: {}", shown.join(" + "))),
            "PDF: {printed}"
        );

        let book = crate::xlsx::build(
            &src,
            &crate::report::ExportRequest {
                day_zone: "UTC".into(),
                my_zone: "UTC".into(),
                client_zone: None,
                include_earnings: true,
                segment_shifts: BTreeMap::new(),
                analytics: vec![],
                selection: None,
            },
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let sheets = crate::xlsx::tests::unzip_all(&book, dir.path());
        for (cur, cents) in want {
            assert_eq!(
                excel_total(&sheets, cur),
                *cents as f64 / 100.0,
                "Excel {cur}"
            );
        }
        assert!(sheets.contains(NOTE), "the title of the Earnings sheet");

        let period = crate::summary::SummaryRequest {
            project_id: p.to_owned(),
            from: "2026-01-01".into(),
            to: "2026-12-31".into(),
            zone: "UTC".into(),
        };
        assert_eq!(
            s.summary_preview(&period).unwrap().totals,
            wanted,
            "preview"
        );
        let c = crate::machine::tests::ctx("2026-12-31T00:00:00Z");
        let id = s.create_summary(&c, &period).unwrap();
        assert_eq!(s.summaries[&id].totals, wanted, "summary");
        let csv = crate::summary::summary_csv(&s.summaries[&id], "P", false);
        let total = csv.lines().find(|l| l.starts_with("Total,")).unwrap();
        for (_, cents) in want {
            let cell = format!(",{}.{:02}", cents / 100, cents % 100);
            assert!(total.contains(&cell), "summary CSV: {total} has no {cell}");
        }
        printed
    }

    #[test]
    fn r7_pdf_summary_and_excel_totals_agree_over_one_data_set() {
        // 15 minutes at 33.33 an hour is 8.3325, shown as 8.33. Forty of those shown add up to 333.20; the
        // exact amounts come to 333.30.
        let (mut s, p, t) = fixture(None);
        priced(&mut s, &p, 3333, "EUR", "2026-01-01T00:00:00Z");
        daily(&mut s, &t, 0, 40, 15);
        let printed = assert_pdf_summary_and_excel_say(s, &p, &[("EUR", 33330)]);
        assert_eq!(printed.matches("\u{20ac}8.33").count(), 40, "{printed}");
        assert!(printed.contains(NOTE), "{printed}");
    }

    #[test]
    fn r7_pdf_summary_and_excel_totals_agree_with_mixed_rates() {
        // Rows of 833.25, 833.25, 1666.5 + 834.25 and 834.25 cents: shown as 833, 833, 2501, 834, which add
        // up to 5001. Exactly they are 5001.5, which rounds up to 5002 once.
        let (mut s, p, t) = fixture(None);
        priced(&mut s, &p, 3333, "EUR", "2026-01-01T00:00:00Z");
        priced(&mut s, &p, 3337, "EUR", "2026-01-04T09:30:00Z");
        daily(&mut s, &t, 0, 2, 15);
        daily(&mut s, &t, 2, 1, 45);
        daily(&mut s, &t, 3, 1, 15);
        let printed = assert_pdf_summary_and_excel_say(s, &p, &[("EUR", 5002)]);
        assert!(printed.contains("\u{20ac}25.01"), "{printed}");
    }

    #[test]
    fn r7_pdf_summary_and_excel_totals_agree_across_two_currencies() {
        // Rows of 833.25: three in euros, two in dollars, and one with 15 minutes on each side of the change,
        // so 4 x 833.25 euro cents (shown 33.32, exactly 33.33) and 3 x 833.25 dollar cents (24.99, exactly 25.00).
        let (mut s, p, t) = fixture(None);
        priced(&mut s, &p, 3333, "EUR", "2026-01-01T00:00:00Z");
        priced(&mut s, &p, 3333, "USD", "2026-01-05T09:15:00Z");
        daily(&mut s, &t, 0, 3, 15);
        daily(&mut s, &t, 3, 1, 30);
        daily(&mut s, &t, 4, 2, 15);
        let printed = assert_pdf_summary_and_excel_say(s, &p, &[("EUR", 3333), ("USD", 2500)]);
        assert!(printed.contains("\u{20ac}8.33 + $8.33"), "{printed}");
    }

    #[test]
    fn r7_the_totals_are_of_billable_time_when_an_increment_is_on() {
        // 20 minutes up to 30, at 33.33 an hour: 16.665, shown 16.67. Three of those shown are 50.01; the exact
        // amounts of the billable time come to 49.995, so 50.00. Not 33.33, which is the 20 minutes unrounded.
        let (mut s, p, t) = fixture(None);
        priced(&mut s, &p, 3333, "EUR", "2026-01-01T00:00:00Z");
        daily(&mut s, &t, 0, 3, 20);
        assert_pdf_summary_and_excel_say(s, &p, &[("EUR", 5000)]);
    }

    #[test]
    fn r7_the_note_is_printed_only_where_rows_do_not_add_up_to_the_total() {
        let (mut s, p, t) = fixture(None);
        priced(&mut s, &p, 3333, "EUR", "2026-01-01T00:00:00Z");
        daily(&mut s, &t, 0, 40, 15);
        let src = source(s, "2026-12-31T00:00:00Z");
        let request = PdfRequest {
            zone: "UTC".into(),
            ..req(true)
        };
        assert!(text(&src, &request).contains(NOTE));
        // With prices off there is no amount for it to speak of, and it isn't in the file to be copied.
        let hidden = PdfRequest {
            include_prices: false,
            ..request.clone()
        };
        assert!(!text(&src, &hidden).contains(NOTE));

        let (mut s, p, t) = fixture(None);
        priced(&mut s, &p, 6000, "EUR", "2026-01-01T00:00:00Z");
        daily(&mut s, &t, 0, 3, 60);
        let src = source(s, "2026-12-31T00:00:00Z");
        assert!(!text(&src, &request).contains(NOTE), "these add up");
    }
}
