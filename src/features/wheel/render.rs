//! The round as a picture. [`round_svg`] lays the round out as SVG text (pure, tested like
//! any other string), and [`round_png`] turns that into a PNG with the shared drawing
//! helpers in `util::svg`, which also explain the fonts.
//!
//! Discord on a computer fits a picture in a box wider than it is tall (about 550 × 350),
//! so a tall picture shows small there, while a phone fits it to the screen's width. An open
//! round, which grows with every player and option, is therefore wide, in two columns. A
//! resolved round is one table and stays narrow, which suits both.

use super::ledger::{OutcomeKind, Rules, Standing, TAX_THRESHOLD, outcomes};
use super::ui::View;
use crate::util::count;
use crate::util::svg::{
    AMBER, BG, DIM, GREEN, LINE, MUTED, RED, ROW, Style, Svg, TEXT, columns, markup, text_width,
    to_png,
};

/// Width of an open round's picture in SVG units.
const WIDTH: f32 = 960.0;
/// Width of each of its two columns.
const COLUMN: f32 = WIDTH / 2.0;
/// Width of a resolved round's picture.
const NARROW: f32 = 600.0;
/// Names longer than this are cut short.
const MAX_NAME: usize = 14;
/// How wide the bets on an option may be before they wrap to another line.
const BETS_WIDTH: f32 = 290.0;

/// The round as a PNG. Call it off the async threads (see [`to_png`]).
pub fn round_png(view: &View) -> anyhow::Result<Vec<u8>> {
    to_png(|| round_svg(view))
}

/// The round as SVG: standings and bets while it's open, the payouts once it has a winner.
pub fn round_svg(view: &View) -> String {
    if view.season.rounds[view.index].winner.is_some() {
        let mut svg = Svg::new(NARROW);
        resolved(&mut svg, view);
        svg.finish()
    } else {
        let mut svg = Svg::new(WIDTH);
        open(&mut svg, view);
        svg.finish()
    }
}

/// The open round: who has what on the left; the bets and who still has to play on the
/// right.
fn open(svg: &mut Svg, view: &View) {
    open_header(svg, view);
    let mut left = Svg::new(COLUMN);
    standings_table(&mut left, view);
    let mut right = Svg::new(COLUMN);
    bets_table(&mut right, view);
    // The rest goes under whichever column is shorter.
    let shorter = if left.y <= right.y {
        &mut left
    } else {
        &mut right
    };
    // Who still has to play, while the round is open.
    if view.active && view.is_latest() {
        waiting_on(shorter, view);
    }
    already_won(shorter, view);
    side_by_side(svg, left, right);
    svg.y += 20.0;
}

/// Puts two columns next to each other under what `svg` already has.
fn side_by_side(svg: &mut Svg, left: Svg, right: Svg) {
    let (top, height) = (svg.y, left.y.max(right.y));
    svg.raw(left.group(0.0, top));
    svg.raw(right.group(COLUMN, top));
    svg.y = top + height;
}

/// "Round 4  OPEN" with the options left, how many of them one may bet on, and the payouts.
fn open_header(svg: &mut Svg, view: &View) {
    let numbers = &view.ledger[view.index];
    let left = numbers.options_left.len();
    let (state, color) = if view.active && view.is_latest() {
        ("OPEN", GREEN)
    } else {
        ("ENDED", DIM)
    };
    let payouts = match numbers.rules {
        Rules::Classic => format!("winning bets pay ×{}", left.saturating_sub(1)),
        Rules::Pool if numbers.carried > 0 => {
            format!("pot {} ({} carried over)", numbers.pot, numbers.carried)
        }
        Rules::Pool => format!("pot {}", numbers.pot),
    };
    let sub = format!(
        "{left} options left  ·  bet on up to {}  ·  {payouts}",
        numbers.max_bets()
    );
    header(svg, view, state, color, &sub);
}

/// Everyone's money, bet and tax, richest first, with the tax rule under it.
fn standings_table(svg: &mut Svg, view: &View) {
    let numbers = &view.ledger[view.index];
    svg.section("Standings");
    let standings = sorted_by_money(view, &numbers.standings);
    if standings.is_empty() {
        svg.note("Nobody has played yet. Press Claim to get started.");
    } else {
        // Shown small (a third of this size on a phone, about half on a computer), so the
        // text is big and there's no decoration: the numbers say it.
        let (money, bet, percent, note) = (236.0, 290.0, 344.0, svg.width - 24.0);
        svg.y += 6.0;
        for (x, label) in [
            (money, "Money"),
            (bet, "Bet"),
            (percent, "Bet %"),
            (note, "If it ended now"),
        ] {
            svg.text(
                x,
                svg.y + 8.0,
                label,
                Style::new(12.0, DIM).weight(500).end(),
            );
        }
        svg.y += 16.0;
        for (n, s) in standings.iter().enumerate() {
            let y = svg.y;
            svg.stripe(n, 32.0);
            let rank = (n + 1).to_string();
            svg.text(
                36.0,
                y + 21.0,
                &rank,
                Style::new(13.0, DIM).weight(600).middle(),
            );
            svg.text(
                52.0,
                y + 21.0,
                &short(view.name(s.user)),
                Style::new(16.0, TEXT).weight(600),
            );
            let row = |x: f32, text: &str, style: Style| (x, text.to_string(), style);
            let bet_text = if s.bet > 0 {
                s.bet.to_string()
            } else {
                "–".to_string()
            };
            let (percent_color, percent_weight) = if s.under_threshold() {
                (AMBER, 700)
            } else {
                (GREEN, 500)
            };
            let (note_text, note_color) = if s.tax > 0 {
                (format!("taxed −{}", s.tax), AMBER)
            } else if s.under_threshold() {
                ("–".to_string(), DIM)
            } else {
                ("safe".to_string(), DIM)
            };
            for (x, text, style) in [
                row(
                    money,
                    &s.money.to_string(),
                    Style::new(16.0, TEXT).weight(600).end(),
                ),
                row(bet, &bet_text, Style::new(16.0, MUTED).end()),
                row(
                    percent,
                    &format!("{}%", s.bet_percent),
                    Style::new(16.0, percent_color).weight(percent_weight).end(),
                ),
                row(
                    note,
                    &note_text,
                    Style::new(15.0, note_color).weight(600).end(),
                ),
            ] {
                svg.text(x, y + 21.0, &text, style);
            }
            svg.y += 32.0;
        }
        // The tax rule itself is in Help; one short line is enough here.
        svg.y += 18.0;
        svg.text(
            24.0,
            svg.y,
            &format!("Bet at least {TAX_THRESHOLD}% each round to avoid the tax."),
            Style::new(13.0, DIM),
        );
        svg.y += 14.0;
    }
}

/// The options still on the wheel, most bet on first, with who bet what on each. An
/// option with bets takes two lines: its name and what is on it, then its movie and the
/// bets. Options nobody bet on come last, two to a row.
fn bets_table(svg: &mut Svg, view: &View) {
    let round = &view.season.rounds[view.index];
    let numbers = &view.ledger[view.index];
    let right = svg.width - 32.0;
    svg.section("Bets this round");
    if numbers.options_left.is_empty() {
        svg.note("Nobody is on the wheel yet. Admins add options with /wheel add.");
        return;
    }
    svg.y += 4.0;
    let total = |option: u64| numbers.total_on(option);
    let mut options = numbers.options_left.clone();
    options.sort_by(|a, b| {
        total(*b)
            .cmp(&total(*a))
            .then_with(|| view.name(*a).cmp(view.name(*b)))
    });
    let (backed, empty): (Vec<u64>, Vec<u64>) = options
        .into_iter()
        .partition(|&o| round.bets.iter().any(|b| b.on == o));
    for option in backed {
        let bets: Vec<_> = round.bets.iter().filter(|b| b.on == option).collect();
        let pieces: Vec<String> = bets
            .iter()
            .map(|b| {
                format!(
                    "{} <tspan fill=\"{TEXT}\" font-weight=\"600\">{}</tspan>",
                    markup(&short(view.name(b.by))),
                    b.amount
                )
            })
            .collect();
        let plain: Vec<String> = bets
            .iter()
            .map(|b| format!("{} {}", short(view.name(b.by)), b.amount))
            .collect();
        let lines = wrap_pieces(&pieces, &plain, BETS_WIDTH, 15.0);
        let height = 34.0 + 22.0 * lines.len() as f32;
        let y = svg.y;
        svg.rect([16.0, y, svg.width - 32.0, height], ROW, 6.0, "");
        svg.text(
            32.0,
            y + 22.0,
            &short(view.name(option)),
            Style::new(16.0, TEXT).weight(600),
        );
        svg.text(
            right,
            y + 22.0,
            &match numbers.odds(option) {
                Some(odds) => format!("{} on it  ·  ×{odds:.1}", total(option)),
                None => format!("{} on it", total(option)),
            },
            Style::new(15.0, MUTED).weight(500).end(),
        );
        for (n, (line, _)) in lines.iter().enumerate() {
            svg.raw_text(
                right,
                y + 44.0 + 22.0 * n as f32,
                line,
                Style::new(15.0, MUTED).end(),
            );
        }
        // The movie shares its line with the first line of bets.
        if let Some(movie) = view.movie(option) {
            let width = right - 32.0 - text_width(&lines[0].1, 15.0) - 20.0;
            svg.text(
                32.0,
                y + 43.0,
                &fit(movie, width, 13.0),
                Style::new(13.0, DIM).weight(500),
            );
        }
        svg.y += height + 4.0;
    }
    if empty.is_empty() {
        return;
    }
    svg.y += 16.0;
    // Said once here, not on every empty option.
    let label = match numbers.rules {
        Rules::Pool => "No bets yet. A bet on one of these takes the whole pot.",
        Rules::Classic => "No bets yet.",
    };
    svg.text(24.0, svg.y, label, Style::new(13.0, DIM));
    svg.y += 10.0;
    let width = (svg.width - 40.0) / 2.0;
    for pair in empty.chunks(2) {
        let height = if pair.iter().any(|&o| view.movie(o).is_some()) {
            56.0
        } else {
            34.0
        };
        for (n, &option) in pair.iter().enumerate() {
            let x = 16.0 + (width + 8.0) * n as f32;
            let dashed = format!("stroke=\"{LINE}\" stroke-dasharray=\"3 3\"");
            svg.rect([x, svg.y, width, height], BG, 6.0, &dashed);
            svg.text(
                x + 16.0,
                svg.y + 22.0,
                &short(view.name(option)),
                Style::new(16.0, MUTED).weight(600),
            );
            if let Some(movie) = view.movie(option) {
                svg.text(
                    x + 16.0,
                    svg.y + 43.0,
                    &fit(movie, width - 32.0, 13.0),
                    Style::new(13.0, DIM).weight(500),
                );
            }
        }
        svg.y += height + 4.0;
    }
}

/// The players who haven't claimed yet, and those who claimed but haven't bet.
fn waiting_on(svg: &mut Svg, view: &View) {
    let round = &view.season.rounds[view.index];
    let numbers = &view.ledger[view.index];
    let players: Vec<u64> = numbers.standings.iter().map(|s| s.user).collect();
    let mut not_claimed: Vec<&str> = players
        .iter()
        .filter(|p| !round.claims.contains(p))
        .map(|&p| view.name(p))
        .collect();
    let mut no_bet: Vec<&str> = players
        .iter()
        .filter(|p| round.claims.contains(p) && !round.bets.iter().any(|b| b.by == **p))
        .map(|&p| view.name(p))
        .collect();
    not_claimed.sort();
    no_bet.sort();
    let mut groups = Vec::new();
    if !not_claimed.is_empty() {
        groups.push(("Haven't claimed", not_claimed));
    }
    if !no_bet.is_empty() {
        groups.push(("Claimed, no bet yet", no_bet));
    }
    if !groups.is_empty() {
        svg.section("Waiting on");
        svg.y += 4.0;
        let mut lines = Vec::new();
        for (label, names) in groups {
            let names: Vec<String> = names.iter().map(|n| short(n)).collect();
            let pieces: Vec<String> = names.iter().map(|n| markup(n)).collect();
            for (n, (line, _)) in wrap_pieces(&pieces, &names, svg.width - 32.0 - 190.0, 15.0)
                .into_iter()
                .enumerate()
            {
                lines.push((if n == 0 { label } else { "" }, line));
            }
        }
        let height = 14.0 + 24.0 * lines.len() as f32;
        svg.rect(
            [16.0, svg.y, svg.width - 32.0, height],
            AMBER,
            8.0,
            "fill-opacity=\"0.10\"",
        );
        svg.rect([16.0, svg.y, 4.0, height], AMBER, 2.0, "");
        for (n, (label, line)) in lines.iter().enumerate() {
            let y = svg.y + 24.0 + 24.0 * n as f32;
            svg.text(32.0, y, label, Style::new(15.0, AMBER).weight(600));
            svg.raw_text(190.0, y, line, Style::new(15.0, TEXT).weight(500));
        }
        svg.y += height;
    }
}

/// "Already won: R1 Alice · R2 Bob", for the earlier rounds of the season.
fn already_won(svg: &mut Svg, view: &View) {
    let mut won: Vec<String> = view.season.rounds[..view.index]
        .iter()
        .filter_map(|r| {
            r.winner
                .map(|w| format!("R{} {}", r.number, short(view.name(w))))
        })
        .collect();
    if let Some(first) = won.first_mut() {
        *first = format!("Already won: {first}");
        let pieces: Vec<String> = won.iter().map(|w| markup(w)).collect();
        svg.y += 8.0;
        for (line, _) in wrap_pieces(&pieces, &won, svg.width - 48.0, 13.0) {
            svg.y += 18.0;
            svg.raw_text(24.0, svg.y, &line, Style::new(13.0, DIM).weight(500));
        }
    }
}

/// A round with a winner: the winner and what everyone won or lost.
fn resolved(svg: &mut Svg, view: &View) {
    let round = &view.season.rounds[view.index];
    let numbers = &view.ledger[view.index];
    let winner = round.winner.expect("resolved rounds have a winner");
    let payouts = match numbers.rules {
        Rules::Classic => format!(
            "winning bets paid ×{}",
            numbers.options_left.len().saturating_sub(1)
        ),
        Rules::Pool if numbers.total_on(winner) == 0 => {
            format!("nobody won the pot of {}, it carries over", numbers.pot)
        }
        Rules::Pool => format!("pot {}", numbers.pot),
    };
    let sub = format!(
        "{}  ·  {}  ·  {payouts}",
        count(round.claims.len(), "claim"),
        count(round.bets.len(), "bet")
    );
    header(svg, view, "RESOLVED", RED, &sub);
    svg.y += 4.0;
    winner_box(svg, view, winner);
    svg.y += 14.0;
    results_table(svg, view);
    svg.y += 24.0;
}

/// The winner, with their movie, and which round is open now.
fn winner_box(svg: &mut Svg, view: &View, winner: u64) {
    let round = &view.season.rounds[view.index];
    let movie = view.movie(winner);
    let height = if movie.is_some() { 78.0 } else { 56.0 };
    svg.rect(
        [16.0, svg.y, svg.width - 32.0, height],
        GREEN,
        10.0,
        "fill-opacity=\"0.14\"",
    );
    if let Some(movie) = movie {
        svg.text(
            32.0,
            svg.y + 68.0,
            &fit(movie, svg.width - 64.0, 15.0),
            Style::new(15.0, MUTED).weight(500),
        );
    }
    svg.text(
        32.0,
        svg.y + 24.0,
        "WINNER",
        Style::new(11.0, GREEN).weight(700).spaced(),
    );
    svg.text(
        32.0,
        svg.y + 46.0,
        &short(view.name(winner)),
        Style::new(22.0, TEXT).weight(700),
    );
    if view.active && view.index + 2 == view.season.rounds.len() {
        let next = format!("round {} is open", round.number + 1);
        svg.text(
            svg.width - 32.0,
            svg.y + 35.0,
            &next,
            Style::new(14.0, MUTED).weight(500).end(),
        );
    }
    svg.y += height;
}

/// Before, bets, tax, after and change for each player, biggest gain first.
fn results_table(svg: &mut Svg, view: &View) {
    let round = &view.season.rounds[view.index];
    let numbers = &view.ledger[view.index];
    svg.section("Results");
    let (name, before, bets, tax, after, change) =
        (40.0, 250.0, 330.0, 400.0, 480.0, svg.width - 28.0);
    svg.y += 6.0;
    for (x, label) in [
        (before, "Before"),
        (bets, "Bets"),
        (tax, "Tax"),
        (after, "After"),
        (change, "Change"),
    ] {
        svg.text(
            x,
            svg.y + 8.0,
            label,
            Style::new(12.0, DIM).weight(500).end(),
        );
    }
    svg.y += 16.0;

    // Biggest gain first; ties by name.
    let mut rows: Vec<&Standing> = numbers.standings.iter().collect();
    rows.sort_by(|a, b| {
        (b.after() - b.money)
            .cmp(&(a.after() - a.money))
            .then_with(|| view.name(a.user).cmp(view.name(b.user)))
    });
    let picked: Vec<u64> = outcomes(round, numbers)
        .iter()
        .filter(|o| o.kind == OutcomeKind::Won)
        .map(|o| o.user)
        .collect();
    for (n, s) in rows.iter().enumerate() {
        let y = svg.y;
        svg.stripe(n, 32.0);
        let shown = short(view.name(s.user));
        svg.text(name, y + 21.0, &shown, Style::new(16.0, TEXT).weight(600));
        if picked.contains(&s.user) {
            let x = name + text_width(&shown, 16.0) + 10.0;
            svg.text(x, y + 21.0, "★", Style::new(15.0, GREEN).weight(700));
        }
        let delta = s.after() - s.money;
        svg.text(
            before,
            y + 21.0,
            &s.money.to_string(),
            Style::new(16.0, MUTED).weight(500).end(),
        );
        svg.text(
            bets,
            y + 21.0,
            &signed(s.payout),
            Style::new(16.0, sign_color(s.payout, GREEN))
                .weight(600)
                .end(),
        );
        let tax_color = if s.tax > 0 { AMBER } else { DIM };
        svg.text(
            tax,
            y + 21.0,
            &signed(-s.tax),
            Style::new(16.0, tax_color).weight(600).end(),
        );
        svg.text(
            after,
            y + 21.0,
            &s.after().to_string(),
            Style::new(16.0, TEXT).weight(700).end(),
        );
        svg.text(
            change,
            y + 21.0,
            &signed(delta),
            Style::new(16.0, sign_color(delta, GREEN)).weight(700).end(),
        );
        svg.y += 32.0;
    }
    if !picked.is_empty() {
        svg.raw_text(
            24.0,
            svg.y + 24.0,
            &format!("<tspan fill=\"{GREEN}\">★</tspan> bet on the winner"),
            Style::new(13.0, DIM).weight(500),
        );
        svg.y += 16.0;
    }
}

/// "Round 4  OPEN" with the season on the right and a summary line under it.
fn header(svg: &mut Svg, view: &View, state: &str, color: &'static str, sub: &str) {
    let title = format!("Round {}", view.season.rounds[view.index].number);
    svg.text(24.0, 44.0, &title, Style::new(26.0, TEXT).weight(700));
    let x = 24.0 + text_width(&title, 26.0) + 14.0;
    svg.pill(x, 26.0, state, color, 11.0);
    let season = format!("Season {}", view.season.number);
    svg.text(
        svg.width - 24.0,
        30.0,
        &season,
        Style::new(12.0, DIM).weight(500).end(),
    );
    svg.text(
        svg.width - 24.0,
        46.0,
        "Movie wheel",
        Style::new(12.0, DIM).weight(500).end(),
    );
    svg.text(24.0, 72.0, sub, Style::new(14.0, MUTED));
    svg.y = 92.0;
}

fn sorted_by_money<'a>(view: &View, standings: &'a [Standing]) -> Vec<&'a Standing> {
    let mut list: Vec<&Standing> = standings.iter().collect();
    list.sort_by(|a, b| {
        b.money
            .cmp(&a.money)
            .then_with(|| view.name(a.user).cmp(view.name(b.user)))
    });
    list
}

/// Joins `pieces` with " · " into lines that fit `width`. `plain` is the same text without
/// markup, for measuring. Each line comes with its plain text.
fn wrap_pieces(
    pieces: &[String],
    plain: &[String],
    width: f32,
    size: f32,
) -> Vec<(String, String)> {
    let mut lines: Vec<(String, String)> = Vec::new();
    for (piece, text) in pieces.iter().zip(plain) {
        match lines.last_mut() {
            Some((line, measured))
                if text_width(&format!("{measured} · {text}"), size) <= width =>
            {
                line.push_str(" · ");
                line.push_str(piece);
                measured.push_str(" · ");
                measured.push_str(text);
            }
            _ => lines.push((piece.clone(), text.clone())),
        }
    }
    lines
}

/// The name cut to [`MAX_NAME`] letters' width.
fn short(name: &str) -> String {
    if name.chars().map(columns).sum::<usize>() <= MAX_NAME {
        return name.to_string();
    }
    let mut cut = String::new();
    let mut width = 0;
    for c in name.chars() {
        width += columns(c);
        if width > MAX_NAME - 1 {
            break;
        }
        cut.push(c);
    }
    format!("{cut}…")
}

/// The text cut short with "…" so it fits `width` at font size `size`.
fn fit(text: &str, width: f32, size: f32) -> String {
    if text_width(text, size) <= width {
        return text.to_string();
    }
    let mut cut = String::new();
    for c in text.chars() {
        cut.push(c);
        if text_width(&format!("{cut}…"), size) > width {
            cut.pop();
            break;
        }
    }
    // End on a whole word when that loses little.
    if let Some(space) = cut.rfind(' ').filter(|&space| space > cut.len() / 2) {
        cut.truncate(space);
    }
    format!("{}…", cut.trim_end())
}

/// "+60", "−15" (a real minus sign), or "–" for nothing.
fn signed(n: i64) -> String {
    match n {
        0 => "–".to_string(),
        n if n > 0 => format!("+{n}"),
        n => format!("−{}", -n),
    }
}

fn sign_color(n: i64, positive: &'static str) -> &'static str {
    match n {
        0 => DIM,
        n if n > 0 => positive,
        _ => RED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::wheel::ledger::{Bet, Round, Season, ledger};
    use crate::features::wheel::ui::Names;

    fn names() -> Names {
        [(1, "Alice"), (2, "Bob"), (3, "Charlie"), (4, "Dana <&>")]
            .into_iter()
            .map(|(id, name)| (id, name.to_string()))
            .collect()
    }

    fn bet(by: u64, on: u64, amount: i64) -> Bet {
        Bet { by, on, amount }
    }

    /// Round 1: Alice bets 20 on Bob and wins 60; Charlie is taxed 30. In round 2 Bob hasn't
    /// claimed and Charlie hasn't bet.
    fn season() -> Season {
        Season {
            id: 1,
            number: 2,
            rules: Default::default(),
            options: vec![1, 2, 3, 4],
            movies: Default::default(),
            rounds: vec![
                Round {
                    id: 1,
                    number: 1,
                    winner: Some(2),
                    claims: vec![1, 2, 3],
                    bets: vec![bet(1, 2, 20)],
                },
                Round {
                    id: 2,
                    number: 2,
                    winner: None,
                    claims: vec![1, 3],
                    bets: vec![bet(1, 4, 30)],
                },
            ],
        }
    }

    fn render(season: &Season, index: usize) -> String {
        let numbers = ledger(season);
        let names = names();
        let view = View {
            season,
            ledger: &numbers,
            index,
            active: true,
            names: &names,
        };
        round_svg(&view)
    }

    #[test]
    fn open_round() {
        let svg = render(&season(), 1);
        assert!(svg.contains(">Round 2<"));
        assert!(svg.contains(">OPEN<"));
        assert!(svg.contains(">Season 2<"));
        assert!(svg.contains("3 options left  ·  bet on up to 2  ·  winning bets pay ×2"));
        // Standings, richest first, with the tax if the round ended now.
        let alice = svg.find(">Alice<").unwrap();
        let bob = svg.find(">Bob<").unwrap();
        assert!(alice < bob);
        assert!(svg.contains(">260<") && svg.contains(">11%<"));
        assert!(svg.contains(">taxed −21<"));
        // Names are escaped.
        assert!(svg.contains("Dana &lt;&amp;&gt;"));
        assert!(svg.contains(">30 on it<"));
        // Bob hasn't claimed; Charlie claimed but hasn't bet.
        assert!(svg.contains(">Haven't claimed<"));
        assert!(svg.contains(">Claimed, no bet yet<"));
        assert!(svg.contains("Already won: R1 Bob"));
    }

    #[test]
    fn resolved_round() {
        let svg = render(&season(), 0);
        assert!(svg.contains(">RESOLVED<"));
        assert!(svg.contains(">WINNER<"));
        assert!(svg.contains(">round 2 is open<"));
        // Alice bet 20 on Bob with 4 options: +60. Charlie paid 30 tax.
        assert!(svg.contains(">+60<"));
        assert!(svg.contains(">−30<"));
        assert!(svg.contains("bet on the winner"));
        assert!(!svg.contains("Waiting on"));
    }

    #[test]
    fn movies_show_under_the_names() {
        let mut season = season();
        season.movies = [
            (2, "Alien".to_string()),
            (
                4,
                "The Lord of the Rings: The Return of the King (Extended)".to_string(),
            ),
        ]
        .into();
        // The winner's movie in the winner box.
        let svg = render(&season, 0);
        assert!(svg.contains(">Alien<"), "{svg}");
        // Options left in the open round, cut short when long.
        let svg = render(&season, 1);
        assert!(
            svg.contains(">The Lord of the Rings: The Return of…<"),
            "{svg}"
        );
        assert!(!svg.contains(">Alien<"));
        assert_eq!(fit("Heat", 100.0, 12.0), "Heat");
    }

    #[test]
    fn empty_round() {
        let season = Season {
            id: 1,
            number: 1,
            rules: Default::default(),
            options: vec![],
            movies: Default::default(),
            rounds: vec![Round {
                id: 1,
                number: 1,
                ..Round::default()
            }],
        };
        let svg = render(&season, 0);
        assert!(svg.contains("Nobody has played yet"));
        assert!(svg.contains("Nobody is on the wheel yet"));
    }

    /// Names resvg can't lay out in a `<tspan>` of their own, and names that need other fonts.
    #[test]
    fn unusual_names() {
        let mut names = names();
        names.insert(1, "Eve 🎬👍🏽 👨‍👩‍👧 🇳🇱 1️⃣".into());
        names.insert(2, "𝗓𝗈𝖾 ✨ ᰁ 𐰁 ａｂ é".into());
        let season = season();
        let numbers = ledger(&season);
        let view = View {
            season: &season,
            ledger: &numbers,
            index: 1,
            active: true,
            names: &names,
        };
        assert!(round_png(&view).unwrap().starts_with(b"\x89PNG"));
    }

    /// A busy pool-rules round, like movie night: six players, movies, several bets. Also
    /// writes the pictures to `$WHEEL_MOCKUP_DIR` when it's set, to look at them.
    #[test]
    fn pool_round_pictures() {
        let names: Names = [
            (1, "Alice"),
            (2, "Bob"),
            (3, "Charlie"),
            (4, "Dana"),
            (5, "Eve 🎬"),
            (6, "Frank"),
        ]
        .into_iter()
        .map(|(id, name)| (id, name.to_string()))
        .collect();
        let season = Season {
            id: 1,
            number: 3,
            rules: Rules::Pool,
            options: vec![1, 2, 3, 4, 5, 6],
            movies: [
                (2, "Alien".to_string()),
                (3, "Paddington 2".to_string()),
                (
                    4,
                    "The Lord of the Rings: The Return of the King".to_string(),
                ),
                (5, "Heat".to_string()),
            ]
            .into(),
            rounds: vec![
                Round {
                    id: 1,
                    number: 1,
                    winner: Some(1),
                    claims: vec![1, 2, 3, 4, 5, 6],
                    bets: vec![bet(2, 1, 30), bet(3, 4, 20), bet(4, 1, 50), bet(5, 2, 10)],
                },
                Round {
                    id: 2,
                    number: 2,
                    winner: Some(6),
                    claims: vec![1, 2, 3, 4, 5, 6],
                    bets: vec![bet(1, 6, 40), bet(2, 3, 25), bet(3, 6, 30), bet(5, 4, 20)],
                },
                Round {
                    id: 3,
                    number: 3,
                    winner: None,
                    claims: vec![1, 2, 3, 5],
                    bets: vec![bet(1, 2, 60), bet(2, 4, 45), bet(3, 2, 30), bet(5, 3, 15)],
                },
            ],
        };
        let numbers = ledger(&season);
        for (index, name) in [(2, "pool-open"), (1, "pool-resolved")] {
            let view = View {
                season: &season,
                ledger: &numbers,
                index,
                active: true,
                names: &names,
            };
            let png = round_png(&view).unwrap();
            assert!(png.starts_with(b"\x89PNG"));
            if let Ok(dir) = std::env::var("WHEEL_MOCKUP_DIR") {
                std::fs::write(format!("{dir}/wheel-{name}.png"), png).unwrap();
            }
        }
    }

    /// A big movie night: twelve players, all on the wheel, most of them betting. Also
    /// written to `$WHEEL_MOCKUP_DIR`, like [`pool_round_pictures`].
    #[test]
    fn big_round_pictures() {
        let people = [
            "Alice",
            "Bob",
            "Charlie",
            "Dana",
            "Eve 🎬",
            "Frank",
            "Grace",
            "Heidi",
            "Ivan",
            "Judy",
            "Mallory",
            "Bartholomew the Third",
        ];
        let names: Names = (1..)
            .zip(people)
            .map(|(id, n)| (id, n.to_string()))
            .collect();
        let movies = [
            (2, "Alien"),
            (3, "Paddington 2"),
            (4, "The Lord of the Rings: The Return of the King"),
            (5, "Heat"),
            (7, "Spirited Away"),
            (8, "Everything Everywhere All at Once"),
            (10, "The Thing"),
            (11, "Mad Max: Fury Road"),
        ];
        let everyone: Vec<u64> = (1..=12).collect();
        let season = Season {
            id: 1,
            number: 4,
            rules: Rules::Pool,
            options: everyone.clone(),
            movies: movies.map(|(id, m)| (id, m.to_string())).into(),
            rounds: vec![
                Round {
                    id: 1,
                    number: 1,
                    winner: Some(6),
                    claims: everyone.clone(),
                    bets: vec![
                        bet(1, 6, 40),
                        bet(2, 3, 25),
                        bet(3, 6, 30),
                        bet(4, 2, 50),
                        bet(5, 4, 20),
                        bet(7, 8, 35),
                        bet(8, 7, 10),
                        bet(9, 6, 15),
                        bet(10, 11, 45),
                        bet(12, 1, 20),
                    ],
                },
                Round {
                    id: 2,
                    number: 2,
                    winner: None,
                    claims: vec![1, 2, 3, 4, 5, 7, 8, 9, 10, 12],
                    bets: vec![
                        bet(1, 2, 60),
                        bet(2, 4, 45),
                        bet(3, 2, 30),
                        bet(5, 3, 15),
                        bet(7, 8, 40),
                        bet(8, 2, 20),
                        bet(9, 11, 25),
                        bet(10, 8, 50),
                        bet(12, 4, 30),
                        bet(4, 7, 35),
                        bet(1, 8, 10),
                    ],
                },
            ],
        };
        let numbers = ledger(&season);
        for (index, name) in [(1, "big-open"), (0, "big-resolved")] {
            let view = View {
                season: &season,
                ledger: &numbers,
                index,
                active: true,
                names: &names,
            };
            let png = round_png(&view).unwrap();
            assert!(png.starts_with(b"\x89PNG"));
            if let Ok(dir) = std::env::var("WHEEL_MOCKUP_DIR") {
                std::fs::write(format!("{dir}/wheel-{name}.png"), png).unwrap();
            }
        }
    }

    #[test]
    fn wrapping_and_names() {
        let pieces: Vec<String> = ["Alice 10", "Bob 20", "Charlie 30"]
            .map(String::from)
            .into();
        let lines = |width| -> Vec<String> {
            wrap_pieces(&pieces, &pieces, width, 13.0)
                .into_iter()
                .map(|(line, _)| line)
                .collect()
        };
        assert_eq!(lines(1000.0), ["Alice 10 · Bob 20 · Charlie 30"]);
        assert_eq!(lines(150.0), ["Alice 10 · Bob 20", "Charlie 30"]);
        assert_eq!(short("Bartholomew the Third"), "Bartholomew t…");
        assert_eq!(short(&"🎬".repeat(8)), format!("{}…", "🎬".repeat(6)));
        assert_eq!(signed(5), "+5");
        assert_eq!(signed(-5), "−5");
        assert_eq!(signed(0), "–");
    }
}
