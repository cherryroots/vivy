//! The game's messages: the question with its buttons, the answer, and the final scores.
//! Everything here takes plain values and builds messages; nothing talks to Discord.

use twilight_model::channel::message::Embed;
use twilight_model::channel::message::component::{ButtonStyle, Component};
use twilight_util::builder::embed::{EmbedBuilder, EmbedFooterBuilder};

use super::game::{Outcome, Quote, Round};
use crate::util::discord::{button, buttons};
use crate::util::shorten;

/// Vivy's teal, down the side of every message.
pub(in crate::features::games) const COLOR: u32 = 0x2bc4b4;

/// A button's ID: `games:who_said_it:pick:<game>:<round>:<person>`. `game` tells a running game
/// from an older one in the same channel.
pub fn button_id(game: u32, round: usize, person: u64) -> String {
    format!("games:who_said_it:pick:{game}:{round}:{person}")
}

/// Reads what [`button_id`] made, without the `games:who_said_it:` the dispatcher and the
/// games feature took off.
pub fn parse_button(action: &str) -> Option<(u32, usize, u64)> {
    let mut parts = action.strip_prefix("pick:")?.split(':');
    let game = parts.next()?.parse().ok()?;
    let round = parts.next()?.parse().ok()?;
    let person = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((game, round, person))
}

/// A question: the quote, when the answers close, and a button per person.
pub fn question(
    game: u32,
    index: usize,
    total: usize,
    round: &Round,
    ends_at: i64,
) -> (Embed, Vec<Component>) {
    let description = format!(
        "{}\n\nAnswers close <t:{ends_at}:R>.",
        quote_block(&round.quote.text)
    );
    let row = round.choices.iter().map(|person| {
        button(
            button_id(game, index, person.id),
            shorten(&person.name, 80),
            ButtonStyle::Secondary,
        )
    });
    (embed(index, total, round, description), vec![buttons(row)])
}

/// The same question once it's over: the author's button turns green, every button stops
/// working, and the text says who got it.
pub fn answer(
    game: u32,
    index: usize,
    total: usize,
    round: &Round,
    outcome: &Outcome,
) -> (Embed, Vec<Component>) {
    let mut lines = vec![
        quote_block(&round.quote.text),
        String::new(),
        format!(
            "**It was {}.** [See the message]({})",
            escape(&round.author().name),
            round.quote.link
        ),
    ];
    if !outcome.right.is_empty() {
        let right: Vec<String> = outcome
            .right
            .iter()
            .map(|(name, points)| format!("{} +{points}", escape(name)))
            .collect();
        lines.push(format!("✅ {}", right.join(", ")));
    }
    if !outcome.wrong.is_empty() {
        let wrong: Vec<String> = outcome.wrong.iter().map(|name| escape(name)).collect();
        lines.push(format!("❌ {}", wrong.join(", ")));
    }
    if outcome.right.is_empty() && outcome.wrong.is_empty() {
        lines.push("Nobody answered.".to_string());
    }
    let row = round.choices.iter().map(|person| {
        let style = if person.id == round.quote.author {
            ButtonStyle::Success
        } else {
            ButtonStyle::Secondary
        };
        button(
            button_id(game, index, person.id),
            shorten(&person.name, 80),
            style,
        )
        .disabled(true)
    });
    (
        embed(index, total, round, lines.join("\n")),
        vec![buttons(row)],
    )
}

/// The final scores, best first. `stopped` says the game ended early because nobody played.
pub fn scores(standings: &[(String, u32)], stopped: bool) -> Embed {
    let medals = ["🥇", "🥈", "🥉"];
    let mut lines: Vec<String> = standings
        .iter()
        .take(15)
        .enumerate()
        .map(|(i, (name, score))| {
            let place = medals
                .get(i)
                .map_or(format!("{}.", i + 1), |m| m.to_string());
            format!("{place} **{}** {score}", escape(name))
        })
        .collect();
    if lines.is_empty() {
        lines.push("Nobody played this time.".to_string());
    }
    if stopped {
        lines.push(String::new());
        lines.push("Nobody answered twice in a row, so I stopped there.".to_string());
    }
    EmbedBuilder::new()
        .title("Who said it? Final scores")
        .description(lines.join("\n"))
        .color(COLOR)
        .build()
}

fn embed(index: usize, total: usize, round: &Round, description: String) -> Embed {
    EmbedBuilder::new()
        .title(format!("Who said it? {}/{total}", index + 1))
        .description(description)
        .footer(EmbedFooterBuilder::new(said(&round.quote)))
        .color(COLOR)
        .build()
}

/// Where and when a quote was said: "Said in #general, March 2025", or "Said in March
/// 2025" for the channel the game is played in.
fn said(quote: &Quote) -> String {
    match &quote.place {
        Some(place) => format!("Said in #{place}, {}", quote.posted),
        None => format!("Said in {}", quote.posted),
    }
}

/// The quote as a Discord block quote, every line of it.
fn quote_block(text: &str) -> String {
    text.lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A name with Discord's formatting characters escaped, so `_moss_` doesn't turn italic.
pub(in crate::features::games) fn escape(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if matches!(
            c,
            '*' | '_' | '~' | '`' | '|' | '>' | '#' | '[' | ']' | '\\'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_ids_read_back() {
        let id = button_id(7, 3, 123456789012345678);
        let action = id.strip_prefix("games:who_said_it:").unwrap();
        assert_eq!(parse_button(action), Some((7, 3, 123456789012345678)));
        assert_eq!(parse_button("pick:7:3"), None);
        assert_eq!(parse_button("pick:7:3:1:2"), None);
        assert_eq!(parse_button("other:7:3:1"), None);
    }

    #[test]
    fn another_channel_is_named_under_the_quote() {
        let mut quote = Quote {
            author: 1,
            text: "hi there everyone".into(),
            posted: "March 2025".into(),
            place: None,
            link: String::new(),
        };
        assert_eq!(said(&quote), "Said in March 2025");
        quote.place = Some("general".into());
        assert_eq!(said(&quote), "Said in #general, March 2025");
    }

    #[test]
    fn every_line_of_a_quote_is_quoted() {
        assert_eq!(quote_block("one\ntwo"), "> one\n> two");
    }

    #[test]
    fn names_keep_their_underscores() {
        assert_eq!(escape("_moss_"), "\\_moss\\_");
        assert_eq!(escape("Cherry"), "Cherry");
    }
}
