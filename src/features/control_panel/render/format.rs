//! Numbers, times and text written out for the status picture.

use chrono::{DateTime, Datelike, Months, NaiveDate, TimeDelta, Utc};

use crate::util::shorten;

/// "reminders" → "Reminders", "control_panel" → "Control panel".
pub(super) fn title(name: &str) -> String {
    let name = name.replace('_', " ");
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => name,
    }
}

pub(super) fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

pub(super) fn format_ms(ms: f64) -> String {
    format!("{ms:.0} ms")
}

pub(super) fn format_mb(mb: f64) -> String {
    format!("{mb:.1} MB")
}

/// 12 → "12", 2.5 → "2.5", 0.123 → "0.12".
pub(super) fn number(n: f64) -> String {
    if n.fract().abs() < 1e-9 {
        format!("{n:.0}")
    } else if n >= 10.0 {
        format!("{n:.1}")
    } else {
        format!("{n:.2}")
    }
}

/// The first day of this month and of the next, in UTC.
pub fn month_bounds(now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    let first = NaiveDate::from_ymd_opt(now.year(), now.month(), 1).expect("valid date");
    let next = first + Months::new(1);
    let midnight = |date: NaiveDate| date.and_hms_opt(0, 0, 0).expect("valid time").and_utc();
    (midnight(first), midnight(next))
}

/// "3d 4h 12m", "4h 12m" or "12m".
pub fn human_duration(duration: TimeDelta) -> String {
    let minutes = duration.num_minutes().max(0);
    let (days, hours, minutes) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    match (days, hours) {
        (0, 0) => format!("{minutes}m"),
        (0, _) => format!("{hours}h {minutes}m"),
        _ => format!("{days}d {hours}h {minutes}m"),
    }
}

/// "in 3h 12m", "12m ago" or "just now".
pub(super) fn relative(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let delta = at - now;
    if delta.num_seconds().abs() < 60 {
        return "just now".to_string();
    }
    // Two units at most: "3d 4h" rather than "3d 4h 12m".
    let short = |d: TimeDelta| {
        let text = human_duration(d);
        let parts: Vec<&str> = text.split(' ').take(2).collect();
        // "9h 0m" reads better as "9h".
        match parts[..] {
            [first, second] if !second.starts_with('0') => format!("{first} {second}"),
            _ => parts[0].to_string(),
        }
    };
    if delta > TimeDelta::zero() {
        format!("in {}", short(delta))
    } else {
        format!("{} ago", short(-delta))
    }
}

/// Discord markup as plain text: timestamps (`<t:1700000000:R>`) written out, and code
/// backticks left out.
pub(super) fn plain(text: &str, now: DateTime<Utc>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("<t:") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 3..];
        let Some(end) = after.find('>') else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let (seconds, style) = after[..end].split_once(':').unwrap_or((&after[..end], "f"));
        match seconds
            .parse::<i64>()
            .ok()
            .and_then(|s| DateTime::from_timestamp(s, 0))
        {
            Some(at) if style == "R" => out.push_str(&relative(at, now)),
            Some(at) => out.push_str(&format!("{} UTC", at.format("%-d %b %H:%M"))),
            None => out.push_str(&rest[start..start + 4 + end]),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out.replace('`', "")
}

/// The error's first lines as one line of text.
pub(super) fn first_lines(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Words into lines of at most `width` characters, at most `max` lines (the last ends in
/// "…" when cut).
pub(super) fn wrap_words(text: &str, width: usize, max: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let word = shorten(word, width);
        match lines.last_mut() {
            Some(line) if line.chars().count() + 1 + word.chars().count() <= width => {
                line.push(' ');
                line.push_str(&word);
            }
            _ => lines.push(word),
        }
    }
    if lines.len() > max {
        lines.truncate(max);
        let last = &mut lines[max - 1];
        *last = format!("{}…", shorten(last, width - 1).trim_end_matches('…'));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text).unwrap().to_utc()
    }

    #[test]
    fn discord_markup_as_text() {
        let now = at("2026-10-10T08:00:00Z");
        let in_2h = now.timestamp() + 2 * 3600 + 5 * 60;
        assert_eq!(plain(&format!("next <t:{in_2h}:R>"), now), "next in 2h 5m");
        assert_eq!(
            plain(&format!("<t:{}:f>", now.timestamp()), now),
            "10 Oct 08:00 UTC"
        );
        assert_eq!(plain("`model` <t:x:R> <t:", now), "model <t:x:R> <t:");
        assert_eq!(
            relative(
                now - TimeDelta::days(3) - TimeDelta::hours(4) - TimeDelta::minutes(9),
                now
            ),
            "3d 4h ago"
        );
        assert_eq!(relative(now + TimeDelta::seconds(20), now), "just now");
    }

    #[test]
    fn durations() {
        assert_eq!(human_duration(TimeDelta::minutes(12)), "12m");
        assert_eq!(human_duration(TimeDelta::minutes(4 * 60 + 12)), "4h 12m");
        assert_eq!(
            human_duration(TimeDelta::minutes(3 * 1440 + 4 * 60 + 12)),
            "3d 4h 12m"
        );
        assert_eq!(human_duration(TimeDelta::seconds(-5)), "0m");
    }

    #[test]
    fn small_helpers() {
        assert_eq!(
            wrap_words("one two three four", 9, 4),
            ["one two", "three", "four"]
        );
        assert_eq!(
            wrap_words("one two three four", 9, 2),
            ["one two", "three…"]
        );
        let (start, end) = month_bounds(at("2026-12-31T23:00:00Z"));
        assert_eq!(
            (start, end),
            (at("2026-12-01T00:00:00Z"), at("2027-01-01T00:00:00Z"))
        );
        assert_eq!(title("control_panel"), "Control panel");
        assert_eq!(number(12.0), "12");
        assert_eq!(number(2.5), "2.50");
    }
}
