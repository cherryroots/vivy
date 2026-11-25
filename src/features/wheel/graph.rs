//! Balance history line chart for a movie wheel season.
//!
//! [`history_svg`] builds the chart as SVG text (pure, tested like any other string), and
//! [`history_png`] renders it to a PNG with [`crate::util::svg::to_png`].

use super::ledger::{
    BalanceHistory, PlayerHistory, Rules, Season, balance_history, is_season_over,
};
use super::ui::{self, Names};
use crate::util::count;
use crate::util::svg::{BG, DIM, GREEN, LINE, MUTED, RED, ROW, Style, Svg, TEXT, columns, to_png};

/// Total chart width in SVG units (landscape, crisp on both desktop and phone).
const WIDTH: f32 = 920.0;
/// Narrow width for the empty-state card when no rounds have resolved yet.
const EMPTY_WIDTH: f32 = 600.0;
/// Plot area horizontal bounds.
const PLOT_LEFT: f32 = 76.0;
const PLOT_RIGHT: f32 = 668.0;
const PLOT_TOP: f32 = 98.0;
const MIN_PLOT_HEIGHT: f32 = 340.0;
/// Right-hand legend bounds.
const LEGEND_LEFT: f32 = 690.0;
const LEGEND_RIGHT: f32 = WIDTH - 20.0;
const LEGEND_ROW_HEIGHT: f32 = 25.0;
/// Max display-name columns in the legend so names never collide with final balances.
const MAX_LEGEND_NAME: usize = 12;

/// Colorblind-friendly palette with strong contrast on Discord's dark theme (`BG`).
/// Past 15 players, colors cycle with a dashed stroke so every line stays distinct.
const PALETTE: &[&str] = &[
    "#56B4E9", // sky blue
    "#E69F00", // orange
    "#009E73", // teal green
    "#F0E442", // bright yellow
    "#CC79A7", // mauve pink
    "#D55E00", // vermilion
    "#949CF7", // blurple
    "#4ADE80", // mint green
    "#F472B6", // rose
    "#22D3EE", // cyan
    "#FB923C", // peach
    "#C084FC", // lavender
    "#A3E635", // lime
    "#F87171", // coral
    "#E2E8F0", // silver
];

/// Renders the season's balance history chart as a PNG. Call off the async threads.
pub fn history_png(season: &Season, names: &Names) -> anyhow::Result<Vec<u8>> {
    to_png(|| history_svg(season, names))
}

/// Renders the season's balance history chart as SVG text.
pub fn history_svg(season: &Season, names: &Names) -> String {
    let history = balance_history(season);
    if history.rounds.is_empty() || history.players.is_empty() {
        return empty_svg(season);
    }

    let mut svg = Svg::new(WIDTH);
    draw_header(&mut svg, season, &history);

    // Sort players by final balance descending; break ties by display name ascending.
    let mut ranked: Vec<&PlayerHistory> = history.players.iter().collect();
    ranked.sort_by(|a, b| {
        let a_final = a.balances.last().copied().unwrap_or(0);
        let b_final = b.balances.last().copied().unwrap_or(0);
        b_final
            .cmp(&a_final)
            .then_with(|| ui::name(names, a.user).cmp(ui::name(names, b.user)))
    });

    let legend_content_height = 28.0 + ranked.len() as f32 * LEGEND_ROW_HEIGHT;
    let plot_height = MIN_PLOT_HEIGHT.max(legend_content_height);
    let plot_bottom = PLOT_TOP + plot_height;
    let plot_width = PLOT_RIGHT - PLOT_LEFT;

    // Subtle dark surface behind the plot area.
    svg.rect(
        [PLOT_LEFT, PLOT_TOP, plot_width, plot_height],
        ROW,
        8.0,
        "fill-opacity=\"0.42\"",
    );

    let (y_min, y_max, y_step) = y_axis_bounds(&history);
    let y_span = (y_max - y_min).max(1) as f32;
    let y_for = |value: i64| -> f32 {
        round_tenths(plot_bottom - (value - y_min) as f32 / y_span * plot_height)
    };

    // Horizontal gridlines and Y-axis money labels.
    let mut tick = y_min;
    while tick <= y_max {
        let y = y_for(tick);
        let is_zero = tick == 0 && y_min < 0;
        if is_zero {
            svg.raw(format!(
                "<line x1=\"{PLOT_LEFT}\" y1=\"{y:.1}\" x2=\"{PLOT_RIGHT}\" y2=\"{y:.1}\" stroke=\"{DIM}\" stroke-width=\"1.2\" stroke-dasharray=\"4 3\"/>"
            ));
        } else {
            svg.raw(format!(
                "<line x1=\"{PLOT_LEFT}\" y1=\"{y:.1}\" x2=\"{PLOT_RIGHT}\" y2=\"{y:.1}\" stroke=\"{LINE}\" stroke-width=\"1\"/>"
            ));
        }
        svg.text(
            PLOT_LEFT - 10.0,
            y + 4.5,
            &format_money(tick),
            Style::new(12.0, MUTED).weight(500).end(),
        );
        tick += y_step;
    }

    // X-axis positions: Start (index 0) plus each resolved round.
    let points_count = history.rounds.len() + 1;
    let x_step = plot_width / (points_count - 1) as f32;
    let x_for = |idx: usize| -> f32 { PLOT_LEFT + idx as f32 * x_step };

    // Thin X-axis labels when there are many rounds so they never overlap.
    let label_stride = (points_count / 12).max(1);
    for idx in 0..points_count {
        let x = x_for(idx);
        let is_first = idx == 0;
        let is_last = idx + 1 == points_count;
        // Faint vertical guide at each round.
        if !is_first && !is_last {
            svg.raw(format!(
                "<line x1=\"{x:.1}\" y1=\"{PLOT_TOP}\" x2=\"{x:.1}\" y2=\"{plot_bottom}\" stroke=\"{LINE}\" stroke-opacity=\"0.35\" stroke-width=\"1\"/>"
            ));
        }
        if is_first || is_last || idx.is_multiple_of(label_stride) {
            let label = if is_first {
                "Start".to_string()
            } else {
                format!("R{}", history.rounds[idx - 1])
            };
            svg.text(
                x,
                plot_bottom + 22.0,
                &label,
                Style::new(12.0, MUTED).weight(500).middle(),
            );
        }
    }

    // Draw player lines back-to-front so the #1 leader's line renders cleanly on top.
    for (rank, player) in ranked.iter().enumerate().rev() {
        let color = PALETTE[rank % PALETTE.len()];
        let dashed = if rank >= PALETTE.len() {
            " stroke-dasharray=\"6 3\""
        } else {
            ""
        };
        let coords: Vec<String> = player
            .balances
            .iter()
            .enumerate()
            .map(|(i, &val)| format!("{:.1},{:.1}", x_for(i), y_for(val)))
            .collect();
        svg.raw(format!(
            "<polyline fill=\"none\" stroke=\"{color}\" stroke-width=\"2.6\" stroke-linejoin=\"round\" stroke-linecap=\"round\"{dashed} points=\"{}\"/>",
            coords.join(" ")
        ));
        // Point markers at each round; slightly larger on the final point.
        let last_idx = player.balances.len().saturating_sub(1);
        for (i, &val) in player.balances.iter().enumerate() {
            let r = if i == last_idx { 4.2 } else { 3.0 };
            svg.raw(format!(
                "<circle cx=\"{:.1}\" cy=\"{:.1}\" r=\"{r}\" fill=\"{color}\" stroke=\"{BG}\" stroke-width=\"1.2\"/>",
                x_for(i),
                y_for(val)
            ));
        }
    }

    // Right-hand legend sorted by final balance.
    draw_legend(&mut svg, &ranked, names);

    svg.y = plot_bottom + 44.0;
    svg.finish()
}

/// Rounds a coordinate to the nearest tenth of an SVG unit.
fn round_tenths(y: f32) -> f32 {
    (y * 10.0).round() / 10.0
}

fn empty_svg(season: &Season) -> String {
    let mut svg = Svg::new(EMPTY_WIDTH);
    svg.text(
        24.0,
        44.0,
        "Balance History",
        Style::new(26.0, TEXT).weight(700),
    );
    svg.text(
        svg.width - 24.0,
        30.0,
        &format!("Season {}", season.number),
        Style::new(12.0, DIM).weight(500).end(),
    );
    svg.text(
        svg.width - 24.0,
        46.0,
        "Movie wheel",
        Style::new(12.0, DIM).weight(500).end(),
    );
    svg.y = 74.0;
    svg.note("No rounds have been resolved yet. Check back after the first winner is picked.");
    svg.y += 18.0;
    svg.finish()
}

fn draw_header(svg: &mut Svg, season: &Season, history: &BalanceHistory) {
    let title = "Balance History";
    svg.text(24.0, 44.0, title, Style::new(26.0, TEXT).weight(700));
    let pill_x = 24.0 + crate::util::svg::text_width(title, 26.0) + 14.0;
    let (badge, badge_color) = if is_season_over(season) {
        ("FINAL", GREEN)
    } else {
        ("IN PROGRESS", MUTED)
    };
    svg.pill(pill_x, 26.0, badge, badge_color, 11.0);

    svg.text(
        svg.width - 24.0,
        30.0,
        &format!("Season {}", season.number),
        Style::new(12.0, DIM).weight(500).end(),
    );
    svg.text(
        svg.width - 24.0,
        46.0,
        "Movie wheel",
        Style::new(12.0, DIM).weight(500).end(),
    );

    let rules = match season.rules {
        Rules::Pool => "Pool rules",
        Rules::Classic => "Classic rules",
    };
    let sub = format!(
        "{} resolved  ·  {}  ·  {rules}",
        count(history.rounds.len(), "round"),
        count(history.players.len(), "player"),
    );
    svg.text(24.0, 72.0, &sub, Style::new(14.0, MUTED));
}

fn draw_legend(svg: &mut Svg, ranked: &[&PlayerHistory], names: &Names) {
    svg.text(
        LEGEND_LEFT,
        PLOT_TOP + 12.0,
        "STANDINGS",
        Style::new(11.0, DIM).weight(700).spaced(),
    );
    let start_y = PLOT_TOP + 24.0;
    for (rank, player) in ranked.iter().enumerate() {
        let row_y = start_y + rank as f32 * LEGEND_ROW_HEIGHT;
        if rank.is_multiple_of(2) {
            svg.rect(
                [
                    LEGEND_LEFT - 6.0,
                    row_y,
                    LEGEND_RIGHT - LEGEND_LEFT + 12.0,
                    LEGEND_ROW_HEIGHT - 2.0,
                ],
                ROW,
                5.0,
                "fill-opacity=\"0.65\"",
            );
        }
        let color = PALETTE[rank % PALETTE.len()];
        let mid_y = row_y + (LEGEND_ROW_HEIGHT - 2.0) / 2.0;
        if rank >= PALETTE.len() {
            svg.raw(format!(
                "<line x1=\"{:.1}\" y1=\"{:.1}\" x2=\"{:.1}\" y2=\"{:.1}\" stroke=\"{color}\" stroke-width=\"3\" stroke-dasharray=\"4 2\"/>",
                LEGEND_LEFT,
                mid_y,
                LEGEND_LEFT + 12.0,
                mid_y
            ));
        } else {
            svg.rect([LEGEND_LEFT, mid_y - 2.5, 12.0, 5.0], color, 2.5, "");
        }

        let display = short_name(ui::name(names, player.user));
        svg.text(
            LEGEND_LEFT + 19.0,
            row_y + 16.0,
            &display,
            Style::new(13.5, TEXT).weight(600),
        );

        let final_balance = player.balances.last().copied().unwrap_or(0);
        let value_color = if final_balance < 0 { RED } else { TEXT };
        svg.text(
            LEGEND_RIGHT,
            row_y + 16.0,
            &format_money(final_balance),
            Style::new(13.5, value_color).weight(700).end(),
        );
    }
}

/// Picks clean `(min, max, step)` Y-axis bounds that cover every balance and include 0.
fn y_axis_bounds(history: &BalanceHistory) -> (i64, i64, i64) {
    let mut min_val = 0_i64;
    let mut max_val = 0_i64;
    for player in &history.players {
        for &b in &player.balances {
            min_val = min_val.min(b);
            max_val = max_val.max(b);
        }
    }
    if min_val == max_val {
        return (0, 100, 25);
    }
    let raw_span = (max_val - min_val) as f64;
    let step = nice_step(raw_span / 5.0);
    let nice_min = if min_val < 0 {
        // Floor to a multiple of step.
        (min_val.div_euclid(step)) * step
    } else {
        0
    };
    let nice_max = ((max_val + step - 1).div_euclid(step)) * step;
    (nice_min, nice_max.max(nice_min + step), step)
}

/// Rounds `raw` up to a clean 1 / 2 / 2.5 / 5 × 10^n tick step.
fn nice_step(raw: f64) -> i64 {
    if raw <= 1.0 {
        return 1;
    }
    let exp = raw.log10().floor();
    let base = 10_f64.powf(exp);
    let frac = raw / base;
    let mult = if frac <= 1.0 {
        1.0
    } else if frac <= 2.0 {
        2.0
    } else if frac <= 2.5 {
        2.5
    } else if frac <= 5.0 {
        5.0
    } else {
        10.0
    };
    (mult * base).round().max(1.0) as i64
}

/// Formats a balance for axis and legend labels (real minus sign for negatives).
fn format_money(value: i64) -> String {
    let abs = value.unsigned_abs();
    let sign = if value < 0 { "−" } else { "" };
    if abs >= 100_000 {
        format!("{sign}{}k", abs / 1_000)
    } else if abs >= 10_000 {
        let tenths = (abs + 50) / 100;
        if tenths.is_multiple_of(10) {
            format!("{sign}{}k", tenths / 10)
        } else {
            format!("{sign}{}.{}k", tenths / 10, tenths % 10)
        }
    } else {
        format!("{sign}{abs}")
    }
}

/// Cuts a display name to [`MAX_LEGEND_NAME`] columns with "…".
fn short_name(name: &str) -> String {
    if name.chars().map(columns).sum::<usize>() <= MAX_LEGEND_NAME {
        return name.to_string();
    }
    let mut cut = String::new();
    let mut width = 0;
    for c in name.chars() {
        width += columns(c);
        if width > MAX_LEGEND_NAME - 1 {
            break;
        }
        cut.push(c);
    }
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::wheel::ledger::{Bet, Round};

    fn bet(by: u64, on: u64, amount: i64) -> Bet {
        Bet { by, on, amount }
    }

    fn make_names(pairs: &[(u64, &str)]) -> Names {
        pairs.iter().map(|&(id, n)| (id, n.to_string())).collect()
    }

    #[test]
    fn zero_resolved_rounds_renders_clean_message() {
        let season = Season {
            id: 1,
            number: 3,
            rules: Rules::Pool,
            options: vec![1, 2],
            movies: Default::default(),
            rounds: vec![Round {
                id: 1,
                number: 1,
                winner: None,
                claims: vec![1],
                bets: vec![],
            }],
        };
        let names = make_names(&[(1, "Alice"), (2, "Bob")]);
        let svg = history_svg(&season, &names);
        assert!(svg.contains("Balance History"));
        assert!(svg.contains("Season 3"));
        assert!(svg.contains("No rounds have been resolved yet"));
        let png = history_png(&season, &names).unwrap();
        assert!(png.starts_with(b"\x89PNG"));
    }

    #[test]
    fn single_resolved_round_and_negative_balance() {
        let season = Season {
            id: 1,
            number: 1,
            rules: Rules::Pool,
            options: vec![1, 2, 3],
            movies: Default::default(),
            rounds: vec![Round {
                id: 1,
                number: 1,
                winner: Some(2),
                claims: vec![1, 2, 3],
                // Admin-inserted oversized bet pushes Alice negative.
                bets: vec![bet(1, 3, 200), bet(2, 2, 40)],
            }],
        };
        let names = make_names(&[(1, "Alice"), (2, "Bob"), (3, "Charlie")]);
        let svg = history_svg(&season, &names);
        assert!(svg.contains(">Start<"));
        assert!(svg.contains(">R1<"));
        assert!(svg.contains(">IN PROGRESS<"));
        assert!(svg.contains("−100"));
        let png = history_png(&season, &names).unwrap();
        assert!(png.starts_with(b"\x89PNG"));
    }

    #[test]
    fn many_players_and_ties() {
        let options: Vec<u64> = (1..=14).collect();
        let names: Names = options
            .iter()
            .map(|&id| (id, format!("Player{id:02}")))
            .collect();
        let season = Season {
            id: 1,
            number: 4,
            rules: Rules::Pool,
            options: options.clone(),
            movies: Default::default(),
            rounds: vec![
                Round {
                    id: 1,
                    number: 1,
                    winner: Some(1),
                    claims: options.clone(),
                    bets: vec![bet(1, 1, 20), bet(2, 1, 20), bet(3, 2, 15)],
                },
                Round {
                    id: 2,
                    number: 2,
                    winner: Some(2),
                    claims: options.clone(),
                    bets: vec![bet(4, 2, 30), bet(5, 3, 10)],
                },
            ],
        };
        let svg = history_svg(&season, &names);
        for id in 1..=14 {
            assert!(svg.contains(&format!("Player{id:02}")));
        }
        let png = history_png(&season, &names).unwrap();
        assert!(png.starts_with(b"\x89PNG"));
    }

    #[test]
    fn realistic_ten_round_seven_player_season_sample() {
        // 10 options on the wheel, 7 players betting across all 10 rounds so the season ends.
        let options: Vec<u64> = (1..=10).collect();
        let names = make_names(&[
            (1, "Cherry"),
            (2, "Mika"),
            (3, "Hana"),
            (4, "Sora"),
            (5, "Ren"),
            (6, "Yuki"),
            (7, "Kai"),
            (8, "Taro"),
            (9, "Neko"),
            (10, "Aoi"),
        ]);
        let players = vec![1, 2, 3, 4, 5, 6, 7];

        // Deterministic, varied 10-round season with upsets, carryovers, taxes and comebacks.
        let round_specs: Vec<(u64, Vec<u64>, Vec<Bet>)> = vec![
            (
                3,
                players.clone(),
                vec![
                    bet(1, 3, 35),
                    bet(2, 5, 30),
                    bet(3, 3, 20),
                    bet(4, 8, 25),
                    bet(5, 1, 40),
                    bet(6, 2, 15),
                    // Kai forgets to bet -> taxed
                ],
            ),
            (
                7,
                players.clone(),
                vec![
                    bet(1, 7, 40),
                    bet(2, 7, 30),
                    bet(3, 4, 45),
                    bet(4, 7, 25),
                    bet(5, 2, 20),
                    bet(6, 9, 30),
                    bet(7, 7, 50),
                ],
            ),
            (
                10, // Upset! Nobody bet on 10 -> pot carries over to Round 4.
                players.clone(),
                vec![
                    bet(1, 1, 60),
                    bet(2, 2, 50),
                    bet(3, 4, 40),
                    bet(4, 5, 35),
                    bet(5, 6, 30),
                    bet(6, 8, 40),
                    bet(7, 9, 45),
                ],
            ),
            (
                2, // Big carryover pot won by Mika and Ren!
                players.clone(),
                vec![
                    bet(1, 4, 70),
                    bet(2, 2, 65),
                    bet(3, 5, 50),
                    bet(4, 1, 40),
                    bet(5, 2, 55),
                    bet(6, 6, 35),
                    bet(7, 8, 40),
                ],
            ),
            (
                5,
                players.clone(),
                vec![
                    bet(1, 5, 80),
                    bet(2, 1, 90),
                    bet(3, 5, 60),
                    bet(4, 9, 45),
                    bet(5, 4, 100),
                    bet(6, 5, 40),
                    bet(7, 6, 35),
                ],
            ),
            (
                9,
                players.clone(),
                vec![
                    bet(1, 1, 60),
                    bet(2, 9, 80),
                    bet(3, 9, 75),
                    bet(4, 4, 55),
                    bet(5, 8, 70),
                    bet(6, 6, 30),
                    bet(7, 9, 45),
                ],
            ),
            (
                1,
                players.clone(),
                vec![
                    bet(1, 1, 90),
                    bet(2, 4, 110),
                    bet(3, 8, 80),
                    bet(4, 1, 65),
                    bet(5, 6, 90),
                    bet(6, 1, 50),
                    bet(7, 4, 60),
                ],
            ),
            (
                6,
                players.clone(),
                vec![
                    bet(1, 8, 100),
                    bet(2, 6, 95),
                    bet(3, 4, 90),
                    bet(4, 6, 80),
                    bet(5, 8, 60),
                    bet(6, 6, 90),
                    bet(7, 8, 50),
                ],
            ),
            (
                4,
                players.clone(),
                vec![
                    bet(1, 4, 120),
                    bet(2, 8, 130),
                    bet(3, 4, 100),
                    bet(4, 8, 70),
                    bet(5, 4, 85),
                    bet(6, 8, 95),
                    bet(7, 4, 60),
                ],
            ),
            (
                8, // Final option on the wheel!
                players.clone(),
                vec![
                    bet(1, 8, 140),
                    bet(2, 8, 120),
                    bet(3, 8, 90),
                    bet(4, 8, 60),
                    bet(5, 8, 75),
                    bet(6, 8, 80),
                    bet(7, 8, 50),
                ],
            ),
        ];

        let rounds: Vec<Round> = round_specs
            .into_iter()
            .enumerate()
            .map(|(i, (winner, claims, bets))| Round {
                id: (i + 1) as i64,
                number: (i + 1) as i64,
                winner: Some(winner),
                claims,
                bets,
            })
            .collect();

        let season = Season {
            id: 7,
            number: 7,
            rules: Rules::Pool,
            options,
            movies: Default::default(),
            rounds,
        };

        let svg = history_svg(&season, &names);
        assert!(svg.contains(">FINAL<"));
        assert!(svg.contains("10 rounds resolved  ·  7 players  ·  Pool rules"));
        assert!(svg.contains(">Start<"));
        assert!(svg.contains(">R10<"));
        assert!(svg.contains(">Cherry<"));

        let png = history_png(&season, &names).unwrap();
        assert!(png.starts_with(b"\x89PNG"));
        assert!(png.len() > 10_000, "PNG unexpectedly tiny: {}", png.len());

        if let Ok(path) = std::env::var("WHEEL_HISTORY_SAMPLE") {
            std::fs::write(&path, &png).unwrap();
        }
    }
}
