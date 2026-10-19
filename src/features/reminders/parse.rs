//! The reminder time grammar, written with the `winnow` parser-combinator crate.
//!
//! Everything here is pure: text goes in, a time comes out. Nothing touches Discord or the
//! database, so the tests at the bottom run without either.
//!
//! The grammar is built from small parsers that each understand one thing (`duration`,
//! `clock`, `day`, `zone`) and are combined into `when`. Each small parser is a plain
//! function `fn(&mut &str) -> ModalResult<T>`: it reads from the front of the input, moves
//! the input forward past what it read, and returns what it found.

use chrono::{
    DateTime, Datelike, Days, Months, NaiveDate, NaiveTime, TimeDelta, TimeZone, Utc, Weekday,
};
use chrono_tz::Tz;
use winnow::ModalResult;
use winnow::ascii::{Caseless, dec_uint, space0, space1};
use winnow::combinator::{alt, cut_err, fail, not, opt, peek, preceded, repeat, terminated};
use winnow::error::{ContextError, ErrMode, StrContext, StrContextValue};
use winnow::prelude::*;
use winnow::token::{literal, one_of, take_while};

/// A time expression as the user wrote it. [`resolve`] turns it into an actual instant.
#[derive(Debug, Clone, PartialEq)]
pub enum When {
    /// "in 2h30m", "in 1 week and 2 days"
    In(Vec<(u32, Unit)>),
    /// "at 16:30 CET", "tomorrow at 3pm", "next friday", "on 2026-12-24 at noon"
    At {
        day: Option<Day>,
        time: Option<NaiveTime>,
        zone: Option<Tz>,
    },
    /// "every day at 9:00", "every weekday at 8:30 CET", "every monday", "every 2 hours"
    Every(Repeat),
}

/// How a recurring reminder repeats.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Repeat {
    pub rule: Rule,
    /// The clock time of a day rule ("every monday at 9:30"); 09:00 when not given.
    pub time: Option<NaiveTime>,
    /// The zone the clock time and calendar days are in; the user's own when not given.
    pub zone: Option<Tz>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Rule {
    /// "every day"
    Daily,
    /// "every weekday": Monday to Friday.
    Weekdays,
    /// "every monday"
    Weekly(Weekday),
    /// "every 2 hours", "every week": this long after the last one. Hours or longer.
    Interval(u32, Unit),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Unit {
    Year,
    Month,
    Week,
    Day,
    Hour,
    Minute,
    Second,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Day {
    Today,
    Tomorrow,
    /// "next week": seven days from today.
    NextWeek,
    /// `next: true` for "next friday", which never means today.
    Weekday {
        day: Weekday,
        next: bool,
    },
    Date(NaiveDate),
    /// "on the 15th": the next 15th, this month or next.
    Monthly(u32),
    /// "march 3", "3rd of march", "24.12": the next such date, this year or next.
    Yearly {
        month: u32,
        day: u32,
    },
}

/// A reminder message split into its time and its text.
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub when: When,
    pub message: String,
}

/// Splits "in 2h check the oven" or "check the oven in 2h" into a time and a message.
///
/// The time is looked for at the start first, then at the end. On failure the error is a
/// sentence that can be shown to the user as is.
pub fn parse_reminder(text: &str) -> Result<Parsed, String> {
    let text = text.trim();

    // 1. The time comes first: "in 2h check the oven".
    let mut input = text;
    let leading_error = match when.parse_next(&mut input) {
        Ok(when) => {
            let parsed = day_then_time(when, input);
            // "in 2 weeks on friday …", "every monday and wednesday …": the rest of the
            // schedule would silently become the message. Better to say so and let chat
            // sort it out.
            if let Some(more) = more_schedule(&parsed.message) {
                return Err(format!(
                    "That has more than one time in it (\"{more}\"). Which one do you mean?"
                ));
            }
            return Ok(parsed);
        }
        Err(err) => (text.len() - input.len(), err),
    };

    // 2. The time comes last: "check the oven in 2h".
    if let Some((start, when)) = time_at_end(text) {
        return Ok(Parsed {
            when,
            message: clean_message(&text[..start]),
        });
    }

    // 3. Neither worked. If the start looked like a time ("in ...", "at ..."), say where it
    //    went wrong; otherwise there was no time at all.
    let (offset, err) = leading_error;
    match err {
        ErrMode::Cut(err) => {
            let word = text[offset..]
                .split_whitespace()
                .next()
                .unwrap_or("the end");
            Err(format!(
                "I got stuck at \"{word}\": I expected {}.",
                expected(&err)
            ))
        }
        _ => Err("I couldn't find a time in that.".to_string()),
    }
}

/// The time was at the start and `rest` is the message after it. For "tomorrow call mom at
/// 5pm" the start only gave a day, so the time at the end of `rest` is used with it; for
/// "at 5pm call mom tomorrow" the start only gave a time, so the day at the end is.
fn day_then_time(when: When, rest: &str) -> Parsed {
    if let When::At {
        day: Some(day),
        time: None,
        ..
    } = when
        && let Some((
            start,
            When::At {
                day: None,
                time,
                zone,
            },
        )) = time_at_end(rest)
    {
        return Parsed {
            when: When::At {
                day: Some(day),
                time,
                zone,
            },
            message: clean_message(&rest[..start]),
        };
    }
    if let When::At {
        day: None,
        time: Some(time),
        zone,
    } = when
        && let Some((start, day)) = day_at_end(rest)
    {
        return Parsed {
            when: When::At {
                day: Some(day),
                time: Some(time),
                zone,
            },
            message: clean_message(&rest[..start]),
        };
    }
    Parsed {
        when,
        message: clean_message(rest),
    }
}

/// A day on its own at the end of `text` ("call mom tomorrow"): where it starts, and the day.
fn day_at_end(text: &str) -> Option<(usize, Day)> {
    for start in word_starts(text) {
        let mut tail = &text[start..];
        if let Ok(day) = day.parse_next(&mut tail)
            && tail.trim_end_matches(['.', '!', '?', ' ']).is_empty()
        {
            return Some((start, day));
        }
    }
    None
}

/// The schedule words at the start of a message that should have been part of the time:
/// "on friday …", "and wednesday …", "evening …". Returns them for the error message.
fn more_schedule(message: &str) -> Option<&str> {
    let mut input = message;
    let found = alt((
        (word("on"), space1, day).take(),
        (word("and"), space1, weekday).take(),
        (opt(in_the), day_part).take(),
    ))
    .parse_next(&mut input)
    .ok()?;
    Some(found.trim_end())
}

/// Where each word after the first starts.
fn word_starts(text: &str) -> impl Iterator<Item = usize> + '_ {
    text.char_indices()
        .filter(|(_, c)| c.is_whitespace())
        .map(|(i, c)| i + c.len_utf8())
}

/// Finds a time at the end of `text`: tries every word as the start of the time, from the
/// left, and takes the first one that reads all the way to the end. Returns where the time
/// starts and the time.
fn time_at_end(text: &str) -> Option<(usize, When)> {
    for start in word_starts(text) {
        let mut tail = &text[start..];
        if let Ok(when) = when.parse_next(&mut tail)
            && tail.trim_end_matches(['.', '!', '?', ' ']).is_empty()
        {
            return Some((start, when));
        }
    }
    None
}

/// Reads a time on its own, like "in 2h" or "tomorrow at 9am". Used by the chat tool, where
/// the model passes the time and the message separately.
pub fn parse_when(text: &str) -> Result<When, String> {
    let mut input = text.trim();
    match when.parse_next(&mut input) {
        Ok(when) if input.trim_end_matches(['.', '!', '?', ' ']).is_empty() => Ok(when),
        Ok(_) => Err(format!(
            "I didn't understand \"{}\" after the time.",
            input.trim()
        )),
        Err(ErrMode::Cut(err)) => Err(format!("I expected {}.", expected(&err))),
        Err(_) => Err("That isn't a time I understand.".to_string()),
    }
}

/// Turns a parsed time into an instant. `now` is the current time and `home` is the
/// user's timezone, used when the text doesn't name one.
pub fn resolve(when: &When, now: DateTime<Utc>, home: Tz) -> Result<DateTime<Utc>, String> {
    let too_far = || "That's too far in the future.".to_string();
    match when {
        When::Every(repeat) => next_repeat(repeat, now, home),
        When::In(parts) => {
            let mut t = now;
            for &(n, unit) in parts {
                t = add(t, n, unit, home).ok_or_else(too_far)?;
            }
            Ok(t)
        }
        When::At { day, time, zone } => {
            let zone = zone.unwrap_or(home);
            let today = now.with_timezone(&zone).date_naive();
            let time = time.unwrap_or(NINE_AM);
            let date = match day {
                // "at 15:00": today, or tomorrow if 15:00 has already passed.
                None => {
                    let t = to_utc(zone, today, time)?;
                    if t > now {
                        return Ok(t);
                    }
                    today.succ_opt().ok_or_else(too_far)?
                }
                Some(Day::Today) => today,
                Some(Day::Tomorrow) => today.succ_opt().ok_or_else(too_far)?,
                Some(Day::NextWeek) => today + Days::new(7),
                // The next 15th: this month's if its time is still ahead, else next month's.
                Some(Day::Monthly(day)) => {
                    let candidate = |months: u32| {
                        today
                            .checked_add_months(Months::new(months))
                            .and_then(|d| d.with_day(*day))
                    };
                    let this_month = candidate(0).filter(|&date| {
                        date > today
                            || (date == today && to_utc(zone, date, time).is_ok_and(|t| t > now))
                    });
                    match this_month.or_else(|| candidate(1)) {
                        Some(date) => date,
                        None => return Err(format!("No month has a day {day}.")),
                    }
                }
                // The next March 3: this year's if still ahead, else next year's.
                Some(Day::Yearly { month, day }) => {
                    let candidate =
                        |years: i32| NaiveDate::from_ymd_opt(today.year() + years, *month, *day);
                    let this_year = candidate(0).filter(|&date| {
                        date > today
                            || (date == today && to_utc(zone, date, time).is_ok_and(|t| t > now))
                    });
                    match this_year.or_else(|| candidate(1)) {
                        Some(date) => date,
                        None => return Err(format!("There's no day {day} in that month.")),
                    }
                }
                Some(Day::Weekday { day, next }) => {
                    let ahead = (7 + day.num_days_from_monday()
                        - today.weekday().num_days_from_monday())
                        % 7;
                    let date = today + Days::new(ahead.into());
                    // "friday" on a Friday means today if the time is still ahead.
                    // "next friday" on a Friday means a week from now.
                    if ahead == 0 && (*next || to_utc(zone, date, time)? <= now) {
                        date + Days::new(7)
                    } else {
                        date
                    }
                }
                Some(Day::Date(date)) => *date,
            };
            let t = to_utc(zone, date, time)?;
            if t <= now {
                return Err(format!("<t:{}:f> has already passed.", t.timestamp()));
            }
            Ok(t)
        }
    }
}

/// The first time `repeat` comes up after `after`. `home` is the user's timezone, used
/// when the repeat doesn't name one.
pub fn next_repeat(
    repeat: &Repeat,
    after: DateTime<Utc>,
    home: Tz,
) -> Result<DateTime<Utc>, String> {
    let zone = repeat.zone.unwrap_or(home);
    let too_far = || "That's too far in the future.".to_string();
    let wanted = |day: Weekday| match repeat.rule {
        Rule::Daily => true,
        Rule::Weekdays => day.num_days_from_monday() < 5,
        Rule::Weekly(weekday) => day == weekday,
        Rule::Interval(..) => false,
    };
    if let Rule::Interval(n, unit) = repeat.rule {
        return add(after, n, unit, zone).ok_or_else(too_far);
    }
    // A day rule: the first wanted day, from the day of `after`, whose time is still ahead.
    // Eight days always hold one.
    let time = repeat.time.unwrap_or(NINE_AM);
    let first = after.with_timezone(&zone).date_naive();
    for ahead in 0..8 {
        let date = first + Days::new(ahead);
        if wanted(date.weekday()) {
            let t = to_utc(zone, date, time)?;
            if t > after {
                return Ok(t);
            }
        }
    }
    Err(too_far())
}

/// How a repeat is saved and shown: "every monday at 09:00 Europe/Oslo", "every 2 hours".
/// The zone (`home` when the repeat named none) is written out, so later changes to the
/// user's timezone don't move it, and [`parse_repeat`] reads it back.
pub fn describe_repeat(repeat: &Repeat, home: Tz) -> String {
    let zone = repeat.zone.unwrap_or(home);
    let time = repeat.time.unwrap_or(NINE_AM).format("%-H:%M");
    let days = match repeat.rule {
        Rule::Daily => "day".to_string(),
        Rule::Weekdays => "weekday".to_string(),
        Rule::Weekly(day) => weekday_name(day).to_string(),
        Rule::Interval(n, unit) => {
            let name = match unit {
                Unit::Year => "year",
                Unit::Month => "month",
                Unit::Week => "week",
                Unit::Day => "day",
                Unit::Hour | Unit::Minute | Unit::Second => "hour",
            };
            let every = match n {
                1 => format!("every {name}"),
                n => format!("every {n} {name}s"),
            };
            // Hours are exact; days and longer follow the calendar in the zone.
            return match unit {
                Unit::Hour | Unit::Minute | Unit::Second => every,
                _ => format!("{every} {zone}"),
            };
        }
    };
    format!("every {days} at {time} {zone}")
}

/// Reads a repeat saved by [`describe_repeat`].
pub fn parse_repeat(text: &str) -> Option<Repeat> {
    match parse_when(text) {
        Ok(When::Every(repeat)) => Some(repeat),
        _ => None,
    }
}

fn weekday_name(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "monday",
        Weekday::Tue => "tuesday",
        Weekday::Wed => "wednesday",
        Weekday::Thu => "thursday",
        Weekday::Fri => "friday",
        Weekday::Sat => "saturday",
        Weekday::Sun => "sunday",
    }
}

/// Looks up a timezone by IANA name ("Europe/Oslo") or a common abbreviation ("CET").
///
/// Abbreviations map to a real place, so summer time is handled: "CET" in July is UTC+2.
/// Single words that aren't abbreviations are rejected on purpose, because tz names like
/// "Turkey" or "Japan" are also ordinary words in a reminder.
pub fn lookup_zone(name: &str) -> Option<Tz> {
    // Two letters are only a zone in capitals, so "at 5pm et voilà" stays in the user's own
    // zone while "5pm ET" means New York.
    if name.len() == 2 && name != name.to_ascii_uppercase() {
        return None;
    }
    let zone = match name.to_ascii_uppercase().as_str() {
        "UTC" | "GMT" | "Z" => Tz::UTC,
        "EST" | "EDT" | "ET" => Tz::America__New_York,
        "CST" | "CDT" | "CT" => Tz::America__Chicago,
        "MST" | "MDT" | "MT" => Tz::America__Denver,
        "PST" | "PDT" | "PT" => Tz::America__Los_Angeles,
        "AKST" | "AKDT" => Tz::America__Anchorage,
        "HST" => Tz::Pacific__Honolulu,
        "WET" | "WEST" => Tz::Europe__Lisbon,
        "CET" | "CEST" => Tz::Europe__Paris,
        "EET" | "EEST" => Tz::Europe__Helsinki,
        "BST" => Tz::Europe__London,
        "MSK" => Tz::Europe__Moscow,
        "SAST" => Tz::Africa__Johannesburg,
        "HKT" => Tz::Asia__Hong_Kong,
        "SGT" => Tz::Asia__Singapore,
        "JST" => Tz::Asia__Tokyo,
        "KST" => Tz::Asia__Seoul,
        "AEST" | "AEDT" => Tz::Australia__Sydney,
        "NZST" | "NZDT" => Tz::Pacific__Auckland,
        _ if name.contains('/') => return Tz::from_str_insensitive(name).ok(),
        _ => return None,
    };
    Some(zone)
}

/// The abbreviations [`lookup_zone`] knows, for `/timezone`'s suggestions.
pub const ZONE_ABBREVIATIONS: &[&str] = &[
    "UTC", "GMT", "ET", "EST", "EDT", "CT", "CST", "CDT", "MT", "MST", "MDT", "PT", "PST", "PDT",
    "AKST", "HST", "WET", "CET", "CEST", "EET", "BST", "MSK", "SAST", "HKT", "SGT", "JST", "KST",
    "AEST", "AEDT", "NZST",
];

/// How a saved repeat reads to a person: "every day at 9:00", with the zone only when it
/// isn't the viewer's own ("every day at 9:00 in Europe/Oslo").
pub fn repeat_for_display(saved: &str, viewer: Tz) -> String {
    let Some(repeat) = parse_repeat(saved) else {
        return saved.to_string();
    };
    let zone = repeat.zone.unwrap_or(viewer);
    let full = describe_repeat(&repeat, viewer);
    // `describe_repeat` ends with the zone (except for hourly repeats): take it off.
    let plain = full
        .strip_suffix(zone.name())
        .map_or(full.as_str(), str::trim_end);
    if zone == viewer {
        plain.to_string()
    } else {
        format!("{plain} in {}", zone.name())
    }
}

// ---------------------------------------------------------------------------------------
// The grammar
// ---------------------------------------------------------------------------------------

const NINE_AM: NaiveTime = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
/// "tonight", "night": 20:00.
const EVENING_LATE: NaiveTime = NaiveTime::from_hms_opt(20, 0, 0).unwrap();

/// The whole time expression.
fn when(input: &mut &str) -> ModalResult<When> {
    alt((
        in_duration,
        at_time,
        every,
        shorthand_repeat,
        tonight,
        on_day,
    ))
    .parse_next(input)
}

/// "daily", "daily at 9:00", "weekly", "monthly", "hourly": "every day/week/month/hour" in
/// one word. Only "daily" takes a clock; the others count from now, like "every week".
fn shorthand_repeat(input: &mut &str) -> ModalResult<When> {
    let rule = alt((
        word("daily").value(Rule::Daily),
        word("weekly").value(Rule::Interval(1, Unit::Week)),
        word("monthly").value(Rule::Interval(1, Unit::Month)),
        word("hourly").value(Rule::Interval(1, Unit::Hour)),
    ))
    .parse_next(input)?;
    let time = day_clock.parse_next(input)?;
    if time.is_some() && rule != Rule::Daily {
        return refuse(
            input,
            "a repeat with a day for that time, like every monday at 10:00",
        );
    }
    let zone = opt(preceded(space1, zone)).parse_next(input)?;
    Ok(When::Every(Repeat { rule, time, zone }))
}

/// "tonight": today at 20:00.
fn tonight(input: &mut &str) -> ModalResult<When> {
    word("tonight").parse_next(input)?;
    let zone = opt(preceded(space1, zone)).parse_next(input)?;
    Ok(When::At {
        day: Some(Day::Today),
        time: Some(EVENING_LATE),
        zone,
    })
}

/// Stop parsing with a hint. Used where a phrase is understood well enough to know it
/// can't be done, so the user gets a reason instead of a generic "couldn't read that".
fn refuse<T>(input: &mut &str, hint: &'static str) -> ModalResult<T> {
    cut_err(fail).context(describe(hint)).parse_next(input)
}

/// "every day at 9:00", "every weekday 8:30 CET", "every monday", "every 2 hours",
/// "every 3 days Europe/Oslo"
fn every(input: &mut &str) -> ModalResult<When> {
    (word("every"), space1).parse_next(input)?;
    // "every other monday" would need a rule the scheduler doesn't have (a weekday every
    // second week), so it's refused with a hint rather than read as every Monday.
    if peek((word("other"), space1, weekday))
        .parse_next(input)
        .is_ok()
    {
        return refuse(
            input,
            "a repeat I can do: every other week works, every other monday doesn't yet",
        );
    }
    let (rule, time) = cut_err(alt((
        (repeat_days, day_clock),
        repeat_interval.map(|rule| (rule, None)),
    )))
    .context(describe(
        "a repeat like every day at 9:00, every monday or every 2 hours",
    ))
    .parse_next(input)?;
    // "every monday and wednesday": one weekday per reminder, so say so.
    if peek((space1, word("and"), space1, weekday))
        .parse_next(input)
        .is_ok()
    {
        return refuse(
            input,
            "one weekday per repeat: set a second reminder for the other day",
        );
    }
    let zone = opt(preceded(space1, zone)).parse_next(input)?;
    Ok(When::Every(Repeat { rule, time, zone }))
}

/// "day", "weekday", "weekend" (Saturday), "monday"
fn repeat_days(input: &mut &str) -> ModalResult<Rule> {
    alt((
        word("day").value(Rule::Daily),
        any_word(&["weekdays", "weekday"]).value(Rule::Weekdays),
        any_word(&["weekends", "weekend"]).value(Rule::Weekly(Weekday::Sat)),
        weekday.map(Rule::Weekly),
    ))
    .parse_next(input)
}

/// The optional clock after a day or a repeat: " at 9:30", " 9:30", " morning". A bare
/// hour ("tomorrow at 7") is an error rather than nothing, so "7" isn't silently 09:00 with
/// "at 7" left in the message.
fn day_clock(input: &mut &str) -> ModalResult<Option<NaiveTime>> {
    let time = opt(preceded(
        space1,
        alt((
            preceded(opt((word("at"), space1)), clock),
            preceded(opt(in_the), day_part),
        )),
    ))
    .parse_next(input)?;
    if time.is_none()
        && peek((
            space1,
            word("at"),
            space1,
            take_while(1..=2, |c: char| c.is_ascii_digit()),
        ))
        .parse_next(input)
        .is_ok()
    {
        return refuse(
            input,
            "a clock time with minutes or am/pm, like 7:00 or 7pm",
        );
    }
    Ok(time)
}

/// "in the " before a part of the day: "tomorrow in the evening".
fn in_the(input: &mut &str) -> ModalResult<()> {
    (word("in"), space1, word("the"), space1)
        .void()
        .parse_next(input)
}

/// "morning" (09:00), "afternoon" (14:00), "evening" (18:00), "night" (20:00).
fn day_part(input: &mut &str) -> ModalResult<NaiveTime> {
    alt((
        word("morning").value(NINE_AM),
        word("afternoon").value(NaiveTime::from_hms_opt(14, 0, 0).unwrap()),
        word("evening").value(NaiveTime::from_hms_opt(18, 0, 0).unwrap()),
        word("night").value(EVENING_LATE),
    ))
    .parse_next(input)
}

/// "2 hours", "hour", "3 days", "week", "other week" (2 weeks). Minutes and seconds are
/// refused: a reminder every few minutes would only be noise.
fn repeat_interval(input: &mut &str) -> ModalResult<Rule> {
    let n: Option<u32> =
        opt(terminated(alt((dec_uint, word("other").value(2))), space0)).parse_next(input)?;
    let unit = unit.parse_next(input)?;
    match (n.unwrap_or(1), unit) {
        (0, _) | (_, Unit::Minute | Unit::Second) => backtrack(),
        (n, unit) => Ok(Rule::Interval(n, unit)),
    }
}

/// "in 2h30m", "in 1 week and 2 days"
fn in_duration(input: &mut &str) -> ModalResult<When> {
    (word("in"), space1).parse_next(input)?;
    // After "in" there must be a duration. `cut_err` stops `alt` from trying the other
    // forms, so the error points at the word that isn't a duration.
    let parts = cut_err(duration)
        .context(describe("a duration like 2h30m or 3 days"))
        .parse_next(input)?;
    Ok(When::In(parts))
}

/// "at 16:30", "at 3pm CET", "at 2026-03-01 15:04 EST", "at 9:00 tomorrow"
fn at_time(input: &mut &str) -> ModalResult<When> {
    (word("at"), space1).parse_next(input)?;
    let (date, time) = cut_err(alt((
        (date, space1, opt((word("at"), space1)), clock).map(|(d, _, _, t)| (Some(d), t)),
        clock.map(|t| (None, t)),
    )))
    .context(describe("a time like 16:30, 3pm or noon"))
    .parse_next(input)?;
    let zone = opt(preceded(space1, zone)).parse_next(input)?;
    let day = match date {
        Some(date) => Some(Day::Date(date)),
        None => opt(preceded(space1, day)).parse_next(input)?,
    };
    Ok(When::At {
        day,
        time: Some(time),
        zone,
    })
}

/// "tomorrow", "friday at 3pm", "next friday 9:30 CET", "on 2026-12-24 at noon"
fn on_day(input: &mut &str) -> ModalResult<When> {
    let day = day.parse_next(input)?;
    let time = day_clock.parse_next(input)?;
    let zone = match time {
        Some(_) => opt(preceded(space1, zone)).parse_next(input)?,
        None => None,
    };
    Ok(When::At {
        day: Some(day),
        time,
        zone,
    })
}

/// One or more "<number> <unit>" parts, optionally separated by spaces, commas or "and".
fn duration(input: &mut &str) -> ModalResult<Vec<(u32, Unit)>> {
    let separator = (space0, opt((alt((",", word("and"))), space0)));
    let parts: Vec<Vec<(u32, Unit)>> =
        repeat(1.., preceded(separator, duration_part)).parse_next(input)?;
    Ok(parts.into_iter().flatten().collect())
}

/// "2h", "30 minutes", "an hour", "half an hour", "1.5 hours", "1h30", "an hour and a half".
/// Fractions become the next smaller unit, so the parts stay whole numbers.
fn duration_part(input: &mut &str) -> ModalResult<Vec<(u32, Unit)>> {
    // "half an hour", "half a day"
    // `opt` puts the input back where it was when the words aren't there.
    if opt((word("half"), space1, any_word(&["an", "a"]), space1))
        .parse_next(input)?
        .is_some()
    {
        let unit = unit.parse_next(input)?;
        return halve(unit).map(|part| vec![part]);
    }
    let n: u32 = alt((dec_uint, any_word(&["an", "a"]).value(1))).parse_next(input)?;
    let tenths: Option<u32> = opt(preceded(
        '.',
        take_while(1, |c: char| c.is_ascii_digit()).parse_to(),
    ))
    .parse_next(input)?;
    space0.parse_next(input)?;
    let unit = unit.parse_next(input)?;
    let mut parts = vec![(n, unit)];
    if let Some(tenths) = tenths.filter(|&t| t > 0) {
        let (per, smaller) = smaller_unit(unit)?;
        parts.push((per * tenths / 10, smaller));
    }
    // "1h30": minutes glued to the hours.
    if unit == Unit::Hour
        && let Some(minutes) = opt(terminated(
            two_digits,
            not(one_of(|c: char| c.is_alphanumeric() || c == ':')),
        ))
        .parse_next(input)?
    {
        parts.push((minutes, Unit::Minute));
    }
    // "an hour and a half"
    if opt((space1, word("and"), space1, word("a"), space1, word("half")))
        .parse_next(input)?
        .is_some()
    {
        parts.push(halve(unit)?);
    }
    Ok(parts)
}

/// How many of the next smaller unit make one of `unit`: an hour is 60 minutes.
fn smaller_unit(unit: Unit) -> ModalResult<(u32, Unit)> {
    Ok(match unit {
        Unit::Week => (7, Unit::Day),
        Unit::Day => (24, Unit::Hour),
        Unit::Hour => (60, Unit::Minute),
        Unit::Minute => (60, Unit::Second),
        _ => return backtrack(),
    })
}

/// Half of `unit` in the next smaller one: half an hour is 30 minutes.
fn halve(unit: Unit) -> ModalResult<(u32, Unit)> {
    let (per, smaller) = smaller_unit(unit)?;
    Ok((per / 2, smaller))
}

fn unit(input: &mut &str) -> ModalResult<Unit> {
    // Longer spellings come first, so "mins" isn't read as "m" followed by "ins".
    alt((
        any_word(&["years", "year", "yrs", "yr", "y"]).value(Unit::Year),
        any_word(&["months", "month", "mos", "mo"]).value(Unit::Month),
        any_word(&["weeks", "week", "wks", "wk", "w"]).value(Unit::Week),
        any_word(&["days", "day", "d"]).value(Unit::Day),
        any_word(&["hours", "hour", "hrs", "hr", "h"]).value(Unit::Hour),
        any_word(&["minutes", "minute", "mins", "min", "m"]).value(Unit::Minute),
        any_word(&["seconds", "second", "secs", "sec", "s"]).value(Unit::Second),
    ))
    .parse_next(input)
}

/// "16:30", "16:30:15", "3pm", "3:30 pm", "noon", "midnight"
fn clock(input: &mut &str) -> ModalResult<NaiveTime> {
    alt((
        word("noon").value(NaiveTime::from_hms_opt(12, 0, 0).unwrap()),
        word("midnight").value(NaiveTime::MIN),
        numeric_clock,
    ))
    .parse_next(input)
}

fn numeric_clock(input: &mut &str) -> ModalResult<NaiveTime> {
    // One or two digits, so "08:30" works too (`dec_uint` stops after a leading 0).
    let hour: u32 = take_while(1..=2, |c: char| c.is_ascii_digit())
        .parse_to()
        .parse_next(input)?;
    let minute = opt(preceded(':', two_digits)).parse_next(input)?;
    let second = match minute {
        Some(_) => opt(preceded(':', two_digits)).parse_next(input)?,
        None => None,
    };
    let pm = opt(preceded(space0, any_word(&["am", "pm"])))
        .map(|m| m.map(|m: &str| m.eq_ignore_ascii_case("pm")))
        .parse_next(input)?;

    // A bare number like "at 5" is too vague: it needs minutes or am/pm.
    if minute.is_none() && pm.is_none() {
        return backtrack();
    }
    let hour = match pm {
        None => hour,
        Some(_) if !(1..=12).contains(&hour) => return backtrack(),
        Some(true) => hour % 12 + 12,
        Some(false) => hour % 12,
    };
    match NaiveTime::from_hms_opt(hour, minute.unwrap_or(0), second.unwrap_or(0)) {
        Some(time) => Ok(time),
        None => backtrack(),
    }
}

/// "today", "tomorrow", "friday", "this friday", "next friday", "next week", "on friday",
/// "2026-12-24", "on 2026-12-24", "on the 15th", "on march 3", "3rd of march", "24.12"
fn day(input: &mut &str) -> ModalResult<Day> {
    alt((
        word("today").value(Day::Today),
        word("tomorrow").value(Day::Tomorrow),
        (word("next"), space1, word("week")).value(Day::NextWeek),
        preceded((word("next"), space1), weekday).map(|day| Day::Weekday { day, next: true }),
        preceded((word("this"), space1), weekday).map(|day| Day::Weekday { day, next: false }),
        preceded(
            (word("on"), space1),
            alt((date.map(Day::Date), calendar_day, plain_weekday)),
        ),
        date.map(Day::Date),
        calendar_day,
        plain_weekday,
    ))
    .parse_next(input)
}

/// A date without a year: "the 15th", "15th", "march 3", "3rd of march", "3 march", "24.12".
fn calendar_day(input: &mut &str) -> ModalResult<Day> {
    alt((
        // "24.12", "24.12.2026"
        (
            day_number,
            '.',
            month_number,
            opt(preceded('.', year_number)),
        )
            .verify_map(|(day, _, month, year)| match year {
                Some(year) => NaiveDate::from_ymd_opt(year, month, day).map(Day::Date),
                None => Some(Day::Yearly { month, day }),
            }),
        // "march 3", "march 3rd"
        (month_name, space1, ordinal).map(|(month, _, day)| Day::Yearly { month, day }),
        // "3rd of march", "3 march"
        (ordinal, space1, opt((word("of"), space1)), month_name)
            .map(|(day, _, _, month)| Day::Yearly { month, day }),
        // "the 15th", "15th" (not a bare number: "5 things" isn't a date)
        preceded(opt((word("the"), space1)), ordinal_only).map(Day::Monthly),
    ))
    .parse_next(input)
}

/// "3", "3rd", "15th": a day of the month, 1 to 31.
fn ordinal(input: &mut &str) -> ModalResult<u32> {
    let day = day_number.parse_next(input)?;
    opt(any_word(&["st", "nd", "rd", "th"])).parse_next(input)?;
    Ok(day)
}

/// "3rd", "15th": like [`ordinal`], but the suffix is required.
fn ordinal_only(input: &mut &str) -> ModalResult<u32> {
    let day = day_number.parse_next(input)?;
    any_word(&["st", "nd", "rd", "th"]).parse_next(input)?;
    Ok(day)
}

fn day_number(input: &mut &str) -> ModalResult<u32> {
    let n: u32 = take_while(1..=2, |c: char| c.is_ascii_digit())
        .parse_to()
        .parse_next(input)?;
    if (1..=31).contains(&n) {
        Ok(n)
    } else {
        backtrack()
    }
}

fn month_number(input: &mut &str) -> ModalResult<u32> {
    let n: u32 = take_while(1..=2, |c: char| c.is_ascii_digit())
        .parse_to()
        .parse_next(input)?;
    if (1..=12).contains(&n) {
        Ok(n)
    } else {
        backtrack()
    }
}

fn year_number(input: &mut &str) -> ModalResult<i32> {
    take_while(4, |c: char| c.is_ascii_digit())
        .parse_to()
        .parse_next(input)
}

/// "january", "jan", … as 1 to 12.
fn month_name(input: &mut &str) -> ModalResult<u32> {
    let name = any_word(&[
        "january",
        "jan",
        "february",
        "feb",
        "march",
        "mar",
        "april",
        "apr",
        "may",
        "june",
        "jun",
        "july",
        "jul",
        "august",
        "aug",
        "september",
        "sept",
        "sep",
        "october",
        "oct",
        "november",
        "nov",
        "december",
        "dec",
    ])
    .parse_next(input)?;
    Ok(match name[..3].to_ascii_lowercase().as_str() {
        "jan" => 1,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        _ => 12,
    })
}

fn plain_weekday(input: &mut &str) -> ModalResult<Day> {
    weekday
        .map(|day| Day::Weekday { day, next: false })
        .parse_next(input)
}

fn weekday(input: &mut &str) -> ModalResult<Weekday> {
    let name = any_word(&[
        "monday",
        "mon",
        "tuesday",
        "tues",
        "tue",
        "wednesday",
        "wed",
        "thursday",
        "thurs",
        "thur",
        "thu",
        "friday",
        "fri",
        "saturday",
        "sat",
        "sunday",
        "sun",
    ])
    .parse_next(input)?;
    // The first three letters are enough to tell the days apart.
    let day = match name[..3].to_ascii_lowercase().as_str() {
        "mon" => Weekday::Mon,
        "tue" => Weekday::Tue,
        "wed" => Weekday::Wed,
        "thu" => Weekday::Thu,
        "fri" => Weekday::Fri,
        "sat" => Weekday::Sat,
        _ => Weekday::Sun,
    };
    Ok(day)
}

/// "2026-12-24"
fn date(input: &mut &str) -> ModalResult<NaiveDate> {
    let year = take_while(4, |c: char| c.is_ascii_digit())
        .parse_to()
        .parse_next(input)?;
    let month = preceded('-', two_digits).parse_next(input)?;
    let day = preceded('-', two_digits).parse_next(input)?;
    match NaiveDate::from_ymd_opt(year, month, day) {
        Some(date) => Ok(date),
        None => backtrack(),
    }
}

/// "CET", "Europe/Oslo"
fn zone(input: &mut &str) -> ModalResult<Tz> {
    let name = take_while(1.., |c: char| {
        c.is_ascii_alphanumeric() || "/_+-".contains(c)
    })
    .parse_next(input)?;
    match lookup_zone(name) {
        Some(zone) => Ok(zone),
        None => backtrack(),
    }
}

// ---------------------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------------------

/// Matches `w` ignoring case, as a whole word: "in" matches "in 2h" but not "inside".
/// Digits may follow, so "h" still matches in "2h30m". An apostrophe counts as part of the
/// word, so "tomorrow's standup" isn't read as "tomorrow".
fn word<'s>(w: &'static str) -> impl Parser<&'s str, &'s str, ErrMode<ContextError>> {
    terminated(
        literal(Caseless(w)),
        not(one_of(|c: char| c.is_alphabetic() || c == '\'' || c == '’')),
    )
}

/// The first of `words` that matches, tried in order.
fn any_word<'s>(
    words: &'static [&'static str],
) -> impl Parser<&'s str, &'s str, ErrMode<ContextError>> {
    move |input: &mut &'s str| {
        for w in words {
            let start = *input;
            if let Ok(found) = word(w).parse_next(input) {
                return Ok(found);
            }
            *input = start;
        }
        backtrack()
    }
}

fn two_digits(input: &mut &str) -> ModalResult<u32> {
    take_while(2, |c: char| c.is_ascii_digit())
        .parse_to()
        .parse_next(input)
}

/// "No match here, try something else."
fn backtrack<T>() -> ModalResult<T> {
    Err(ErrMode::Backtrack(ContextError::new()))
}

fn describe(what: &'static str) -> StrContext {
    StrContext::Expected(StrContextValue::Description(what))
}

/// The description attached with `.context(describe(...))`, for error messages.
fn expected(err: &ContextError) -> &'static str {
    err.context()
        .find_map(|c| match c {
            StrContext::Expected(StrContextValue::Description(d)) => Some(*d),
            _ => None,
        })
        .unwrap_or("a time")
}

/// Strips the glue around a message: "in 2h: to check the oven" becomes "check the oven",
/// and "in 2h and call mom" becomes "call mom".
fn clean_message(s: &str) -> String {
    let s = s.trim().trim_start_matches([':', '-', ',']).trim_start();
    let s = strip_word(s, "and ");
    let s = strip_word(s, "to ");
    s.trim()
        .trim_end_matches([':', '-', ','])
        .trim_end()
        .to_string()
}

/// `s` without `word` at its start, ignoring case.
fn strip_word<'s>(s: &'s str, word: &str) -> &'s str {
    match s.get(..word.len()) {
        Some(start) if start.eq_ignore_ascii_case(word) => &s[word.len()..],
        _ => s,
    }
}

/// Adds `n` units to `t`. Years, months, weeks and days move the date on the calendar in
/// `zone` and keep the clock time there, so summer time doesn't shift the hour and "in 1
/// month" on Jan 31 in Tokyo is Feb 28 in Tokyo. Hours, minutes and seconds are exact.
fn add(t: DateTime<Utc>, n: u32, unit: Unit, zone: Tz) -> Option<DateTime<Utc>> {
    let n64 = i64::from(n);
    let local = t.with_timezone(&zone).naive_local();
    let moved = match unit {
        Unit::Year => local.checked_add_months(Months::new(n.checked_mul(12)?)),
        Unit::Month => local.checked_add_months(Months::new(n)),
        Unit::Week => local.checked_add_days(Days::new(u64::from(n) * 7)),
        Unit::Day => local.checked_add_days(Days::new(n.into())),
        Unit::Hour => return t.checked_add_signed(TimeDelta::try_hours(n64)?),
        Unit::Minute => return t.checked_add_signed(TimeDelta::try_minutes(n64)?),
        Unit::Second => return t.checked_add_signed(TimeDelta::try_seconds(n64)?),
    }?;
    to_utc(zone, moved.date(), moved.time()).ok()
}

/// A wall-clock time in `zone` as UTC. When summer time skips that hour, the hour after
/// is used; when the hour happens twice, the first one is used.
fn to_utc(zone: Tz, date: NaiveDate, time: NaiveTime) -> Result<DateTime<Utc>, String> {
    let local = date.and_time(time);
    zone.from_local_datetime(&local)
        .earliest()
        .or_else(|| {
            zone.from_local_datetime(&(local + TimeDelta::hours(1)))
                .earliest()
        })
        .map(|t| t.with_timezone(&Utc))
        .ok_or_else(|| format!("{local} doesn't exist in {zone}."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_on_its_own() {
        assert!(matches!(parse_when("in 2h"), Ok(When::In(_))));
        assert!(matches!(
            parse_when(" tomorrow at 9am "),
            Ok(When::At { .. })
        ));
        assert!(parse_when("in 2h to cook").is_err());
        assert!(parse_when("in soon").is_err());
        assert!(parse_when("whenever").is_err());
    }

    /// Sunday 2026-02-22 12:00 UTC, the same reference time as the Go tests.
    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 2, 22, 12, 0, 0).unwrap()
    }

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    /// Parses and resolves with UTC as the user's timezone.
    fn run(text: &str) -> (DateTime<Utc>, String) {
        run_in(text, now(), Tz::UTC)
    }

    fn run_in(text: &str, now: DateTime<Utc>, home: Tz) -> (DateTime<Utc>, String) {
        let parsed = parse_reminder(text).unwrap_or_else(|e| panic!("{text:?}: {e}"));
        let at = resolve(&parsed.when, now, home).unwrap_or_else(|e| panic!("{text:?}: {e}"));
        (at, parsed.message)
    }

    // Ported from voltgpt's parse_test.go.
    #[test]
    fn relative_times() {
        let cases = [
            (
                "in 2h to check the oven",
                TimeDelta::hours(2),
                "check the oven",
            ),
            (
                "in 30m: buy groceries",
                TimeDelta::minutes(30),
                "buy groceries",
            ),
            ("in 2h30m meeting", TimeDelta::minutes(150), "meeting"),
            ("in 1 hour dentist", TimeDelta::hours(1), "dentist"),
            ("in 30 minutes call mom", TimeDelta::minutes(30), "call mom"),
            ("in 7 days workout", TimeDelta::days(7), "workout"),
            ("in 1y update CV", TimeDelta::days(365), "update CV"),
            ("in 2 years tax", TimeDelta::days(730), "tax"),
            // New in Vivy.
            ("in 1 week 2 days stretch", TimeDelta::days(9), "stretch"),
            (
                "in 1 week and 2 days stretch",
                TimeDelta::days(9),
                "stretch",
            ),
            ("in 1h, 15 mins tea", TimeDelta::minutes(75), "tea"),
            ("in 2h and call mom", TimeDelta::hours(2), "call mom"),
            ("in 2h and to call mom", TimeDelta::hours(2), "call mom"),
            ("IN 2H shout", TimeDelta::hours(2), "shout"),
            ("in 3 months renew", TimeDelta::days(89), "renew"),
        ];
        for (text, delta, message) in cases {
            let (at, msg) = run(text);
            assert_eq!(at - now(), delta, "{text}");
            assert_eq!(msg, message, "{text}");
        }
    }

    // Ported from voltgpt's parse_test.go.
    #[test]
    fn absolute_times() {
        assert_eq!(
            run("at 2026-03-01 15:04 check it"),
            (utc(2026, 3, 1, 15, 4), "check it".into())
        );
        assert_eq!(
            run("at 2026-03-01 15:04 EST check it"),
            (utc(2026, 3, 1, 20, 4), "check it".into())
        );
        // Still ahead today.
        assert_eq!(run("at 15:00 reminder").0, utc(2026, 2, 22, 15, 0));
        // Already passed today, so tomorrow.
        assert_eq!(run("at 10:00 reminder").0, utc(2026, 2, 23, 10, 0));
    }

    #[test]
    fn clock_forms() {
        assert_eq!(run("at 3pm x").0, utc(2026, 2, 22, 15, 0));
        assert_eq!(run("at 3:30 pm x").0, utc(2026, 2, 22, 15, 30));
        assert_eq!(
            run("at 12:30:15 x").0,
            utc(2026, 2, 22, 12, 30) + TimeDelta::seconds(15)
        );
        assert_eq!(run("at noon x").0, utc(2026, 2, 23, 12, 0));
        assert_eq!(run("at midnight x").0, utc(2026, 2, 23, 0, 0));
        assert_eq!(run("at 12am x").0, utc(2026, 2, 23, 0, 0));
        assert_eq!(run("at 12pm x").0, utc(2026, 2, 23, 12, 0));
        // A leading zero.
        assert_eq!(run("at 08:30 x").0, utc(2026, 2, 23, 8, 30));
        assert_eq!(run("at 00:05 x").0, utc(2026, 2, 23, 0, 5));
    }

    #[test]
    fn days() {
        assert_eq!(
            run("tomorrow at 3pm call mom"),
            (utc(2026, 2, 23, 15, 0), "call mom".into())
        );
        assert_eq!(run("tomorrow").0, utc(2026, 2, 23, 9, 0));
        assert_eq!(
            run("friday dentist"),
            (utc(2026, 2, 27, 9, 0), "dentist".into())
        );
        assert_eq!(run("on fri dentist").0, utc(2026, 2, 27, 9, 0));
        assert_eq!(run("at 9:00 friday x").0, utc(2026, 2, 27, 9, 0));
        // Today is Sunday: plain "sunday" is today while the time is ahead...
        assert_eq!(run("sunday at 13:00 x").0, utc(2026, 2, 22, 13, 0));
        // ...and next week once it has passed.
        assert_eq!(run("sunday at 11:00 x").0, utc(2026, 3, 1, 11, 0));
        // "next sunday" is never today.
        assert_eq!(run("next sunday at 13:00 x").0, utc(2026, 3, 1, 13, 0));
        assert_eq!(run("next monday x").0, utc(2026, 2, 23, 9, 0));
        assert_eq!(
            run("on 2026-12-24 at 18:00 CET christmas"),
            (utc(2026, 12, 24, 17, 0), "christmas".into())
        );
        assert_eq!(run("2026-12-24 x").0, utc(2026, 12, 24, 9, 0));
    }

    #[test]
    fn day_first_time_last() {
        assert_eq!(
            run("tomorrow call mom at 5pm"),
            (utc(2026, 2, 23, 17, 0), "call mom".into())
        );
        assert_eq!(
            run("friday dentist at 3:30pm CET."),
            (utc(2026, 2, 27, 14, 30), "dentist".into())
        );
        // A time in the middle of the message stays in the message.
        assert_eq!(
            run("tomorrow at 3pm is the deadline"),
            (utc(2026, 2, 23, 15, 0), "is the deadline".into())
        );
        assert_eq!(
            run("tomorrow meet at 5pm friday"),
            (utc(2026, 2, 23, 9, 0), "meet at 5pm friday".into())
        );
    }

    #[test]
    fn apostrophe_ends_no_word() {
        assert_eq!(
            run("tomorrow's standup at 3pm"),
            (utc(2026, 2, 22, 15, 0), "tomorrow's standup".into())
        );
        assert_eq!(run("friday’s game at 3pm").1, "friday’s game");
    }

    #[test]
    fn calendar_units_follow_the_users_zone() {
        // Jan 31 00:00 in Tokyo is Jan 30 in UTC; a month later is Feb 28 in Tokyo.
        let tokyo_jan_31 = utc(2026, 1, 30, 15, 0);
        assert_eq!(
            run_in("in 1 month x", tokyo_jan_31, Tz::Asia__Tokyo).0,
            utc(2026, 2, 27, 15, 0)
        );
        // New York moves to summer time on Mar 8: "in 1 day" keeps 12:00 on the clock,
        // "in 24h" doesn't.
        let ny_noon = utc(2026, 3, 7, 17, 0);
        assert_eq!(
            run_in("in 1 day x", ny_noon, Tz::America__New_York).0,
            utc(2026, 3, 8, 16, 0)
        );
        assert_eq!(
            run_in("in 1 week x", ny_noon, Tz::America__New_York).0,
            utc(2026, 3, 14, 16, 0)
        );
        assert_eq!(
            run_in("in 24h x", ny_noon, Tz::America__New_York).0,
            utc(2026, 3, 8, 17, 0)
        );
    }

    #[test]
    fn zones() {
        assert_eq!(run("at 16:30 Europe/Oslo x").0, utc(2026, 2, 22, 15, 30));
        assert_eq!(run("at 16:30 europe/oslo x").0, utc(2026, 2, 22, 15, 30));
        // CET follows summer time, unlike Go's fixed offsets.
        let july = utc(2026, 7, 1, 8, 0);
        assert_eq!(
            run_in("at 16:30 CET x", july, Tz::UTC).0,
            utc(2026, 7, 1, 14, 30)
        );
        // Without a zone in the text, the user's own zone is used.
        assert_eq!(
            run_in("at 15:00 x", now(), Tz::America__New_York).0,
            utc(2026, 2, 22, 20, 0)
        );
        // Words that happen to be tz names stay part of the message.
        assert_eq!(run("at 5pm turkey in the oven").1, "turkey in the oven");
    }

    #[test]
    fn time_at_the_end() {
        assert_eq!(
            run("to check the oven in 2h"),
            (now() + TimeDelta::hours(2), "check the oven".into())
        );
        assert_eq!(
            run("call mom tomorrow at 3pm"),
            (utc(2026, 2, 23, 15, 0), "call mom".into())
        );
        assert_eq!(
            run("meet at the station at 5pm."),
            (utc(2026, 2, 22, 17, 0), "meet at the station".into())
        );
    }

    #[test]
    fn errors() {
        let err = parse_reminder("something").unwrap_err();
        assert_eq!(err, "I couldn't find a time in that.");

        let err = parse_reminder("in nothing").unwrap_err();
        assert!(err.contains("\"nothing\""), "{err}");
        assert!(err.contains("duration"), "{err}");

        let err = parse_reminder("at invalid-date here").unwrap_err();
        assert!(err.contains("\"invalid-date\""), "{err}");

        // "5 dogs" is not a duration.
        assert!(parse_reminder("in 5 dogs").is_err());
        // A bare hour is too vague.
        assert!(parse_reminder("at 5 o'clock").is_err());

        let past = parse_reminder("on 2020-01-01 x").unwrap();
        assert!(resolve(&past.when, now(), Tz::UTC).is_err());
    }

    /// Parses a repeat and lists its next few times after `now`, in UTC.
    fn repeats(text: &str, home: Tz, count: usize) -> (Vec<DateTime<Utc>>, String) {
        let parsed = parse_reminder(text).unwrap_or_else(|e| panic!("{text:?}: {e}"));
        let When::Every(repeat) = parsed.when else {
            panic!("{text:?} isn't a repeat");
        };
        let mut times = Vec::new();
        let mut at = now();
        for _ in 0..count {
            at = next_repeat(&repeat, at, home).unwrap();
            times.push(at);
        }
        (times, parsed.message)
    }

    #[test]
    fn recurring_days() {
        // now() is Sunday 2026-02-22 12:00 UTC.
        assert_eq!(
            repeats("every day at 15:00 water the plants", Tz::UTC, 2),
            (
                vec![utc(2026, 2, 22, 15, 0), utc(2026, 2, 23, 15, 0)],
                "water the plants".into()
            )
        );
        // 09:00 when no time is given; today's has passed.
        assert_eq!(
            repeats("every day x", Tz::UTC, 1).0,
            [utc(2026, 2, 23, 9, 0)]
        );
        // Weekdays skip Saturday and Sunday.
        assert_eq!(
            repeats("every weekday at 8:30 standup", Tz::UTC, 6).0,
            [
                utc(2026, 2, 23, 8, 30),
                utc(2026, 2, 24, 8, 30),
                utc(2026, 2, 25, 8, 30),
                utc(2026, 2, 26, 8, 30),
                utc(2026, 2, 27, 8, 30),
                utc(2026, 3, 2, 8, 30),
            ]
        );
        assert_eq!(
            repeats("every monday at 9am take out the trash", Tz::UTC, 2),
            (
                vec![utc(2026, 2, 23, 9, 0), utc(2026, 3, 2, 9, 0)],
                "take out the trash".into()
            )
        );
        // A Sunday rule on a Sunday: today while the time is ahead.
        assert_eq!(
            repeats("every sun at 13:00 x", Tz::UTC, 1).0,
            [utc(2026, 2, 22, 13, 0)]
        );
        // At the end of the message, with a zone, in the user's zone.
        assert_eq!(
            repeats("stretch every day at 18:00 CET", Tz::UTC, 1),
            (vec![utc(2026, 2, 22, 17, 0)], "stretch".into())
        );
        assert_eq!(
            repeats("every day at 15:00 x", Tz::America__New_York, 1).0,
            [utc(2026, 2, 22, 20, 0)]
        );
    }

    #[test]
    fn recurring_days_keep_the_clock_through_summer_time() {
        // New York moves to summer time on Mar 8: 09:00 stays 09:00 there.
        let (times, _) = repeats("every day at 9:00 America/New_York x", Tz::UTC, 16);
        // 07:00 in New York now, so today's 09:00 is first.
        assert_eq!(times[0], utc(2026, 2, 22, 14, 0));
        assert_eq!(times[14], utc(2026, 3, 8, 13, 0));
        assert_eq!(times[15], utc(2026, 3, 9, 13, 0));
    }

    #[test]
    fn recurring_intervals() {
        assert_eq!(
            repeats("every 2 hours drink water", Tz::UTC, 2),
            (
                vec![now() + TimeDelta::hours(2), now() + TimeDelta::hours(4)],
                "drink water".into()
            )
        );
        assert_eq!(
            repeats("every hour x", Tz::UTC, 1).0,
            [now() + TimeDelta::hours(1)]
        );
        assert_eq!(
            repeats("every 3 days x", Tz::UTC, 1).0,
            [now() + TimeDelta::days(3)]
        );
        assert_eq!(
            repeats("every week x", Tz::UTC, 1).0,
            [now() + TimeDelta::days(7)]
        );
        // Too often, or never.
        let err = parse_reminder("every 5 minutes x").unwrap_err();
        assert!(err.contains("every day at 9:00"), "{err}");
        assert!(parse_reminder("every 0 hours x").is_err());
        assert!(parse_reminder("every now and then").is_err());
        // "everyone" is not "every".
        assert!(parse_reminder("everyone at 5pm").is_ok_and(|p| matches!(p.when, When::At { .. })));
    }

    #[test]
    fn repeats_are_saved_as_text() {
        for (text, saved) in [
            ("every day x", "every day at 9:00 Europe/Oslo"),
            (
                "every weekday at 08:30 x",
                "every weekday at 8:30 Europe/Oslo",
            ),
            (
                "every fri at 5pm CET x",
                "every friday at 17:00 Europe/Paris",
            ),
            ("every 2h x", "every 2 hours"),
            ("every hour x", "every hour"),
            ("every 3 days x", "every 3 days Europe/Oslo"),
            ("every month x", "every month Europe/Oslo"),
        ] {
            let parsed = parse_reminder(text).unwrap();
            let When::Every(repeat) = parsed.when else {
                panic!("{text:?}");
            };
            let described = describe_repeat(&repeat, Tz::Europe__Oslo);
            assert_eq!(described, saved, "{text}");
            // Read back, it means the same, now with the zone written out.
            let back = parse_repeat(&described)
                .unwrap_or_else(|| panic!("{described:?}: {:?}", parse_when(&described)));
            assert_eq!(describe_repeat(&back, Tz::UTC), saved, "{text}");
        }
        assert_eq!(parse_repeat("in 2h"), None);
    }

    #[test]
    fn zone_lookup() {
        assert_eq!(lookup_zone("cest"), Some(Tz::Europe__Paris));
        assert_eq!(lookup_zone("America/New_York"), Some(Tz::America__New_York));
        assert_eq!(lookup_zone("Turkey"), None);
        assert_eq!(lookup_zone("Mars/Base"), None);
    }
    #[test]
    fn plainer_durations() {
        let cases = [
            ("in an hour tea", TimeDelta::hours(1), "tea"),
            ("in a minute tea", TimeDelta::minutes(1), "tea"),
            ("in half an hour tea", TimeDelta::minutes(30), "tea"),
            ("in 1.5 hours tea", TimeDelta::minutes(90), "tea"),
            ("in 1h30 tea", TimeDelta::minutes(90), "tea"),
            ("in an hour and a half tea", TimeDelta::minutes(90), "tea"),
            ("in 2.5 days tea", TimeDelta::hours(60), "tea"),
            ("tea in half an hour", TimeDelta::minutes(30), "tea"),
        ];
        for (text, delta, message) in cases {
            let (at, msg) = run(text);
            assert_eq!(at - now(), delta, "{text}");
            assert_eq!(msg, message, "{text}");
        }
    }

    #[test]
    fn plainer_days() {
        // now() is Sunday 2026-02-22 12:00 UTC.
        let cases = [
            ("tomorrow evening dinner", utc(2026, 2, 23, 18, 0), "dinner"),
            ("tomorrow morning run", utc(2026, 2, 23, 9, 0), "run"),
            (
                "tomorrow in the afternoon nap",
                utc(2026, 2, 23, 14, 0),
                "nap",
            ),
            ("tonight game", utc(2026, 2, 22, 20, 0), "game"),
            ("game tonight", utc(2026, 2, 22, 20, 0), "game"),
            ("this friday party", utc(2026, 2, 27, 9, 0), "party"),
            ("next week review", utc(2026, 3, 1, 9, 0), "review"),
            (
                "next week at 10:00 review",
                utc(2026, 3, 1, 10, 0),
                "review",
            ),
            ("on the 15th rent", utc(2026, 3, 15, 9, 0), "rent"),
            ("15th rent", utc(2026, 3, 15, 9, 0), "rent"),
            ("march 3 dentist", utc(2026, 3, 3, 9, 0), "dentist"),
            (
                "on march 3rd at 2pm dentist",
                utc(2026, 3, 3, 14, 0),
                "dentist",
            ),
            ("3rd of march dentist", utc(2026, 3, 3, 9, 0), "dentist"),
            ("3 mar dentist", utc(2026, 3, 3, 9, 0), "dentist"),
            ("24.12 presents", utc(2026, 12, 24, 9, 0), "presents"),
            ("24.12.2027 presents", utc(2027, 12, 24, 9, 0), "presents"),
            // A date that has passed this year rolls to next year.
            ("january 5 taxes", utc(2027, 1, 5, 9, 0), "taxes"),
            // Time first, day last.
            (
                "at 5pm call mom tomorrow",
                utc(2026, 2, 23, 17, 0),
                "call mom",
            ),
            (
                "at 9:00 standup on monday",
                utc(2026, 2, 23, 9, 0),
                "standup",
            ),
        ];
        for (text, at, message) in cases {
            assert_eq!(run(text), (at, message.into()), "{text}");
        }
        // "5 things" is not a date, and neither is "at 10am CT scan" a scan in Chicago.
        assert_eq!(run("5 things to do tomorrow").1, "5 things to do");
    }

    #[test]
    fn plainer_repeats() {
        let (times, msg) = repeats("daily stretch", Tz::UTC, 2);
        assert_eq!(times, [utc(2026, 2, 23, 9, 0), utc(2026, 2, 24, 9, 0)]);
        assert_eq!(msg, "stretch");

        let (times, _) = repeats("daily at 10:00 report", Tz::UTC, 1);
        assert_eq!(times, [utc(2026, 2, 23, 10, 0)]);

        let (times, _) = repeats("weekly report", Tz::UTC, 1);
        assert_eq!(times, [utc(2026, 3, 1, 12, 0)]);

        let (times, _) = repeats("monthly rent", Tz::UTC, 1);
        assert_eq!(times, [utc(2026, 3, 22, 12, 0)]);

        let (times, _) = repeats("every other week laundry", Tz::UTC, 1);
        assert_eq!(times, [utc(2026, 3, 8, 12, 0)]);

        let (times, _) = repeats("every weekend sleep in", Tz::UTC, 1);
        assert_eq!(times, [utc(2026, 2, 28, 9, 0)]);

        let (times, _) = repeats("every day in the evening walk", Tz::UTC, 1);
        assert_eq!(times, [utc(2026, 2, 22, 18, 0)]);
    }

    #[test]
    fn plainer_errors() {
        let err = parse_reminder("tomorrow at 7 call").unwrap_err();
        assert!(err.contains("7:00 or 7pm"), "{err}");

        let err = parse_reminder("every monday and wednesday standup").unwrap_err();
        assert!(err.contains("one weekday"), "{err}");

        let err = parse_reminder("every other monday standup").unwrap_err();
        assert!(err.contains("every other week"), "{err}");

        let err = parse_reminder("weekly at 10:00 report").unwrap_err();
        assert!(err.contains("every monday at 10:00"), "{err}");

        let err = parse_reminder("in 2 weeks on friday review").unwrap_err();
        assert!(err.contains("more than one time"), "{err}");
        assert!(err.contains("\"on friday\""), "{err}");

        // Not a date: 31.02 doesn't exist and 5 isn't a day on its own.
        assert!(parse_reminder("31.02.2026 nope").is_err());
        assert_eq!(
            parse_reminder("5 nope").unwrap_err(),
            "I couldn't find a time in that."
        );
    }

    #[test]
    fn repeats_read_in_the_viewers_zone() {
        let oslo = Tz::Europe__Oslo;
        assert_eq!(
            repeat_for_display("every day at 9:00 Europe/Oslo", oslo),
            "every day at 9:00"
        );
        assert_eq!(
            repeat_for_display("every day at 9:00 Europe/Oslo", Tz::UTC),
            "every day at 9:00 in Europe/Oslo"
        );
        assert_eq!(repeat_for_display("every 2 hours", oslo), "every 2 hours");
        assert_eq!(
            repeat_for_display("every 3 days Europe/Oslo", oslo),
            "every 3 days"
        );
        assert_eq!(repeat_for_display("not a repeat", oslo), "not a repeat");
    }

    #[test]
    fn two_letter_zones_need_capitals() {
        assert_eq!(lookup_zone("CT"), Some(Tz::America__Chicago));
        assert_eq!(lookup_zone("ct"), None);
        assert_eq!(lookup_zone("et"), None);
        assert_eq!(run("at 5pm et voilà").1, "et voilà");
        assert_eq!(
            run("at 5pm ET voilà"),
            (utc(2026, 2, 22, 22, 0), "voilà".into())
        );
    }
}
