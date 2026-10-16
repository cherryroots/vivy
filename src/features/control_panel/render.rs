//! The status as a picture, drawn like the movie wheel's rounds (see `util::svg`).
//!
//! [`online_svg`] lays out a [`Dashboard`]: a header, four tiles with the last day's graph
//! (latency, memory, database, errors), the AI provider, the AI spend this month (a card per
//! provider side by side, and one graph of the month with a line each), one card per feature with its `stats()`, the background
//! loops' timers, and the last error.
//! Graphs come from the samples in `history.rs`; a graph needs two samples, so a new
//! install shows plain numbers for its first 15 minutes.
//!
//! Text from features and from the AI status can hold Discord timestamps (`<t:…:R>`). The
//! embed shows those as they are; here [`plain`] writes them out ("in 3h 12m").
//!
//! The parts in their own files:
//! - `render/spend.rs`: the AI spend cards and the month's graph
//! - `render/graphs.rs`: line graphs, bars, and scaling samples to positions
//! - `render/format.rs`: numbers, times and Discord markup as text

mod format;
mod graphs;
mod spend;

use chrono::{DateTime, TimeDelta, Utc};

use self::format::{
    first_lines, format_mb, format_ms, number, plain, plural, relative, title, wrap_words,
};
pub use self::format::{human_duration, month_bounds};
use self::graphs::{bars, series, sparkline};
use self::spend::spend;
pub use self::spend::warm_cache_text;
use super::history::{self, History};
use crate::core::logging::ErrorStats;
use crate::core::timers::TimerState;
use crate::core::{Panel, Stat};
use crate::util::shorten;
use crate::util::svg::{
    ACCENT, AMBER, DIM, GREEN, MUTED, RED, ROW, Style, Svg, TEXT, esc, pill_width, text_width,
};

/// Width of the picture in SVG units.
const WIDTH: f32 = 800.0;
/// Left and right edge of the cards.
const LEFT: f32 = 16.0;
const RIGHT: f32 = WIDTH - 16.0;
/// Space between cards.
const GAP: f32 = 8.0;

/// Everything the status shows, gathered by `status.rs`. The fallback embed shows the same.
pub struct Dashboard {
    pub now: DateTime<Utc>,
    pub bot_name: String,
    /// "0.1.0 · abc1234"
    pub version: String,
    pub uptime: TimeDelta,
    pub servers: usize,
    pub latency_ms: Option<f64>,
    pub memory_mb: Option<f64>,
    pub database_mb: f64,
    pub errors: ErrorStats,
    /// `None` when chat has no provider (no API key).
    pub ai: Option<AiView>,
    /// One per provider that's set up: Claude, then OpenAI.
    pub spends: Vec<ProviderSpend>,
    pub features: Vec<FeatureView>,
    /// Boxes of longer text from the features, those with the same title merged.
    pub panels: Vec<Panel>,
    /// The background loops, in the order they started.
    pub timers: Vec<TimerState>,
    pub history: History,
}

pub struct AiView {
    /// "Claude"
    pub provider: String,
    pub model: String,
    /// Why chat isn't on the main provider, like "Claude is out of credit; trying again
    /// <t:…:R>".
    pub fallback: Option<String>,
}

/// What one provider cost this month, or why it couldn't be read.
pub struct ProviderSpend {
    /// "claude" or "openai".
    pub provider: &'static str,
    pub view: Result<SpendView, String>,
}

pub struct SpendView {
    pub usd: f64,
    /// 0 when there's no budget.
    pub budget: f64,
    /// The company's bill (with an Admin API key) rather than the estimate.
    pub billed: bool,
    /// "Admin key: on", or why it isn't used.
    pub admin_key: String,
    pub admin_key_failing: bool,
    /// "Last read <t:…:R>: $12.00 billed"
    pub last_read: Option<String>,
    /// Share of input read from the cache, when `show_cache_hits` is on.
    pub cache_hits: Option<f64>,
    /// USD the chat cache keep-warm pings saved this month after paying for themselves,
    /// once they've run (Claude only).
    pub warm_cache: Option<f64>,
    /// USD per job, most expensive first, when `show_job_costs` is on.
    pub jobs: Option<Vec<(String, f64)>>,
}

pub struct FeatureView {
    pub name: &'static str,
    /// The feature's stats, or why they couldn't be loaded.
    pub stats: Result<Vec<Stat>, String>,
}

// ---------------------------------------------------------------------------------------
// The two pictures
// ---------------------------------------------------------------------------------------

pub fn online_svg(d: &Dashboard) -> String {
    let mut svg = Svg::new(WIDTH);
    let sub = format!(
        "{}  ·  updated {} UTC",
        plural(d.servers, "server"),
        d.now.format("%H:%M")
    );
    let right = [
        d.version.clone(),
        format!("up {}", human_duration(d.uptime)),
    ];
    header(&mut svg, &d.bot_name, "ONLINE", GREEN, &right, &sub);
    tiles(&mut svg, d);
    ai(&mut svg, d);
    for panel in &d.panels {
        text_panel(&mut svg, d, panel);
    }
    spend(&mut svg, d);
    features(&mut svg, d);
    timers(&mut svg, d);
    last_error(&mut svg, d);

    svg.y += 22.0;
    svg.text(
        24.0,
        svg.y,
        "Graphs: system over the last day, features over 7 days, spend this month",
        Style::new(11.0, DIM),
    );
    svg.y += 14.0;
    svg.finish()
}

/// The last picture before the bot stops.
pub fn offline_svg(bot_name: &str, version: &str, now: DateTime<Utc>, uptime: TimeDelta) -> String {
    let mut svg = Svg::new(WIDTH);
    let sub = format!(
        "Stopped {} UTC  ·  was up for {}",
        now.format("%-d %b %H:%M"),
        human_duration(uptime)
    );
    header(
        &mut svg,
        bot_name,
        "OFFLINE",
        RED,
        &[version.to_string()],
        &sub,
    );
    svg.y += 4.0;
    svg.finish()
}

// ---------------------------------------------------------------------------------------
// Parts
// ---------------------------------------------------------------------------------------

/// "Vivy  ONLINE" with up to two lines on the right and a summary line under it.
fn header(
    svg: &mut Svg,
    name: &str,
    state: &str,
    color: &'static str,
    right: &[String],
    sub: &str,
) {
    let name = shorten(name, 20);
    svg.text(24.0, 44.0, &name, Style::new(26.0, TEXT).weight(700));
    let x = 24.0 + text_width(&name, 26.0) + 14.0;
    let width = 9.0 + state.len() as f32 * 7.4;
    svg.rect([x, 26.0, width, 20.0], color, 10.0, "fill-opacity=\"0.18\"");
    svg.text(
        x + width / 2.0,
        41.0,
        state,
        Style::new(11.0, color).weight(700).middle().spaced(),
    );
    for (n, line) in right.iter().enumerate() {
        svg.text(
            WIDTH - 24.0,
            30.0 + 16.0 * n as f32,
            line,
            Style::new(12.0, DIM).weight(500).end(),
        );
    }
    svg.text(24.0, 70.0, sub, Style::new(13.0, MUTED));
    svg.y = 86.0;
}

/// Latency, memory, database and errors, each with the last day's graph.
fn tiles(svg: &mut Svg, d: &Dashboard) {
    let width = (RIGHT - LEFT - 3.0 * GAP) / 4.0;
    let height = 84.0;
    let y = svg.y;
    let day_ago = d.now.timestamp() - 86_400;
    let tile = |svg: &mut Svg, n: usize, label: &str| {
        let x = LEFT + n as f32 * (width + GAP);
        svg.rect([x, y, width, height], ROW, 8.0, "");
        svg.text(
            x + 12.0,
            y + 20.0,
            label,
            Style::new(10.0, DIM).weight(700).spaced(),
        );
        x
    };

    let measured = [
        (
            "LATENCY",
            history::LATENCY,
            d.latency_ms,
            format_ms as fn(f64) -> String,
        ),
        ("MEMORY", history::MEMORY, d.memory_mb, format_mb),
        (
            "DATABASE",
            history::DATABASE,
            Some(d.database_mb),
            format_mb,
        ),
    ];
    for (n, (label, key, value, format)) in measured.into_iter().enumerate() {
        let x = tile(svg, n, label);
        let shown = value.map_or("–".to_string(), format);
        svg.text(
            x + 12.0,
            y + 46.0,
            &shown,
            Style::new(20.0, TEXT).weight(700),
        );
        let points = series(
            &d.history,
            key,
            day_ago,
            value.map(|v| (d.now.timestamp(), v)),
        );
        sparkline(
            svg,
            [x + 12.0, y + 56.0, width - 24.0, 18.0],
            &points,
            (day_ago, d.now.timestamp()),
            ACCENT,
        );
    }

    // Errors: today's count, and a bar per hour.
    let x = tile(svg, 3, "ERRORS");
    let color = if d.errors.last_hour > 0 { RED } else { TEXT };
    svg.text(
        x + 12.0,
        y + 46.0,
        &d.errors.last_day.to_string(),
        Style::new(20.0, color).weight(700),
    );
    svg.text(
        x + 12.0 + text_width(&d.errors.last_day.to_string(), 20.0) + 6.0,
        y + 46.0,
        &format!("today, {} this hour", d.errors.last_hour),
        Style::new(11.0, DIM),
    );
    bars(
        svg,
        [x + 12.0, y + 56.0, width - 24.0, 18.0],
        &d.errors.per_hour,
        RED,
    );
    svg.y += height;
}

/// The provider and model chat uses, and the fallback if it's on one.
fn ai(svg: &mut Svg, d: &Dashboard) {
    section(svg, "AI");
    let lines = d.ai.as_ref().and_then(|ai| ai.fallback.as_ref()).is_some();
    let height = if lines { 62.0 } else { 40.0 };
    let y = svg.y;
    svg.rect([LEFT, y, RIGHT - LEFT, height], ROW, 8.0, "");
    let Some(ai) = &d.ai else {
        svg.text(32.0, y + 25.0, "Off (no API key)", Style::new(14.0, DIM));
        svg.y += height;
        return;
    };
    svg.text(
        32.0,
        y + 25.0,
        &ai.provider,
        Style::new(15.0, TEXT).weight(700),
    );
    svg.text(
        32.0 + text_width(&ai.provider, 15.0) + 12.0,
        y + 25.0,
        &ai.model,
        Style::new(13.0, MUTED),
    );
    let (state, color) = if ai.fallback.is_some() {
        ("FALLBACK", AMBER)
    } else {
        ("MAIN", GREEN)
    };
    // Right-aligned.
    svg.pill(
        RIGHT - 16.0 - pill_width(state),
        y + 11.0,
        state,
        color,
        10.0,
    );
    if let Some(fallback) = &ai.fallback {
        svg.text(
            32.0,
            y + 48.0,
            &shorten(&plain(fallback, d.now), 90),
            Style::new(12.0, AMBER).weight(500),
        );
    }
    svg.y += height;
}

/// A feature's box of longer text: labels on the left, a line or a few on the right, and
/// its picture (like Vivy's face) in a circle at the top right.
fn text_panel(svg: &mut Svg, d: &Dashboard, panel: &Panel) {
    section(svg, &panel.title);
    let (label_x, text_x) = (32.0, 160.0);
    let picture_size = 64.0;
    let text_right = match panel.picture {
        Some(_) => RIGHT - 16.0 - picture_size - 12.0,
        None => RIGHT - 16.0,
    };
    // Running text is narrower than `text_width`'s estimate, which is made for names.
    let chars = ((text_right - text_x) / (13.0 * 0.5)) as usize;
    let rows: Vec<(&str, Vec<String>)> = panel
        .rows
        .iter()
        .map(|row| {
            (
                row.name.as_str(),
                wrap_words(&plain(&row.value, d.now), chars, 3),
            )
        })
        .collect();
    let lines: usize = rows.iter().map(|(_, lines)| lines.len().max(1)).sum();
    let mut height = 20.0 + 19.0 * lines as f32 + 8.0 * rows.len().saturating_sub(1) as f32;
    if panel.picture.is_some() {
        height = height.max(picture_size + 24.0);
    }
    let y = svg.y;
    svg.rect([LEFT, y, RIGHT - LEFT, height], ROW, 8.0, "");
    svg.rect([LEFT, y, 4.0, height], ACCENT, 2.0, "");
    if let Some(picture) = &panel.picture {
        let r = picture_size / 2.0;
        let (cx, cy) = (RIGHT - 16.0 - r, y + 12.0 + r);
        let id = format!("picture{}", y as i32);
        svg.raw(format!(
            "<clipPath id=\"{id}\"><circle cx=\"{cx}\" cy=\"{cy}\" r=\"{r}\"/></clipPath>\
             <image href=\"{}\" x=\"{}\" y=\"{}\" width=\"{picture_size}\" height=\"{picture_size}\" \
             preserveAspectRatio=\"xMidYMid slice\" clip-path=\"url(#{id})\"/>\
             <circle cx=\"{cx}\" cy=\"{cy}\" r=\"{r}\" fill=\"none\" stroke=\"{ACCENT}\" stroke-width=\"2\"/>",
            esc(picture),
            cx - r,
            cy - r,
        ));
    }
    let mut line_y = y + 24.0;
    for (label, lines) in rows {
        svg.text(
            label_x,
            line_y,
            &shorten(label, 16),
            Style::new(12.0, DIM).weight(600),
        );
        for line in &lines {
            svg.text(text_x, line_y, line, Style::new(13.0, TEXT).weight(500));
            line_y += 19.0;
        }
        if lines.is_empty() {
            line_y += 19.0;
        }
        line_y += 8.0;
    }
    svg.y += height;
}

/// Feature cards side by side.
const FEATURE_COLUMNS: usize = 3;
/// Space between a feature card's stat lines.
const STAT_LINE: f32 = 22.0;

/// One card per feature with its stats, three side by side. Numbers get a graph of the
/// last 7 days and how much they changed in that time.
fn features(svg: &mut Svg, d: &Dashboard) {
    let shown: Vec<&FeatureView> = d
        .features
        .iter()
        .filter(|f| !matches!(&f.stats, Ok(stats) if stats.is_empty()))
        .collect();
    if shown.is_empty() {
        return;
    }
    section(svg, "Features");
    let columns = FEATURE_COLUMNS as f32;
    let width = (RIGHT - LEFT - GAP * (columns - 1.0)) / columns;
    let week_ago = d.now.timestamp() - 7 * 86_400;
    let rows = |f: &FeatureView| match &f.stats {
        Ok(stats) => stats.len(),
        Err(_) => 1,
    };
    for (n, row) in shown.chunks(FEATURE_COLUMNS).enumerate() {
        if n > 0 {
            svg.y += GAP;
        }
        let lines = row.iter().map(|f| rows(f)).max().unwrap_or(1) as f32;
        let height = 40.0 + STAT_LINE * lines;
        for (column, feature) in row.iter().enumerate() {
            let x = LEFT + column as f32 * (width + GAP);
            feature_card(svg, d, feature, [x, svg.y, width, height], week_ago);
        }
        svg.y += height;
    }
}

fn feature_card(svg: &mut Svg, d: &Dashboard, f: &FeatureView, card: [f32; 4], week_ago: i64) {
    let [x, y, width, _] = card;
    svg.rect(card, ROW, 8.0, "");
    svg.text(
        x + 14.0,
        y + 26.0,
        &title(f.name),
        Style::new(14.0, TEXT).weight(700),
    );
    let stats = match &f.stats {
        Ok(stats) => stats,
        Err(err) => {
            svg.text(
                x + 14.0,
                y + 54.0,
                &shorten(&format!("Couldn't load: {err}"), 40),
                Style::new(12.0, RED),
            );
            return;
        }
    };
    let mut graphed = false;
    let right = x + width - 14.0;
    for (n, stat) in stats.iter().enumerate() {
        let row_y = y + 50.0 + STAT_LINE * n as f32;
        let value = plain(&stat.value, d.now);
        let points = match value.parse::<f64>() {
            Ok(number) => series(
                &d.history,
                &history::stat_key(f.name, &stat.name),
                week_ago,
                Some((d.now.timestamp(), number)),
            ),
            Err(_) => Vec::new(),
        };
        if points.len() < 2 {
            // The value gets what it needs, up to half the card; the name gets the rest.
            let value = shorten(&value, ((width / 2.0) / (13.0 * 0.6)) as usize);
            let room = width - 28.0 - text_width(&value, 13.0) - 8.0;
            label(svg, x + 14.0, row_y, &stat.name, room);
            svg.text(
                right,
                row_y,
                &value,
                Style::new(13.0, TEXT).weight(600).end(),
            );
            continue;
        }
        graphed = true;
        svg.text(
            right,
            row_y,
            &value,
            Style::new(13.0, TEXT).weight(600).end(),
        );
        // Value, graph and change from the right; the name gets what's left.
        let spark_right = right - text_width(&value, 13.0).max(28.0) - 8.0;
        let spark = [spark_right - 56.0, row_y - 12.0, 56.0, 14.0];
        sparkline(svg, spark, &points, (week_ago, d.now.timestamp()), ACCENT);
        let change = points[points.len() - 1].1 - points[0].1;
        let (text, color) = match change {
            c if c > 0.0 => (format!("+{}", number(c)), GREEN),
            c if c < 0.0 => (format!("−{}", number(-c)), AMBER),
            _ => ("±0".to_string(), DIM),
        };
        svg.text(
            spark[0] - 6.0,
            row_y,
            &text,
            Style::new(11.0, color).weight(600).end(),
        );
        let room = spark[0] - 6.0 - text_width(&text, 11.0) - 8.0 - (x + 14.0);
        label(svg, x + 14.0, row_y, &stat.name, room);
    }
    if graphed {
        svg.text(
            x + width - 14.0,
            y + 25.0,
            "7 days",
            Style::new(11.0, DIM).end(),
        );
    }
}

/// A stat's name, cut to fit in `room` units.
fn label(svg: &mut Svg, x: f32, y: f32, name: &str, room: f32) {
    // `text_width` is made for names in bold; plain text this size is narrower.
    let max = (room / (13.0 * 0.52)).max(4.0) as usize;
    svg.text(
        x,
        y,
        &shorten(name, max),
        Style::new(13.0, MUTED).weight(500),
    );
}

/// The most recent error, in a red box.
/// One small card per background loop, two side by side: its name and when it runs next on
/// top, how often it runs and when it last ran below.
fn timers(svg: &mut Svg, d: &Dashboard) {
    if d.timers.is_empty() {
        return;
    }
    section(svg, "Timers");
    let width = (RIGHT - LEFT - GAP) / 2.0;
    let height = 44.0;
    for (n, pair) in d.timers.chunks(2).enumerate() {
        if n > 0 {
            svg.y += 6.0;
        }
        for (column, timer) in pair.iter().enumerate() {
            let x = LEFT + column as f32 * (width + GAP);
            let (y, right) = (svg.y, x + width - 14.0);
            svg.rect([x, y, width, height], ROW, 8.0, "");
            let (next, color) = timer_next(timer, d.now);
            svg.text(
                right,
                y + 19.0,
                &next,
                Style::new(13.0, color).weight(600).end(),
            );
            let room = width - 28.0 - text_width(&next, 13.0) - 8.0;
            let max = (room / (13.0 * 0.55)) as usize;
            svg.text(
                x + 14.0,
                y + 19.0,
                &shorten(timer.name, max),
                Style::new(13.0, TEXT).weight(500),
            );
            let last = timer
                .last
                .map(|last| format!("ran {}", relative(last, d.now)))
                .unwrap_or_default();
            svg.text(right, y + 35.0, &last, Style::new(11.0, MUTED).end());
            let room = width - 28.0 - text_width(&last, 11.0) - 8.0;
            let max = (room / (11.0 * 0.55)) as usize;
            svg.text(
                x + 14.0,
                y + 35.0,
                &shorten(timer.every, max),
                Style::new(11.0, DIM),
            );
        }
        svg.y += height;
    }
}

/// "in 12m", "running" while it works, or "due" when it's late: a loop busy with something
/// else, or stuck.
fn timer_next(timer: &TimerState, now: DateTime<Utc>) -> (String, &'static str) {
    match timer.next {
        None => ("running".to_string(), GREEN),
        Some(at) if at < now - TimeDelta::seconds(60) => ("due".to_string(), AMBER),
        Some(at) if at <= now => ("now".to_string(), ACCENT),
        Some(at) => (relative(at, now), ACCENT),
    }
}

fn last_error(svg: &mut Svg, d: &Dashboard) {
    let Some((at, text)) = &d.errors.last else {
        return;
    };
    section(svg, "Last error");
    let lines = wrap_words(&first_lines(text), 88, 4);
    let height = 36.0 + 17.0 * lines.len() as f32;
    let y = svg.y;
    svg.rect(
        [LEFT, y, RIGHT - LEFT, height],
        RED,
        8.0,
        "fill-opacity=\"0.10\"",
    );
    svg.rect([LEFT, y, 4.0, height], RED, 2.0, "");
    svg.text(
        32.0,
        y + 22.0,
        &format!(
            "{}  ·  {} UTC",
            relative(*at, d.now),
            at.format("%-d %b %H:%M")
        ),
        Style::new(12.0, RED).weight(600),
    );
    for (n, line) in lines.iter().enumerate() {
        svg.text(
            32.0,
            y + 42.0 + 17.0 * n as f32,
            line,
            Style::new(12.0, MUTED),
        );
    }
    svg.y += height;
}

// ---------------------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------------------

/// A section title, with room above it for the cards before.
fn section(svg: &mut Svg, title: &str) {
    svg.y += 12.0;
    svg.section(title);
}

/// A small label at the top of a card.
fn card_title(svg: &mut Svg, x: f32, y: f32, text: &str) {
    svg.text(
        x + 14.0,
        y + 22.0,
        text,
        Style::new(10.0, DIM).weight(700).spaced(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text).unwrap().to_utc()
    }

    /// A week of made-up history: numbers that wander a bit, spend that grows.
    fn sample_history(now: DateTime<Utc>) -> History {
        let mut history = History::new();
        let (start, _) = month_bounds(now);
        let end = now.timestamp() / history::EVERY_SECS * history::EVERY_SECS;
        let mut t = end - 7 * 86_400;
        let mut answers: f64 = 1180.0;
        while t < end {
            let hour = (t / 3600 % 24) as f64;
            let wave = (t as f64 / 5000.0).sin();
            let mut push =
                |key: String, value: f64| history.entry(key).or_default().push((t, value));
            push(
                history::LATENCY.into(),
                42.0 + 8.0 * wave + if hour == 3.0 { 30.0 } else { 0.0 },
            );
            push(
                history::MEMORY.into(),
                38.0 + 4.0 * (t as f64 / 40_000.0).sin(),
            );
            push(
                history::DATABASE.into(),
                11.0 + (t - end) as f64 / 900_000.0,
            );
            if (8.0..23.0).contains(&hour) {
                answers += 1.0;
            }
            push(history::stat_key("chat", "Answers"), answers.floor());
            push(
                history::stat_key("reminders", "Pending"),
                4.0 + (2.0 * (t as f64 / 90_000.0).sin()).round(),
            );
            push(
                history::stat_key("snails", "Links"),
                5200.0 + ((t - end) / 4000) as f64,
            );
            if t >= start.timestamp() {
                let days = (t - start.timestamp()) as f64 / 86_400.0;
                push(history::spend_key("claude"), 2.9 * days + 0.4 * wave);
                push(history::spend_key("openai"), 0.6 * days + 0.1 * wave);
            }
            t += history::EVERY_SECS;
        }
        history
    }

    fn sample(now: DateTime<Utc>) -> Dashboard {
        let mut per_hour = [0; 24];
        per_hour[5] = 2;
        per_hour[17] = 1;
        per_hour[23] = 1;
        Dashboard {
            now,
            bot_name: "Vivy".into(),
            version: "0.1.0 · c61bd5c".into(),
            uptime: TimeDelta::minutes(3 * 1440 + 4 * 60 + 12),
            servers: 3,
            latency_ms: Some(44.0),
            memory_mb: Some(39.4),
            database_mb: 11.2,
            errors: ErrorStats {
                last_hour: 1,
                last_day: 4,
                per_hour,
                last: Some((
                    now - TimeDelta::minutes(12),
                    "chat: couldn't answer <message link>\nCaused by:\n  Claude API error 529: Overloaded. The server is temporarily overloaded, please try again in a moment.".into(),
                )),
            },
            ai: Some(AiView {
                provider: "Claude".into(),
                model: "claude-opus-5-5".into(),
                fallback: None,
            }),
            spends: vec![
                ProviderSpend {
                    provider: "claude",
                    view: Ok(SpendView {
                        usd: 26.84,
                        budget: 100.0,
                        billed: true,
                        admin_key: "Admin key: on".into(),
                        admin_key_failing: false,
                        last_read: Some(format!(
                            "Last read <t:{}:R>: $26.10 billed",
                            (now - TimeDelta::minutes(34)).timestamp()
                        )),
                        cache_hits: Some(0.71),
                        warm_cache: Some(1.24),
                        jobs: Some(vec![
                            ("chat".into(), 19.42),
                            ("chime".into(), 3.10),
                            ("reflection".into(), 2.65),
                            ("diary".into(), 0.92),
                            ("emoji".into(), 0.75),
                        ]),
                    }),
                },
                ProviderSpend {
                    provider: "openai",
                    view: Ok(SpendView {
                        usd: 5.62,
                        budget: 25.0,
                        billed: false,
                        admin_key: "Admin key: off".into(),
                        admin_key_failing: false,
                        last_read: None,
                        cache_hits: Some(0.38),
                        warm_cache: None,
                        jobs: Some(vec![("drawing".into(), 4.80), ("chat".into(), 0.82)]),
                    }),
                },
            ],
            features: vec![
                FeatureView {
                    name: "chat",
                    stats: Ok(vec![Stat::new("Answers", 1301), Stat::new("Follow-ups", 2)]),
                },
                FeatureView {
                    name: "reminders",
                    stats: Ok(vec![
                        Stat::new("Pending", 5),
                        Stat::new("Sent this week", 11),
                        Stat::new("Next", format!("<t:{}:R>", (now + TimeDelta::minutes(134)).timestamp())),
                    ]),
                },
                FeatureView {
                    name: "wheel",
                    stats: Ok(vec![
                        Stat::new("Games", 1),
                        Stat::new("Open bets", 6),
                        Stat::new("Rounds played", 7),
                    ]),
                },
                FeatureView {
                    name: "memory",
                    stats: Ok(vec![
                        Stat::new("Files", 48),
                        Stat::new("Folders", 7),
                        Stat::new("Changes today", 9),
                        Stat::new("Last reflection", format!("<t:{}:R>", (now - TimeDelta::hours(9)).timestamp())),
                    ]),
                },
                FeatureView {
                    name: "snails",
                    stats: Ok(vec![
                        Stat::new("Caught this week", 14),
                        Stat::new("Links", 5302),
                        Stat::new("Pictures", 18113),
                        Stat::new("Backfill", "41/52 channels"),
                    ]),
                },
                FeatureView {
                    name: "broken",
                    stats: Err("database is locked".into()),
                },
            ],
            panels: vec![Panel {
                title: "Vivy's mind".into(),
                rows: vec![
                    Stat::new("Mood", "cozy and a little smug after winning the horror movie argument"),
                    Stat::new("Thinking about", "the wheel finale, Sam's Japan trip, whether Alien beats Aliens"),
                    Stat::new("Wants to know", "what Mira's thesis is about, and if anyone actually finished Dune"),
                    Stat::new(
                        "Next check-in",
                        format!(
                            "<t:{}:R> with Sam (2 planned): ask how the job interview went",
                            (now + TimeDelta::minutes(5 * 60 + 20)).timestamp()
                        ),
                    ),
                ],
                picture: Some(sample_face()),
            }],
            timers: sample_timers(now),
            history: sample_history(now),
        }
    }

    fn sample_timers(now: DateTime<Utc>) -> Vec<TimerState> {
        let timer = |name, every, last: Option<i64>, next: Option<i64>| TimerState {
            name,
            every,
            last: last.map(|m| now - TimeDelta::minutes(m)),
            next: next.map(|m| now + TimeDelta::minutes(m)),
        };
        vec![
            timer("Status graphs", "every 15m", Some(10), Some(5)),
            timer("Reminders", "at the next reminder", Some(3), Some(75)),
            timer("Check-ins", "at the next check-in", Some(40), Some(20)),
            timer("Emoji descriptions", "daily", Some(300), Some(1140)),
            timer(
                "Memory upkeep",
                "hourly: reflection, mood, diary",
                Some(0),
                None,
            ),
            timer("Snail picture retries", "every 5m", None, Some(-4)),
            timer("Anthropic bill", "hourly", Some(55), Some(5)),
            timer("OpenAI bill", "hourly", Some(50), Some(10)),
        ]
    }

    /// A stand-in for Vivy's face: a teal square with a lighter middle.
    fn sample_face() -> String {
        use base64::Engine as _;
        let face = image::RgbaImage::from_fn(64, 64, |x, y| {
            let middle = (16..48).contains(&x) && (16..48).contains(&y);
            image::Rgba(if middle {
                [190, 240, 235, 255]
            } else {
                [64, 190, 200, 255]
            })
        });
        let mut png = Vec::new();
        face.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        format!(
            "data:image/png;base64,{}",
            base64::prelude::BASE64_STANDARD.encode(png)
        )
    }

    /// Also writes the pictures to `$STATUS_MOCKUP_DIR` when it's set, to look at them.
    #[test]
    fn draws_the_status() {
        let now = at("2026-10-10T08:40:00Z");
        let full = sample(now);
        let svg = online_svg(&full);
        assert!(svg.contains(">Vivy<") && svg.contains(">ONLINE<"));
        assert!(svg.contains(">$26.84<") && svg.contains(">of $100<"));
        assert!(svg.contains(">$5.62<") && svg.contains(">of $25<"));
        assert!(svg.contains(">AI SPEND THIS MONTH<") && svg.contains(">OPENAI<"));
        assert!(svg.contains("on pace for $"));
        // Too long for the card on one line, so keep-warm goes on the next.
        assert!(svg.contains(">keep-warm saved $1.24<"));
        assert!(svg.contains(">in 2h 14m<"));
        assert!(svg.contains(">VIVY&apos;S MIND<") || svg.contains(">VIVY'S MIND<"));
        assert!(svg.contains(">in 5h 20m with Sam (2 planned): ask how the job"));
        assert!(
            svg.contains(">Couldn&apos;t load: database is locked<")
                || svg.contains("Couldn't load: database is locked")
        );
        assert!(svg.contains(">12m ago  ·  10 Oct 08:28 UTC<"));
        assert!(svg.contains(">TIMERS<"));
        assert!(svg.contains(">in 1h 15m<") && svg.contains(">ran 3m ago<"));
        assert!(svg.contains(">running<") && svg.contains(">due<"));

        // A new install: no history, no spend, a fallback, no errors.
        let mut fresh = sample(now);
        fresh.history = History::new();
        fresh.spends = Vec::new();
        fresh.panels = Vec::new();
        fresh.timers = Vec::new();
        fresh.errors = ErrorStats {
            last_hour: 0,
            last_day: 0,
            per_hour: [0; 24],
            last: None,
        };
        fresh.ai.as_mut().unwrap().fallback =
            Some("Claude is out of credit; trying again <t:1791622800:R>".into());
        let fresh_svg = online_svg(&fresh);
        assert!(fresh_svg.contains(">FALLBACK<"));
        assert!(!fresh_svg.contains("Last error"));

        let offline = offline_svg("Vivy", "0.1.0 · c61bd5c", now, TimeDelta::hours(5));
        for (name, svg) in [("online", svg), ("fresh", fresh_svg), ("offline", offline)] {
            let png = crate::util::svg::to_png(|| svg.clone()).unwrap();
            assert!(png.starts_with(b"\x89PNG"));
            if let Ok(dir) = std::env::var("STATUS_MOCKUP_DIR") {
                std::fs::write(format!("{dir}/status-{name}.png"), png).unwrap();
            }
        }
    }
}
