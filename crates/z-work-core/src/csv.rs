//! CSV export. Text cells are neutralised against spreadsheet formula injection and quoted per RFC 4180.

use std::fmt::Write as _;

use chrono_tz::Tz;

use crate::billing::{cents_from_numerator, split_by_rate};
use crate::error::CoreError;
use crate::pieces::PieceKind;
use crate::report::{ExportRequest, ExportSource, hms, zoned};
use crate::validate::parse_zone;

/// Prefixes an apostrophe when a spreadsheet could read the cell as a formula.
#[must_use]
pub fn neutralize(cell: &str) -> String {
    let first = cell.trim_start_matches([' ', '\u{a0}']).chars().next();
    let dangerous = matches!(
        first,
        Some(
            '=' | '+'
                | '-'
                | '@'
                | '\t'
                | '\r'
                | '\n'
                | '\u{ff1d}'
                | '\u{ff0b}'
                | '\u{ff0d}'
                | '\u{ff20}'
        )
    );
    if dangerous {
        format!("'{cell}")
    } else {
        cell.to_owned()
    }
}

#[must_use]
pub fn quote(cell: &str) -> String {
    // Semicolon and tab too: Excel in some locales splits on them, which could expose a formula.
    if cell.contains(['"', ',', ';', '\t', '\n', '\r']) {
        format!("\"{}\"", cell.replace('"', "\"\""))
    } else {
        cell.to_owned()
    }
}

pub(crate) fn text(cell: &str) -> String {
    quote(&neutralize(cell))
}

fn safe_end(end: Option<crate::time::Ts>, f: impl Fn(crate::time::Ts) -> String) -> String {
    end.map_or_else(String::new, f)
}

/// Segment records hold raw UTC times and exact durations. Task-day records hold the rounded
/// (billable) time for the selected day zone, so the two never mix within a column sum if filtered
/// by `record_type`. Prices appear only when `include_earnings` is set.
pub fn build(source: &ExportSource, request: &ExportRequest) -> Result<String, CoreError> {
    let day_zone: Tz = parse_zone(&request.day_zone)?;
    let my_zone: Tz = parse_zone(&request.my_zone)?;
    let client_zone: Option<Tz> = request.client_zone.as_deref().map(parse_zone).transpose()?;
    let narrowed;
    let source = match &request.selection {
        Some(entries) => {
            narrowed = source.narrowed(day_zone, entries)?;
            &narrowed
        }
        None => source,
    };

    let mut header = vec![
        "record_type".to_owned(),
        "task".to_owned(),
        "project".to_owned(),
        format!("day_{}", request.day_zone),
        "start_utc".to_owned(),
        "end_utc".to_owned(),
        format!("start_my_{}", request.my_zone),
        format!("end_my_{}", request.my_zone),
    ];
    if let Some(z) = &request.client_zone {
        header.push(format!("start_client_{z}"));
        header.push(format!("end_client_{z}"));
    }
    header.push("exact_duration".to_owned());
    header.push("billable_duration".to_owned());
    if request.include_earnings {
        header.extend([
            "rate_per_hour".to_owned(),
            "currency".to_owned(),
            "amount".to_owned(),
        ]);
    }
    header.extend([
        "kind".to_owned(),
        "raw_start_utc".to_owned(),
        "raw_end_utc".to_owned(),
    ]);
    let mut out = String::from("\u{feff}");
    out.push_str(
        &header
            .iter()
            .map(|h| quote(h))
            .collect::<Vec<_>>()
            .join(","),
    );
    out.push_str("\r\n");

    for seg in source.segments() {
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
        for (i, piece) in pieces.iter().enumerate() {
            let shown_end = if i == last { seg.end } else { Some(piece.end) };
            let mut row = vec![
                "segment".to_owned(),
                text(&seg.task_name),
                text(&seg.project_name),
                crate::billing::day_of(piece.start, day_zone).to_string(),
                piece.start.iso(),
                safe_end(shown_end, crate::time::Ts::iso),
                zoned(piece.start, my_zone),
                safe_end(shown_end, |t| zoned(t, my_zone)),
            ];
            if let Some(z) = client_zone {
                row.push(zoned(piece.start, z));
                row.push(safe_end(shown_end, |t| zoned(t, z)));
            }
            let secs = piece.end.secs() - piece.start.secs();
            // A billable pause is billable time only, so it stays out of the exact column.
            if seg.kind == PieceKind::BillablePause {
                row.push(String::new());
                row.push(hms(secs));
            } else {
                row.push(hms(secs));
                row.push(String::new());
            }
            if request.include_earnings {
                match (piece.rate_cents, &piece.currency) {
                    (Some(rate), Some(cur)) => {
                        row.push(format!("{}.{:02}", rate / 100, rate % 100));
                        row.push(cur.clone());
                        let cents = cents_from_numerator(rate * secs);
                        row.push(format!("{}.{:02}", cents / 100, cents % 100));
                    }
                    _ => row.extend([String::new(), String::new(), String::new()]),
                }
            }
            row.push(match seg.kind {
                PieceKind::Work => "work".to_owned(),
                PieceKind::Thinking => "thinking".to_owned(),
                PieceKind::BillablePause => "billable_pause".to_owned(),
            });
            row.push(safe_end(seg.raw_start, crate::time::Ts::iso));
            row.push(safe_end(seg.raw_end, crate::time::Ts::iso));
            out.push_str(&row.join(","));
            out.push_str("\r\n");
        }
    }

    for day_row in source.rows(day_zone) {
        let mut row = vec![
            "task_day".to_owned(),
            text(&day_row.task_name),
            text(&day_row.project_name),
            day_row.row.day.to_string(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        ];
        if client_zone.is_some() {
            row.extend([String::new(), String::new()]);
        }
        row.push(hms(day_row.row.exact_secs));
        row.push(hms(day_row.row.billable_secs));
        if request.include_earnings {
            let numerators = day_row.row.numerators();
            row.push(String::new());
            let mut currencies = Vec::new();
            let mut amounts = Vec::new();
            for (cur, n) in &numerators {
                let cents = cents_from_numerator(*n);
                currencies.push(cur.clone());
                amounts.push(format!("{}.{:02}", cents / 100, cents % 100));
            }
            row.push(quote(&currencies.join(";")));
            row.push(quote(&amounts.join(";")));
        }
        let mut line = String::new();
        row.extend([String::new(), String::new(), String::new()]);
        let _ = write!(line, "{}\r\n", row.join(","));
        out.push_str(&line);
    }
    if let Some(count) = source.selected_count() {
        out.push_str(&quote("selection"));
        out.push(',');
        out.push_str(&quote(&format!(
            "This export holds a selection of {count} task-day entries. Totals cover those only."
        )));
        out.push_str("\r\n");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::billing::tests::{fixture, seg};
    use crate::machine::RateInput;
    use crate::machine::tests::{ctx, task_input};
    use crate::model::RoundMethod;
    use crate::report::tests::source;

    fn request(earn: bool) -> ExportRequest {
        ExportRequest {
            day_zone: "America/New_York".into(),
            my_zone: "Europe/Lisbon".into(),
            client_zone: Some("America/New_York".into()),
            include_earnings: earn,
            segment_shifts: Default::default(),
            analytics: vec![],
            selection: None,
        }
    }

    #[test]
    fn formula_injection_is_neutralised() {
        for cell in [
            "=1+1",
            "+cmd",
            "-2",
            "@SUM(A1)",
            "\tx",
            "\rx",
            " =1+1",
            "\u{a0}@x",
            "\u{ff1d}1",
        ] {
            assert!(neutralize(cell).starts_with('\''), "{cell}");
        }
        for cell in ["plain", "a=b", "10-2", ""] {
            assert_eq!(neutralize(cell), cell);
        }
        assert_eq!(
            text("=HYPERLINK(\"x\",\"y\")"),
            "\"'=HYPERLINK(\"\"x\"\",\"\"y\"\")\""
        );
    }

    #[test]
    fn quoting_follows_rfc_4180() {
        assert_eq!(quote("a,b"), "\"a,b\"");
        assert_eq!(quote("a\"b"), "\"a\"\"b\"");
        assert_eq!(quote("a\nb"), "\"a\nb\"");
        assert_eq!(quote("ab"), "ab");
        assert_eq!(quote("x;=1+1;y"), "\"x;=1+1;y\"");
        assert_eq!(quote("x\t=1"), "\"x\t=1\"");
    }

    #[test]
    fn segments_keep_raw_utc_and_task_days_hold_billable() {
        let (mut s, p, t) = fixture(Some((15, RoundMethod::Nearest)));
        let bad = s
            .create_task(&ctx("2026-01-01T00:00:00Z"), &task_input("=evil()", &p))
            .unwrap();
        seg(
            &mut s,
            &t,
            "a",
            "2026-03-20T14:00:07Z",
            "2026-03-20T14:20:07Z",
        );
        seg(
            &mut s,
            &bad,
            "b",
            "2026-03-20T15:00:00Z",
            "2026-03-20T15:01:00Z",
        );
        let csv = build(&source(s, "2026-04-01T00:00:00Z"), &request(false)).unwrap();
        let lines: Vec<&str> = csv.trim_start_matches('\u{feff}').split("\r\n").collect();
        assert!(lines[0].starts_with(
            "record_type,task,project,day_America/New_York,start_utc,end_utc,start_my_Europe/Lisbon"
        ));
        assert!(lines[1].contains("segment,Write,Alpha,2026-03-20,2026-03-20T14:00:07Z,2026-03-20T14:20:07Z,2026-03-20 14:00:07 +00:00"));
        assert!(
            lines[1].contains("2026-03-20 10:00:07 -04:00"),
            "US on DST, Lisbon not yet: {}",
            lines[1]
        );
        assert!(lines[1].contains(",0:20:00,,work,"));
        assert!(lines[2].contains("'=evil()"));
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("task_day,Write,Alpha,2026-03-20")
                    && l.ends_with(",0:20:00,0:15:00,,,"))
        );
    }

    #[test]
    fn no_price_data_without_earnings() {
        let (mut s, p, t) = fixture(Some((0, RoundMethod::Nearest)));
        s.set_rate(
            &ctx("2026-01-01T00:00:00Z"),
            &RateInput {
                project_id: p,
                rate_cents: Some(7777),
                currency: "EUR".into(),
                effective_from: None,
                confirm_retroactive: false,
            },
        )
        .unwrap();
        seg(
            &mut s,
            &t,
            "a",
            "2026-03-20T14:00:00Z",
            "2026-03-20T15:00:00Z",
        );
        let src = source(s, "2026-04-01T00:00:00Z");
        let plain = build(&src, &request(false)).unwrap();
        assert!(
            !plain.contains("EUR") && !plain.contains("77.77") && !plain.contains("rate_per_hour")
        );
        let priced = build(&src, &request(true)).unwrap();
        assert!(priced.contains("77.77,EUR,77.77"));
    }
}
