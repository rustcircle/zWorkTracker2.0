//! Excel export. Sheet 1 is exactly Project | Day | Time | Task name | Subtasks. User text is always
//! written as a string and never as a formula.

use std::collections::BTreeMap;

use chrono::Datelike;
use chrono_tz::Tz;
use rust_xlsxwriter::{
    Color, ExcelDateTime, Format, FormatAlign, FormatBorder, Formula, Workbook, Worksheet,
    XlsxError,
};
use serde_json::Value;

use crate::billing::{ROUNDED_ROWS_NOTE, split_by_rate, total_cents};
use crate::error::CoreError;
use crate::model::RoundMethod;
use crate::report::{AnalyticsSheet, CellKind, ExportRequest, ExportSource, zoned};
use crate::validate::parse_zone;

const SECS_PER_DAY: f64 = 86_400.0;
const MAX_CELL_CHARS: usize = 32_000;
const DURATION_FORMAT: &str = "[h]:mm:ss";
const HEADER_BG: u32 = 0x1F_2937;

fn err(e: XlsxError) -> CoreError {
    CoreError::Xlsx(e.to_string())
}

/// WCAG relative luminance from a `#rrggbb` color, and the better of black or white text on it.
#[must_use]
pub fn parse_rgb(color: &str) -> u32 {
    u32::from_str_radix(color.trim_start_matches('#'), 16).unwrap_or(0x80_8080)
}

#[must_use]
pub fn readable_text(rgb: u32) -> u32 {
    let channel = |shift: u32| {
        let c = f64::from((rgb >> shift) & 0xff) / 255.0;
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let l = 0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0);
    let on_white = 1.05 / (l + 0.05);
    let on_black = (l + 0.05) / 0.05;
    if on_black >= on_white {
        0x00_0000
    } else {
        0xFF_FFFF
    }
}

fn clip(text: &str) -> String {
    if text.chars().count() > MAX_CELL_CHARS {
        let mut cut: String = text.chars().take(MAX_CELL_CHARS).collect();
        cut.push_str("\n(truncated)");
        cut
    } else {
        text.to_owned()
    }
}

fn duration(secs: i64) -> f64 {
    secs as f64 / SECS_PER_DAY
}

fn describe_rule(increment: u32, method: RoundMethod) -> String {
    if increment == 0 {
        return "exact (no rounding)".to_owned();
    }
    let m = match method {
        RoundMethod::Nearest => "nearest",
        RoundMethod::Up => "up",
        RoundMethod::Down => "down",
    };
    format!("{increment} min, {m}")
}

fn clean_sheet_name(raw: &str, used: &mut Vec<String>) -> String {
    let mut name: String = raw
        .chars()
        .filter(|c| !"[]:*?/\\".contains(*c))
        .take(28)
        .collect();
    name = name.trim_matches('\'').trim().to_owned();
    if name.is_empty() {
        name = "Sheet".to_owned();
    }
    let base = name.clone();
    let mut n = 2;
    while used.iter().any(|u| u.eq_ignore_ascii_case(&name)) {
        name = format!("{base} {n}");
        n += 1;
    }
    used.push(name.clone());
    name
}

struct Styles {
    title: Format,
    header: Format,
    date: Format,
    time: Format,
    time_flagged: Format,
    text: Format,
    wrap: Format,
    bold: Format,
    total_time: Format,
    grand_label: Format,
    grand_time: Format,
}

impl Styles {
    fn new() -> Self {
        let header = Format::new()
            .set_bold()
            .set_font_color(Color::White)
            .set_background_color(Color::RGB(HEADER_BG))
            .set_border_bottom(FormatBorder::Thin)
            .set_align(FormatAlign::VerticalCenter);
        let top = Format::new().set_align(FormatAlign::Top);
        Self {
            title: Format::new().set_bold().set_font_size(11),
            header,
            date: top
                .clone()
                .set_num_format("yyyy-mm-dd")
                .set_align(FormatAlign::Left),
            time: top.clone().set_num_format(DURATION_FORMAT),
            time_flagged: top
                .clone()
                .set_num_format(DURATION_FORMAT)
                .set_italic()
                .set_font_color(Color::RGB(0xB4_5309)),
            text: top.clone().set_text_wrap(),
            wrap: top.set_text_wrap(),
            bold: Format::new().set_bold().set_border_top(FormatBorder::Thin),
            total_time: Format::new()
                .set_bold()
                .set_num_format(DURATION_FORMAT)
                .set_border_top(FormatBorder::Thin),
            grand_label: Format::new()
                .set_bold()
                .set_border_top(FormatBorder::Double)
                .set_background_color(Color::RGB(0xE5_E7EB)),
            grand_time: Format::new()
                .set_bold()
                .set_num_format(DURATION_FORMAT)
                .set_border_top(FormatBorder::Double)
                .set_background_color(Color::RGB(0xE5_E7EB)),
        }
    }
}

fn write_labor_sheet(
    sheet: &mut Worksheet,
    source: &ExportSource,
    zone_name: &str,
    tz: Tz,
    st: &Styles,
) -> Result<(), CoreError> {
    sheet.set_name("Labor by project").map_err(err)?;
    let rows = source.rows(tz);

    let mut rules: Vec<String> = rows
        .iter()
        .map(|r| describe_rule(r.row.rule.increment_minutes, r.row.rule.method))
        .collect();
    rules.sort();
    rules.dedup();
    let rule_text = if rules.is_empty() {
        "none".to_owned()
    } else {
        rules.join("; ")
    };
    let flagged = rows
        .iter()
        .any(|r| r.row.rounded_to_zero || r.row.zero_exact);
    let mut title = format!(
        "Labor hours by project. Days in zone: {zone_name}. Time increment: {rule_text}. Exported {}.",
        source.now.iso()
    );
    if flagged {
        title
            .push_str(" Italic time: a row whose time is 0 (no exact time, or rounded down to 0).");
    }
    sheet
        .write_string_with_format(0, 0, &title, &st.title)
        .map_err(err)?;

    for (col, name) in ["Project", "Day", "Time", "Task name", "Subtasks"]
        .iter()
        .enumerate()
    {
        sheet
            .write_string_with_format(1, col as u16, *name, &st.header)
            .map_err(err)?;
    }
    sheet.set_row_height(1, 20).map_err(err)?;
    for (col, width) in [(0u16, 26.0), (1, 12.0), (2, 11.0), (3, 42.0), (4, 62.0)] {
        sheet.set_column_width(col, width).map_err(err)?;
    }
    sheet.set_freeze_panes(2, 0).map_err(err)?;

    let mut row: u32 = 2;
    let first_data_row = row;
    let mut grand: i64 = 0;
    let mut i = 0usize;
    while i < rows.len() {
        let Some(head) = rows.get(i) else { break };
        let bg = parse_rgb(&head.project_color);
        let project_format = Format::new()
            .set_background_color(Color::RGB(bg))
            .set_font_color(Color::RGB(readable_text(bg)))
            .set_align(FormatAlign::Top);
        let start = row;
        let mut subtotal: i64 = 0;
        while let Some(r) = rows.get(i).filter(|r| r.project_name == head.project_name) {
            sheet
                .write_string_with_format(row, 0, clip(&r.project_name), &project_format)
                .map_err(err)?;
            let date = ExcelDateTime::from_ymd(
                u16::try_from(r.row.day.year()).unwrap_or(1970),
                u8::try_from(r.row.day.month()).unwrap_or(1),
                u8::try_from(r.row.day.day()).unwrap_or(1),
            )
            .map_err(err)?;
            sheet
                .write_datetime_with_format(row, 1, &date, &st.date)
                .map_err(err)?;
            let time_fmt = if r.row.rounded_to_zero || r.row.zero_exact {
                &st.time_flagged
            } else {
                &st.time
            };
            sheet
                .write_number_with_format(row, 2, duration(r.row.billable_secs), time_fmt)
                .map_err(err)?;
            sheet
                .write_string_with_format(row, 3, clip(&r.task_name), &st.text)
                .map_err(err)?;
            let subtasks = clip(&r.subtasks);
            if subtasks.is_empty() {
                sheet.write_blank(row, 4, &st.wrap).map_err(err)?;
            } else {
                sheet
                    .write_string_with_format(row, 4, &subtasks, &st.wrap)
                    .map_err(err)?;
            }
            let lines: usize = subtasks
                .split('\n')
                .map(|l| l.chars().count().div_ceil(64).max(1))
                .sum::<usize>()
                .max(1);
            let name_lines = r.task_name.chars().count().div_ceil(44).max(1);
            let height = (lines.max(name_lines) as f64 * 15.0).clamp(15.0, 400.0);
            sheet.set_row_height(row, height).map_err(err)?;
            subtotal += r.row.billable_secs;
            row += 1;
            i += 1;
        }
        grand += subtotal;
        sheet
            .write_string_with_format(row, 0, clip(&head.project_name), &project_format)
            .map_err(err)?;
        sheet
            .write_string_with_format(row, 1, "Subtotal", &st.bold)
            .map_err(err)?;
        // SUBTOTAL(9) follows the autofilter and is skipped by the grand total's own SUBTOTAL.
        let formula = Formula::new(format!("=SUBTOTAL(9,C{}:C{})", start + 1, row))
            .set_result(duration(subtotal).to_string());
        sheet
            .write_formula_with_format(row, 2, formula, &st.total_time)
            .map_err(err)?;
        sheet.write_blank(row, 3, &st.bold).map_err(err)?;
        sheet.write_blank(row, 4, &st.bold).map_err(err)?;
        row += 1;
    }
    let last_filter_row = row.saturating_sub(1).max(1);
    sheet.autofilter(1, 0, last_filter_row, 4).map_err(err)?;

    sheet
        .write_string_with_format(row, 0, "Grand total", &st.grand_label)
        .map_err(err)?;
    sheet.write_blank(row, 1, &st.grand_label).map_err(err)?;
    let formula_range = if row > first_data_row {
        format!("C{}:C{}", first_data_row + 1, row)
    } else {
        format!("C{0}:C{0}", row + 1)
    };
    if row > first_data_row {
        let formula = Formula::new(format!("=SUBTOTAL(9,{formula_range})"))
            .set_result(duration(grand).to_string());
        sheet
            .write_formula_with_format(row, 2, formula, &st.grand_time)
            .map_err(err)?;
    } else {
        sheet
            .write_number_with_format(row, 2, 0.0, &st.grand_time)
            .map_err(err)?;
    }
    sheet.write_blank(row, 3, &st.grand_label).map_err(err)?;
    sheet.write_blank(row, 4, &st.grand_label).map_err(err)?;
    sheet.set_landscape();
    sheet.set_repeat_rows(1, 1).map_err(err)?;
    sheet.set_print_fit_to_pages(1, 0);
    Ok(())
}

fn write_details_sheet(
    sheet: &mut Worksheet,
    source: &ExportSource,
    request: &ExportRequest,
    st: &Styles,
) -> Result<(), CoreError> {
    sheet.set_name("Segment details").map_err(err)?;
    let my: Tz = parse_zone(&request.my_zone)?;
    let client: Option<Tz> = request.client_zone.as_deref().map(parse_zone).transpose()?;
    sheet
        .write_string_with_format(0, 0, "Every work segment. Raw times in UTC are never rounded. Exact is the unrounded duration.", &st.title)
        .map_err(err)?;
    let mut headers: Vec<String> = vec![
        "Project".into(),
        "Task name".into(),
        "Start UTC".into(),
        "End UTC".into(),
        format!("Start {}", request.my_zone),
        format!("End {}", request.my_zone),
    ];
    if let Some(z) = &request.client_zone {
        headers.push(format!("Start {z}"));
        headers.push(format!("End {z}"));
    }
    headers.push("Exact".into());
    headers.push("Shift".into());
    if request.include_earnings {
        headers.extend([
            "Rate per hour".to_owned(),
            "Currency".to_owned(),
            "Exact amount".to_owned(),
        ]);
    }
    for (col, h) in headers.iter().enumerate() {
        sheet
            .write_string_with_format(1, col as u16, h, &st.header)
            .map_err(err)?;
        let width = match h.as_str() {
            "Project" => 22.0,
            "Task name" => 32.0,
            "Exact" => 10.0,
            "Shift" => 26.0,
            _ if h.starts_with("Start") || h.starts_with("End") => 27.0,
            _ => 13.0,
        };
        sheet.set_column_width(col as u16, width).map_err(err)?;
    }
    sheet.set_freeze_panes(2, 0).map_err(err)?;

    let money = Format::new().set_num_format("#,##0.00");
    let mut row: u32 = 2;
    let segments = source.segments();
    // The UI's text covers whole segments, which a selection can cut, so a selection works out its own.
    let own_shifts = source.only.as_ref().map(|_| {
        let from = segments.iter().map(|s| s.start).min().unwrap_or_default();
        let to = segments
            .iter()
            .map(|s| s.effective_end)
            .max()
            .unwrap_or_default();
        crate::analytics::ShiftText::new(&source.data, from, to)
    });
    for seg in segments {
        let own_text;
        let shift_text = if let Some(own) = &own_shifts {
            own_text = own.of(&source.data, seg.start, seg.effective_end);
            own_text.as_str()
        } else {
            request
                .segment_shifts
                .get(&seg.segment_id)
                .map_or("", String::as_str)
        };
        let pieces = if request.include_earnings {
            split_by_rate(&source.data, &seg.project_id, seg.start, seg.effective_end)
        } else {
            vec![crate::billing::RatePiece {
                start: seg.start,
                end: seg.effective_end,
                rate_cents: None,
                currency: None,
            }]
        };
        let last = pieces.len().saturating_sub(1);
        for (i, p) in pieces.iter().enumerate() {
            let shown_end = if i == last { seg.end } else { Some(p.end) };
            sheet
                .write_string(row, 0, clip(&seg.project_name))
                .map_err(err)?;
            sheet
                .write_string(row, 1, clip(&seg.task_name))
                .map_err(err)?;
            sheet.write_string(row, 2, p.start.iso()).map_err(err)?;
            sheet
                .write_string(
                    row,
                    3,
                    shown_end.map_or_else(|| "(running)".to_owned(), |t| t.iso()),
                )
                .map_err(err)?;
            sheet
                .write_string(row, 4, zoned(p.start, my))
                .map_err(err)?;
            sheet
                .write_string(row, 5, shown_end.map_or_else(String::new, |t| zoned(t, my)))
                .map_err(err)?;
            let mut col: u16 = 6;
            if let Some(z) = client {
                sheet
                    .write_string(row, col, zoned(p.start, z))
                    .map_err(err)?;
                sheet
                    .write_string(
                        row,
                        col + 1,
                        shown_end.map_or_else(String::new, |t| zoned(t, z)),
                    )
                    .map_err(err)?;
                col += 2;
            }
            let secs = p.end.secs() - p.start.secs();
            sheet
                .write_number_with_format(row, col, duration(secs), &st.time)
                .map_err(err)?;
            sheet
                .write_string(row, col + 1, clip(shift_text))
                .map_err(err)?;
            if request.include_earnings
                && let (Some(rate), Some(cur)) = (p.rate_cents, &p.currency)
            {
                sheet
                    .write_number_with_format(row, col + 2, rate as f64 / 100.0, &money)
                    .map_err(err)?;
                sheet.write_string(row, col + 3, cur).map_err(err)?;
                sheet
                    .write_number_with_format(
                        row,
                        col + 4,
                        (rate * secs) as f64 / 360_000.0,
                        &money,
                    )
                    .map_err(err)?;
            }
            row += 1;
        }
    }
    if row > 2 {
        let last_col = headers.len().saturating_sub(1) as u16;
        sheet.autofilter(1, 0, row - 1, last_col).map_err(err)?;
    }
    Ok(())
}

fn currency_format(cur: &str) -> &'static str {
    if cur == "EUR" {
        "[$\u{20ac}-x-euro2]#,##0.00"
    } else {
        "[$$-409]#,##0.00"
    }
}

fn write_earnings_sheet(
    sheet: &mut Worksheet,
    source: &ExportSource,
    zone_name: &str,
    tz: Tz,
    st: &Styles,
) -> Result<(), CoreError> {
    sheet.set_name("Earnings").map_err(err)?;
    let rows = source.rows(tz);
    let cents = total_cents(rows.iter().map(|r| &r.row));
    // project -> currency -> (billable seconds under that currency, numerator)
    let mut per: BTreeMap<(String, String), (i64, i64)> = BTreeMap::new();
    for r in &rows {
        for b in &r.row.buckets {
            if let (Some(rate), Some(cur)) = (b.rate_cents, &b.currency) {
                let e = per
                    .entry((r.project_name.clone(), cur.clone()))
                    .or_insert((0, 0));
                e.0 += b.billable_secs;
                e.1 += rate * b.billable_secs;
            }
        }
    }
    sheet
        .write_string_with_format(0, 0, format!("Earnings from billable (rounded) time. Days in zone: {zone_name}. Currencies are never added together. {ROUNDED_ROWS_NOTE}"), &st.title)
        .map_err(err)?;
    for (col, h) in ["Project", "Currency", "Billable time", "Amount"]
        .iter()
        .enumerate()
    {
        sheet
            .write_string_with_format(1, col as u16, *h, &st.header)
            .map_err(err)?;
    }
    for (col, w) in [(0u16, 30.0), (1, 10.0), (2, 14.0), (3, 16.0)] {
        sheet.set_column_width(col, w).map_err(err)?;
    }
    sheet.set_freeze_panes(2, 0).map_err(err)?;
    let mut row: u32 = 2;
    let mut totals: BTreeMap<String, i64> = BTreeMap::new();
    for ((project, cur), (secs, numerator)) in &per {
        sheet.write_string(row, 0, clip(project)).map_err(err)?;
        sheet.write_string(row, 1, cur).map_err(err)?;
        sheet
            .write_number_with_format(row, 2, duration(*secs), &st.time)
            .map_err(err)?;
        let f = Format::new().set_num_format(currency_format(cur));
        sheet
            .write_number_with_format(row, 3, *numerator as f64 / 360_000.0, &f)
            .map_err(err)?;
        *totals.entry(cur.clone()).or_insert(0) += secs;
        row += 1;
    }
    let last = row;
    for (cur, secs) in &totals {
        sheet
            .write_string_with_format(row, 0, format!("Total {cur}"), &st.bold)
            .map_err(err)?;
        sheet
            .write_string_with_format(row, 1, cur, &st.bold)
            .map_err(err)?;
        let time = Formula::new(format!("=SUMIF(B3:B{last},\"{cur}\",C3:C{last})"))
            .set_result(duration(*secs).to_string());
        sheet
            .write_formula_with_format(row, 2, time, &st.total_time)
            .map_err(err)?;
        let amount = Formula::new(format!("=SUMIF(B3:B{last},\"{cur}\",D3:D{last})"))
            .set_result((cents.get(cur).copied().unwrap_or(0) as f64 / 100.0).to_string());
        let f = Format::new()
            .set_bold()
            .set_num_format(currency_format(cur))
            .set_border_top(FormatBorder::Thin);
        sheet
            .write_formula_with_format(row, 3, amount, &f)
            .map_err(err)?;
        row += 1;
    }
    Ok(())
}

fn write_analytics_sheet(
    sheet: &mut Worksheet,
    a: &AnalyticsSheet,
    name: &str,
    st: &Styles,
) -> Result<(), CoreError> {
    sheet.set_name(name).map_err(err)?;
    sheet
        .write_string_with_format(0, 0, clip(&a.title), &st.title)
        .map_err(err)?;
    for (col, c) in a.columns.iter().enumerate() {
        sheet
            .write_string_with_format(1, col as u16, clip(&c.header), &st.header)
            .map_err(err)?;
        let width = if c.kind == CellKind::Text { 34.0 } else { 14.0 };
        sheet.set_column_width(col as u16, width).map_err(err)?;
    }
    sheet.set_freeze_panes(2, 0).map_err(err)?;
    let int = Format::new().set_num_format("0");
    let num = Format::new().set_num_format("#,##0.00");
    let pct = Format::new().set_num_format("0.0%");
    let eur = Format::new().set_num_format(currency_format("EUR"));
    let usd = Format::new().set_num_format(currency_format("USD"));
    for (r, cells) in a.rows.iter().enumerate() {
        let row = u32::try_from(r + 2).map_err(|_| CoreError::Xlsx("too many rows".into()))?;
        for (col, (cell, c)) in cells.iter().zip(&a.columns).enumerate() {
            let col = col as u16;
            match cell {
                Value::Null => {}
                Value::Number(n) => {
                    let v = n.as_f64().unwrap_or(0.0);
                    match c.kind {
                        CellKind::Duration => {
                            sheet.write_number_with_format(row, col, duration(v as i64), &st.time)
                        }
                        CellKind::Percent => sheet.write_number_with_format(row, col, v, &pct),
                        CellKind::Int => sheet.write_number_with_format(row, col, v, &int),
                        CellKind::Eur => sheet.write_number_with_format(row, col, v / 100.0, &eur),
                        CellKind::Usd => sheet.write_number_with_format(row, col, v / 100.0, &usd),
                        CellKind::Number | CellKind::Text => {
                            sheet.write_number_with_format(row, col, v, &num)
                        }
                    }
                    .map_err(err)?;
                }
                Value::String(s) => {
                    sheet.write_string(row, col, clip(s)).map_err(err)?;
                }
                other => {
                    sheet
                        .write_string(row, col, clip(&other.to_string()))
                        .map_err(err)?;
                }
            }
        }
    }
    Ok(())
}

/// Corrections, hand entries, billable pauses and time away. Raw times stay in the note.
fn write_adjustments_sheet(
    sheet: &mut Worksheet,
    rows: &[crate::report::AdjustmentRow],
    zone_name: &str,
    tz: Tz,
    st: &Styles,
) -> Result<(), CoreError> {
    sheet.set_name("Adjustments").map_err(err)?;
    sheet
        .write_string(
            0,
            0,
            format!("Changes laid over the recorded time. Times in {zone_name}."),
        )
        .map_err(err)?;
    for (col, (head, width)) in [
        ("Type", 30.0),
        ("Task name", 32.0),
        ("Start", 22.0),
        ("End", 22.0),
        ("Time", 12.0),
        ("Note", 60.0),
    ]
    .iter()
    .enumerate()
    {
        let col = u16::try_from(col).unwrap_or(0);
        sheet
            .write_string_with_format(1, col, *head, &st.header)
            .map_err(err)?;
        sheet.set_column_width(col, *width).map_err(err)?;
    }
    sheet.set_freeze_panes(2, 0).map_err(err)?;
    for (i, r) in rows.iter().enumerate() {
        let row = u32::try_from(i).unwrap_or(u32::MAX - 2) + 2;
        sheet.write_string(row, 0, r.kind).map_err(err)?;
        sheet
            .write_string(row, 1, clip(&r.task_name))
            .map_err(err)?;
        sheet
            .write_string(row, 2, zoned(r.start, tz))
            .map_err(err)?;
        sheet.write_string(row, 3, zoned(r.end, tz)).map_err(err)?;
        sheet
            .write_number_with_format(row, 4, duration(r.end.secs() - r.start.secs()), &st.time)
            .map_err(err)?;
        sheet.write_string(row, 5, clip(&r.note)).map_err(err)?;
    }
    Ok(())
}

/// Builds the workbook bytes.
pub fn build(source: &ExportSource, request: &ExportRequest) -> Result<Vec<u8>, CoreError> {
    let tz: Tz = parse_zone(&request.day_zone)?;
    let narrowed;
    let source = match &request.selection {
        Some(entries) => {
            narrowed = source.narrowed(tz, entries)?;
            &narrowed
        }
        None => source,
    };
    let st = Styles::new();
    let mut wb = Workbook::new();
    write_labor_sheet(wb.add_worksheet(), source, &request.day_zone, tz, &st)?;
    write_details_sheet(wb.add_worksheet(), source, request, &st)?;
    let mut used = vec!["Labor by project".to_owned(), "Segment details".to_owned()];
    let adjustments = source.adjustments();
    if !adjustments.is_empty() {
        used.push("Adjustments".to_owned());
        write_adjustments_sheet(wb.add_worksheet(), &adjustments, &request.day_zone, tz, &st)?;
    }
    if request.include_earnings {
        used.push("Earnings".to_owned());
        write_earnings_sheet(wb.add_worksheet(), source, &request.day_zone, tz, &st)?;
    }
    // A selection's analysis sheets are worked out here from its own entries. Sheets sent by the UI
    // describe everything logged, so they are never used for one.
    let selection_sheets;
    let sheets: &[AnalyticsSheet] = if let Some(count) = source.selected_count() {
        let sheet = wb.add_worksheet();
        sheet.set_name("About this selection").map_err(err)?;
        used.push("About this selection".to_owned());
        sheet
            .write_string(0, 0, format!("This export holds a selection of {count} task-day entries from the Timeline. Time and money on every sheet cover those entries only."))
            .map_err(err)?;
        sheet
            .write_string(1, 0, "The Subtasks column on the first sheet shows each task's full subtask list and ticks as of the export, which can include subtasks worked on days outside the selection.")
            .map_err(err)?;
        sheet
            .write_string(2, 0, "The analysis sheets run from the first to the last selected day and count only the selected entries. Averages per calendar day still count the days between them. Shift figures count the shifts of selected days, and an effective hourly rate on a fixed fee is over the selected time.")
            .map_err(err)?;
        selection_sheets = crate::analytics::selection_sheets(source, &request.day_zone)?;
        &selection_sheets
    } else {
        &request.analytics
    };
    for a in sheets
        .iter()
        .filter(|a| request.include_earnings || !a.contains_prices)
    {
        let name = clean_sheet_name(&a.name, &mut used);
        write_analytics_sheet(wb.add_worksheet(), a, &name, &st)?;
    }
    wb.save_to_buffer().map_err(err)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::billing::tests::{fixture, seg};
    use crate::machine::tests::ctx;
    use crate::machine::{RateInput, TaskInput};
    use crate::report::tests::source;
    use crate::report::{AnalyticsColumn, AnalyticsSheet};
    use serde_json::json;

    pub fn sample() -> (ExportSource, ExportRequest) {
        let (mut s, p, t) = fixture(Some((15, RoundMethod::Nearest)));
        let c = ctx("2026-01-01T00:00:00Z");
        let list = json!({"type":"doc","content":[
            {"type":"paragraph","content":[{"type":"text","text":"prose stays out"}]},
            {"type":"orderedList","content":[
                {"type":"listItem","content":[{"type":"paragraph","content":[{"type":"text","text":"Draft"}]},
                    {"type":"bulletList","content":[{"type":"listItem","content":[{"type":"paragraph","content":[{"type":"text","text":"Outline"}]}]}]}]},
                {"type":"listItem","content":[{"type":"paragraph","content":[{"type":"text","text":"Review"}]}]},
            ]},
        ]});
        s.update_task(
            &c,
            &t,
            &TaskInput {
                name: "Write report".into(),
                project_id: p.clone(),
                description: list,
                continued_from: None,
                tags: Vec::new(),
            },
        )
        .unwrap();
        let evil = s
            .create_task(
                &c,
                &crate::machine::tests::task_input("=HYPERLINK(\"http://x\")", &p),
            )
            .unwrap();
        let mut other = crate::machine::tests::project_input("=Beta");
        other.color = "#fde047".into();
        let q = s.create_project(&c, &other).unwrap();
        let t3 = s
            .create_task(&c, &crate::machine::tests::task_input("Support", &q))
            .unwrap();
        s.set_rate(
            &c,
            &RateInput {
                project_id: p.clone(),
                rate_cents: Some(6000),
                currency: "EUR".into(),
                effective_from: Some("2026-01-01T00:00:00Z".into()),
                confirm_retroactive: true,
            },
        )
        .unwrap();
        s.set_rate(
            &c,
            &RateInput {
                project_id: q,
                rate_cents: Some(8050),
                currency: "USD".into(),
                effective_from: Some("2026-01-01T00:00:00Z".into()),
                confirm_retroactive: true,
            },
        )
        .unwrap();
        // Multi-day through midnight UTC with a pause gap.
        seg(
            &mut s,
            &t,
            "s1",
            "2026-03-20T23:00:00Z",
            "2026-03-21T00:20:00Z",
        );
        seg(
            &mut s,
            &t,
            "s2",
            "2026-03-21T09:00:00Z",
            "2026-03-21T09:40:00Z",
        );
        seg(
            &mut s,
            &evil,
            "s3",
            "2026-03-21T10:00:00Z",
            "2026-03-21T10:03:00Z",
        );
        seg(
            &mut s,
            &t3,
            "s4",
            "2026-03-22T10:00:00Z",
            "2026-03-22T12:07:30Z",
        );
        let src = source(s, "2026-04-01T00:00:00Z");
        let mut shifts = BTreeMap::new();
        shifts.insert("s2".to_owned(), "Morning 0:40:00".to_owned());
        let req = ExportRequest {
            day_zone: "UTC".into(),
            my_zone: "Europe/Lisbon".into(),
            client_zone: Some("America/New_York".into()),
            include_earnings: true,
            segment_shifts: shifts,
            analytics: vec![
                AnalyticsSheet {
                    name: "KPI summary".into(),
                    title: "KPI summary (sample)".into(),
                    columns: vec![
                        AnalyticsColumn {
                            header: "Metric".into(),
                            kind: CellKind::Text,
                        },
                        AnalyticsColumn {
                            header: "Value".into(),
                            kind: CellKind::Duration,
                        },
                    ],
                    rows: vec![vec![json!("Exact total"), json!(14850)]],
                    contains_prices: false,
                },
                AnalyticsSheet {
                    name: "Earnings by month".into(),
                    title: "Earnings by month (sample)".into(),
                    columns: vec![
                        AnalyticsColumn {
                            header: "Month".into(),
                            kind: CellKind::Text,
                        },
                        AnalyticsColumn {
                            header: "EUR".into(),
                            kind: CellKind::Eur,
                        },
                    ],
                    rows: vec![vec![json!("2026-03"), json!(12345)]],
                    contains_prices: true,
                },
            ],
            selection: None,
        };
        (src, req)
    }

    pub fn unzip_all(bytes: &[u8], dir: &std::path::Path) -> String {
        let zip = dir.join("x.xlsx");
        std::fs::write(&zip, bytes).unwrap();
        let out = std::process::Command::new("unzip")
            .arg("-p")
            .arg(&zip)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn contrast_picks_readable_text() {
        assert_eq!(readable_text(0xFFFFFF), 0x000000);
        assert_eq!(readable_text(0x000000), 0xFFFFFF);
        assert_eq!(readable_text(0xFDE047), 0x000000);
        assert_eq!(readable_text(0x1E3A8A), 0xFFFFFF);
    }

    #[test]
    fn builds_with_earnings_and_writes_sample_when_asked() {
        let (src, req) = sample();
        let bytes = build(&src, &req).unwrap();
        assert!(bytes.starts_with(b"PK"));
        if let Ok(path) = std::env::var("ZWT_SAMPLE_XLSX") {
            std::fs::write(path, &bytes).unwrap();
        }
    }

    #[test]
    fn export_without_earnings_has_no_price_data_anywhere() {
        let (src, mut req) = sample();
        req.include_earnings = false;
        let bytes = build(&src, &req).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let all = unzip_all(&bytes, dir.path());
        assert!(all.contains("Labor by project"));
        for needle in [
            "EUR",
            "USD",
            "Earnings",
            "Rate per hour",
            "Exact amount",
            "\u{20ac}",
            "[$$",
            "12345",
            "Amount",
        ] {
            assert!(!all.contains(needle), "found {needle}");
        }
    }

    /// Shift+Enter in a list item is a `hardBreak` in the stored document, and Enter in a paste can make a second
    /// paragraph. The Subtasks column reads each as a space, so an item stays on one line of its cell with its words
    /// apart, and the file is still a sound archive.
    #[test]
    fn a_line_break_or_second_paragraph_in_a_subtask_is_a_space_in_the_subtasks_column() {
        let (mut src, req) = sample();
        let id = src
            .data
            .tasks
            .values()
            .find(|t| t.name == "Write report")
            .map(|t| t.id.clone())
            .unwrap();
        src.data.tasks.get_mut(&id).unwrap().description = json!({"type":"doc","content":[
            {"type":"orderedList","content":[
                {"type":"listItem","content":[{"type":"paragraph","content":[
                    {"type":"text","text":"Call Anna"},{"type":"hardBreak"},{"type":"text","text":"about the invoice"}
                ]}]},
                {"type":"listItem","content":[
                    {"type":"paragraph","content":[{"type":"text","text":"Send"}]},
                    {"type":"paragraph","content":[{"type":"text","text":"the file"}]}
                ]},
            ]},
        ]});
        let bytes = build(&src, &req).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let all = unzip_all(&bytes, dir.path());
        assert!(
            all.contains("1. Call Anna about the invoice\n2. Send the file"),
            "the cell holds one line per item, each with its words apart"
        );
        assert!(!all.contains("Annaabout") && !all.contains("Sendthe"));
        let sound = std::process::Command::new("unzip")
            .arg("-t")
            .arg(dir.path().join("x.xlsx"))
            .output()
            .unwrap();
        assert!(sound.status.success(), "the workbook is a sound archive");
    }

    #[test]
    fn empty_data_still_builds() {
        let (src, mut req) = sample();
        req.analytics.clear();
        let empty = ExportSource {
            only: None,
            data: crate::model::Shared::default(),
            ..src
        };
        assert!(build(&empty, &req).is_ok());
    }

    #[test]
    fn sheet_names_are_cleaned_and_unique() {
        let mut used = vec!["Earnings".to_owned()];
        assert_eq!(clean_sheet_name("earnings", &mut used), "earnings 2");
        assert_eq!(clean_sheet_name("a/b[c]:d*e?f", &mut used), "abcdef");
    }
}
