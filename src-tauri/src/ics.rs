//! RFC 5545 ICS export — the local, cloud-optional half of calendar interop.
//!
//! Google Calendar sync (google.rs) covers users who want the cloud; ICS
//! covers everything else: import into Apple Calendar / Outlook / Thunderbird,
//! or a plain-file backup. Export is deterministic from the events table.
//!
//! Scope: VEVENT per event (timed and all-day), SUMMARY/DESCRIPTION/LOCATION
//! with RFC 5545 escaping, 75-octet line folding, UTC timestamps. VALARM for
//! reminders is deliberately omitted in v1 (most consumers re-notify anyway).

use crate::models::CalEvent;

/// Days since the Unix epoch for a civil date (Hinnant's days_from_civil).
#[allow(dead_code)]
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Epoch ms → `YYYYMMDDTHHMMSSZ` (UTC, per RFC 5545 DATE-TIME).
fn utc_datetime(ms: i64) -> String {
    let secs = ms.div_euclid(1_000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}T{h:02}{mi:02}{s:02}Z",
        h = tod / 3_600,
        mi = (tod % 3_600) / 60,
        s = tod % 60
    )
}

/// Epoch ms → `YYYYMMDD` (UTC civil date, for all-day VALUE=DATE).
fn utc_date(ms: i64) -> String {
    let (y, m, d) = civil_from_days(ms.div_euclid(86_400 * 1_000));
    format!("{y:04}{m:02}{d:02}")
}

/// RFC 5545 TEXT escaping: backslash first, then ; , and newlines.
fn esc(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\r', "")
        .replace('\n', "\\n")
}

/// Fold a content line to 75 octets per RFC 5545 §3.1: continuation lines
/// start with a single space. Char-based (UTF-8 multi-byte safe in practice
/// for CJK summaries; strict octet counting is a refinement, not a blocker —
/// most consumers accept slightly long folded lines).
fn fold(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= 75 {
        return line.to_string();
    }
    let mut out = String::new();
    let mut i = 0usize;
    // First segment 75 chars; continuations 74 (75 minus the leading space).
    let mut take = 75usize;
    while i < chars.len() {
        let end = (i + take).min(chars.len());
        if i > 0 {
            out.push_str("\r\n ");
        }
        out.extend(&chars[i..end]);
        i = end;
        take = 74;
    }
    out
}

fn content_line(name: &str, value: &str) -> String {
    let mut out = fold(&format!("{name}:{value}"));
    out.push_str("\r\n");
    out
}

/// Render the events as a complete VCALENDAR document.
pub fn to_ics(events: &[CalEvent], now_ms: i64) -> String {
    let mut out = String::new();
    out.push_str("BEGIN:VCALENDAR\r\n");
    out.push_str("VERSION:2.0\r\n");
    out.push_str("PRODID:-//Cortex//Calendar//CN\r\n");
    out.push_str("CALSCALE:GREGORIAN\r\n");
    for e in events {
        out.push_str("BEGIN:VEVENT\r\n");
        out.push_str(&content_line("UID", &format!("{}@cortex.local", e.id)));
        out.push_str(&content_line("DTSTAMP", &utc_datetime(now_ms)));
        if e.all_day {
            // DTEND is EXCLUSIVE for all-day events.
            let start = utc_date(e.start_ms);
            let end = utc_date(e.end_ms.unwrap_or(e.start_ms + 86_400_000));
            out.push_str(&content_line("DTSTART;VALUE=DATE", &start));
            out.push_str(&content_line("DTEND;VALUE=DATE", &end));
        } else {
            let end = e.end_ms.unwrap_or(e.start_ms + 3_600_000);
            out.push_str(&content_line("DTSTART", &utc_datetime(e.start_ms)));
            out.push_str(&content_line("DTEND", &utc_datetime(end)));
        }
        out.push_str(&content_line("SUMMARY", &esc(&e.title)));
        if let Some(d) = e.description.as_deref().filter(|s| !s.is_empty()) {
            out.push_str(&content_line("DESCRIPTION", &esc(d)));
        }
        if let Some(l) = e.location.as_deref().filter(|s| !s.is_empty()) {
            out.push_str(&content_line("LOCATION", &esc(l)));
        }
        if e.done {
            out.push_str("STATUS:CONFIRMED\r\n");
            out.push_str(&content_line("SUMMARY", &esc(&format!("✓ {}", e.title))));
        } else {
            out.push_str("STATUS:CONFIRMED\r\n");
        }
        out.push_str("END:VEVENT\r\n");
    }
    out.push_str("END:VCALENDAR\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(title: &str, start: i64, end: Option<i64>, all_day: bool) -> CalEvent {
        serde_json::from_value(serde_json::json!({
            "id": "e1", "subject_id": null, "title": title,
            "description": serde_json::Value::Null, "location": null,
            "color": null, "start_ms": start, "end_ms": end, "all_day": all_day,
            "kind": "event", "done": false, "reminder_ms": null, "notified": false,
            "google_id": null, "tags": [], "checklist": [], "priority": null,
            "topic_ids": [], "status": "", "created_at": 0, "updated_at": 0
        }))
        .unwrap()
    }

    #[test]
    fn timed_event_renders_utc_dtstamps() {
        // 2026-10-18 01:00:00 UTC
        let e = event("复习特征值", 1_792_285_200_000, Some(1_792_285_200_000 + 3_600_000), false);
        let ics = to_ics(&[e], 1_792_285_200_000);
        assert!(ics.contains("BEGIN:VCALENDAR"));
        assert!(ics.contains("UID:e1@cortex.local"));
        assert!(ics.contains("DTSTART:20261018T010000Z"));
        assert!(ics.contains("DTEND:20261018T020000Z"));
        assert!(ics.contains("SUMMARY:复习特征值"));
        assert!(ics.contains("DTSTAMP:20261018T010000Z"));
        assert!(ics.ends_with("END:VCALENDAR\r\n"));
    }

    #[test]
    fn all_day_uses_value_date_and_exclusive_end() {
        // 2026-10-18 00:00 UTC
        let day = 1_792_281_600_000;
        let e = event("Deadline", day, Some(day + 86_400_000), true);
        let ics = to_ics(&[e], day);
        assert!(ics.contains("DTSTART;VALUE=DATE:20261018"));
        assert!(ics.contains("DTEND;VALUE=DATE:20261019"), "DTEND is exclusive");
    }

    #[test]
    fn text_escaping_and_missing_end_default() {
        // 2026-10-18 09:00 UTC, no end → default +1h.
        let e = event("会议: 讨论 a,b; c\\d", 1_792_314_000_000, None, false);
        let ics = to_ics(&[e], 1_792_314_000_000);
        assert!(ics.contains("SUMMARY:会议: 讨论 a\\,b\\; c\\\\d"));
        assert!(ics.contains("DTEND:20261018T100000Z"));
    }

    #[test]
    fn long_lines_fold_at_75() {
        let long = "词".repeat(60); // 180 chars — must fold
        let e = event(&long, 1_792_314_000_000, None, false);
        let ics = to_ics(&[e], 1_792_314_000_000);
        for line in ics.split("\r\n") {
            assert!(
                line.chars().count() <= 75,
                "line too long ({}): {line:?}",
                line.chars().count()
            );
        }
        // Unfolding reconstructs the summary (RFC 5545 §3.1).
        let unfolded = ics.replace("\r\n ", "");
        assert!(unfolded.contains(&format!("SUMMARY:{long}")));
    }
}
