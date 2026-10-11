//! "AI spend this month" on the status picture: a card per provider, then one graph of the
//! month.

use chrono::{DateTime, TimeDelta, Utc};

use super::format::{month_bounds, plain};
use super::graphs::{path, scale, series};
use super::history;
use super::{Dashboard, GAP, LEFT, RIGHT, SpendView, card_title, section};
use crate::ai::display_name;
use crate::util::shorten;
use crate::util::svg::{
    ACCENT, AMBER, DIM, GREEN, LINE, MUTED, RED, ROW, Style, Svg, TEXT, pill_width, text_width,
};

/// Each provider's color in the spend section.
const CLAUDE: &str = "#d97757";
const OPENAI: &str = "#10a37f";

/// Each provider's line and labels on the spend graph and cards.
fn provider_color(provider: &str) -> &'static str {
    match provider {
        "claude" => CLAUDE,
        "openai" => OPENAI,
        _ => ACCENT,
    }
}

/// The most job rows a spend card shows.
const MAX_JOB_ROWS: usize = 5;

/// "AI spend this month": a card per provider side by side (total against its budget, the
/// pace, the admin key, the cache hits and cost per job), then one graph of the month
/// with every provider's line, budget (dashed) and pace (dotted).
pub(super) fn spend(svg: &mut Svg, d: &Dashboard) {
    if d.spends.is_empty() {
        return;
    }
    section(svg, "AI spend this month");
    let (start, end) = month_bounds(d.now);
    let elapsed = (d.now - start).num_seconds() as f64;
    let length = (end - start).num_seconds() as f64;
    // Only guess at the month's end after a day of it.
    let pace_of = |usd: f64| (elapsed >= 86_400.0).then(|| usd * length / elapsed);

    // The cards, all as tall as the one with the most jobs.
    let rows = d
        .spends
        .iter()
        .filter_map(|spend| spend.view.as_ref().ok())
        .map(|view| view.jobs.as_ref().map_or(0, Vec::len).min(MAX_JOB_ROWS))
        .max()
        .unwrap_or(0);
    let height = 150.0 + 20.0 * rows as f32;
    let count = d.spends.len() as f32;
    let width = (RIGHT - LEFT - GAP * (count - 1.0)) / count;
    let y = svg.y;
    for (n, spend) in d.spends.iter().enumerate() {
        let x = LEFT + (width + GAP) * n as f32;
        let area = [x, y, width, height];
        match &spend.view {
            Ok(view) => spend_card(svg, d, area, spend.provider, view, pace_of(view.usd)),
            Err(err) => {
                svg.rect(area, ROW, 8.0, "");
                card_title(svg, x, y, &display_name(spend.provider).to_uppercase());
                svg.text(
                    x + 14.0,
                    y + 48.0,
                    &shorten(&format!("Couldn't read it: {err}"), 40),
                    Style::new(12.0, RED),
                );
            }
        }
    }
    svg.y += height + GAP;

    // The month so far, in one graph.
    let chart_height = 150.0;
    let y = svg.y;
    svg.rect([LEFT, y, RIGHT - LEFT, chart_height], ROW, 8.0, "");
    let area = [32.0, y + 24.0, RIGHT - LEFT - 32.0, chart_height - 50.0];
    let lines: Vec<SpendLine> = d
        .spends
        .iter()
        .filter_map(|spend| {
            let view = spend.view.as_ref().ok()?;
            Some(SpendLine {
                color: provider_color(spend.provider),
                points: series(
                    &d.history,
                    &history::spend_key(spend.provider),
                    start.timestamp(),
                    Some((d.now.timestamp(), view.usd)),
                ),
                usd: view.usd,
                budget: view.budget,
                pace: pace_of(view.usd),
            })
        })
        .collect();
    month_chart(svg, area, &lines, (start, end), d.now);
    svg.y += chart_height;
}

/// One provider's card: its total, budget bar, pace, admin key, cache hits and jobs.
fn spend_card(
    svg: &mut Svg,
    d: &Dashboard,
    [x, y, w, h]: [f32; 4],
    provider: &str,
    spend: &SpendView,
    pace: Option<f64>,
) {
    let color = provider_color(provider);
    let (inner, right) = (x + 16.0, x + w - 16.0);
    svg.rect([x, y, w, h], ROW, 8.0, "");
    svg.raw(format!(
        "<circle cx=\"{}\" cy=\"{}\" r=\"4\" fill=\"{color}\"/>",
        inner + 4.0,
        y + 19.0
    ));
    svg.text(
        inner + 14.0,
        y + 23.0,
        &display_name(provider).to_uppercase(),
        Style::new(11.0, color).weight(700).spaced(),
    );
    let (tag, tag_color) = if spend.billed {
        ("BILLED", GREEN)
    } else {
        ("ESTIMATE", DIM)
    };
    svg.pill(right - pill_width(tag), y + 9.0, tag, tag_color, 10.0);

    // "$12.34 of $100" with a bar.
    let total = format!("${:.2}", spend.usd);
    svg.text(inner, y + 60.0, &total, Style::new(26.0, TEXT).weight(700));
    if spend.budget > 0.0 {
        let of = format!("of ${:.0}", spend.budget);
        let at = inner + text_width(&total, 26.0) + 8.0;
        svg.text(at, y + 60.0, &of, Style::new(13.0, MUTED).weight(500));
    }
    let share = (spend.budget > 0.0).then(|| spend.usd / spend.budget);
    let pace_over = pace.is_some_and(|p| spend.budget > 0.0 && p > spend.budget);
    let bar_color = match share {
        Some(s) if s >= 1.0 => RED,
        Some(s) if s >= 0.8 => AMBER,
        _ if pace_over => AMBER,
        _ => GREEN,
    };
    if let Some(share) = share {
        let width = right - inner;
        svg.rect([inner, y + 70.0, width, 6.0], LINE, 3.0, "");
        let filled = (width * share.min(1.0) as f32).max(6.0);
        svg.rect([inner, y + 70.0, filled, 6.0], bar_color, 3.0, "");
    }
    let mut summary = Vec::new();
    if let Some(share) = share {
        summary.push(format!("{:.0}%", share * 100.0));
    }
    if let Some(pace) = pace {
        summary.push(format!("pace ${pace:.2}"));
    }
    if let Some(hits) = spend.cache_hits {
        summary.push(format!("{:.0}% cache hits", hits * 100.0));
    }
    if let Some(net) = spend.warm_cache {
        summary.push(warm_cache_text(net));
    }
    svg.text(
        inner,
        y + 94.0,
        &summary.join("  ·  "),
        Style::new(12.0, MUTED).weight(500),
    );

    // The admin key and the last bill read.
    let mut key = spend.admin_key.clone();
    if let Some(read) = &spend.last_read {
        key = format!("{key}  ·  {}", plain(read, d.now));
    }
    let key_color = if spend.admin_key_failing { AMBER } else { DIM };
    let fits = ((right - inner) / 5.6) as usize;
    svg.text(
        inner,
        y + 114.0,
        &shorten(&key, fits),
        Style::new(11.0, key_color).weight(500),
    );

    // Cost per job as bars.
    let jobs = spend.jobs.as_deref().unwrap_or_default();
    let most = jobs.first().map_or(0.0, |(_, usd)| *usd).max(0.000_001);
    let (bar_x, bar_width) = (inner + 82.0, right - inner - 82.0 - 58.0);
    for (n, (job, usd)) in jobs.iter().take(MAX_JOB_ROWS).enumerate() {
        let row_y = y + 142.0 + 20.0 * n as f32;
        svg.text(
            inner,
            row_y,
            &shorten(job, 11),
            Style::new(12.0, MUTED).weight(500),
        );
        svg.rect([bar_x, row_y - 8.0, bar_width, 6.0], LINE, 3.0, "");
        let filled = (bar_width * (*usd / most) as f32).max(6.0);
        svg.rect([bar_x, row_y - 8.0, filled, 6.0], color, 3.0, "");
        svg.text(
            right,
            row_y,
            &format!("${usd:.2}"),
            Style::new(12.0, TEXT).weight(600).end(),
        );
    }
}

/// One provider on the spend graph.
struct SpendLine {
    color: &'static str,
    /// Spend samples this month, the current total last.
    points: Vec<(i64, f64)>,
    usd: f64,
    /// 0 when there's no budget.
    budget: f64,
    pace: Option<f64>,
}

/// Spend over the month: each provider's line so far, its pace to the month's end
/// (dotted) and its budget (dashed), in its color, with the days along the bottom.
fn month_chart(
    svg: &mut Svg,
    area: [f32; 4],
    lines: &[SpendLine],
    (start, end): (DateTime<Utc>, DateTime<Utc>),
    now: DateTime<Utc>,
) {
    let [x, y, w, h] = area;
    let span = (start.timestamp(), end.timestamp());
    let top = lines
        .iter()
        .flat_map(|line| [line.usd, line.budget, line.pace.unwrap_or(0.0)])
        .fold(0.01, f64::max)
        * 1.1;
    let range = (0.0, top);

    // The floor and the days of the month.
    svg.raw(format!(
        "<path d=\"M{x},{} H{}\" stroke=\"{LINE}\" stroke-width=\"1\"/>",
        y + h,
        x + w
    ));
    let days = (end - start).num_days();
    for day in [1, 8, 15, 22, days] {
        let at = start + TimeDelta::days(day - 1) + TimeDelta::hours(12);
        let (tx, _) = scale(&[(at.timestamp(), 0.0)], area, span, range)[0];
        svg.text(
            tx,
            y + h + 14.0,
            &format!("{} {}", day, start.format("%b")),
            Style::new(10.0, DIM).middle(),
        );
    }

    for line in lines {
        let color = line.color;
        if line.budget > 0.0 {
            let (_, by) = scale(&[(span.0, line.budget)], area, span, range)[0];
            svg.raw(format!(
                "<path d=\"M{x},{by:.1} H{}\" stroke=\"{color}\" stroke-opacity=\"0.7\" stroke-width=\"1\" stroke-dasharray=\"4 4\"/>",
                x + w
            ));
            svg.text(
                x + w,
                by - 5.0,
                &format!("budget ${:.0}", line.budget),
                Style::new(10.0, color).weight(600).end(),
            );
        }
        let (nx, ny) = scale(&[(now.timestamp(), line.usd)], area, span, range)[0];
        if let Some(pace) = line.pace {
            let (ex, ey) = scale(&[(span.1, pace)], area, span, range)[0];
            svg.raw(format!(
                "<path d=\"M{nx:.1},{ny:.1} L{ex:.1},{ey:.1}\" stroke=\"{color}\" stroke-opacity=\"0.6\" stroke-width=\"1.5\" stroke-dasharray=\"2 4\" stroke-linecap=\"round\"/>"
            ));
        }
        if line.points.len() >= 2 {
            let coords = scale(&line.points, area, span, range);
            let path = path(&coords);
            svg.raw(format!(
                "<path d=\"{path} L{nx:.1},{:.1} L{:.1},{:.1} Z\" fill=\"{color}\" fill-opacity=\"0.12\"/>",
                y + h,
                coords[0].0,
                y + h
            ));
            svg.raw(format!(
                "<path d=\"{path}\" fill=\"none\" stroke=\"{color}\" stroke-width=\"2\" stroke-linejoin=\"round\"/>"
            ));
        }
        svg.raw(format!(
            "<circle cx=\"{nx:.1}\" cy=\"{ny:.1}\" r=\"3.5\" fill=\"{color}\"/>"
        ));
    }
}

/// What keeping the chat cache warm saved this month, or cost when it didn't pay off.
pub fn warm_cache_text(net: f64) -> String {
    if net < 0.0 {
        format!("keep-warm cost ${:.2}", -net)
    } else {
        format!("keep-warm saved ${net:.2}")
    }
}
