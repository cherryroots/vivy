//! The game's messages: the Reaping (the lobby), each round, the cannons for the fallen,
//! and the victor. Everything here takes plain values and builds messages; nothing talks
//! to Discord.

use serenity::all::{
    ButtonStyle, CreateActionRow, CreateButton, CreateEmbed, CreateEmbedFooter, CreateSelectMenu,
    CreateSelectMenuKind, UserId,
};

use super::game::{Game, Kind, Round, fill};
use super::writer::title;
use crate::features::who_said_it::ui::{COLOR, escape};
use crate::util::shorten_at_word;

/// The longest a round's text may be. Discord allows 4096 in an embed.
const MAX_ROUND: usize = 3900;

/// What a button or menu does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// The host's menu of tributes.
    Pick,
    Volunteer,
    Start,
    Cancel,
    /// On to the round after `n` (the round's index).
    Next(usize),
}

/// A button's or menu's ID: `who_said_it:hunger:<game>:<action>`. `game` tells a game from
/// an older one in the same channel.
pub fn custom_id(game: u32, action: Action) -> String {
    let action = match action {
        Action::Pick => "pick".to_string(),
        Action::Volunteer => "volunteer".to_string(),
        Action::Start => "start".to_string(),
        Action::Cancel => "cancel".to_string(),
        Action::Next(round) => format!("next:{round}"),
    };
    format!("who_said_it:hunger:{game}:{action}")
}

/// Reads what [`custom_id`] made, without the `who_said_it:` the dispatcher took off.
pub fn parse_id(action: &str) -> Option<(u32, Action)> {
    let rest = action.strip_prefix("hunger:")?;
    let (game, what) = rest.split_once(':')?;
    let game = game.parse().ok()?;
    let action = match what {
        "pick" => Action::Pick,
        "volunteer" => Action::Volunteer,
        "start" => Action::Start,
        "cancel" => Action::Cancel,
        _ => Action::Next(what.strip_prefix("next:")?.parse().ok()?),
    };
    Some((game, action))
}

/// The Reaping: who's in so far, the host's menu and the buttons. `closes_at` is a Unix
/// time for Discord's countdown.
pub fn lobby(
    game: u32,
    host: u64,
    tributes: &[(u64, String)],
    picked: &[u64],
    max: usize,
    closes_at: i64,
) -> (CreateEmbed, Vec<CreateActionRow>) {
    let mut lines = vec![
        format!(
            "<@{host}> is hosting. The host picks tributes from the menu, and anyone can press \
             **Volunteer** to enter (press it again to back out). The host presses **Start** \
             when everyone's in: 2 to {max} tributes."
        ),
        String::new(),
        format!("**Tributes ({}):**", tributes.len()),
    ];
    if tributes.is_empty() {
        lines.push("Nobody yet.".to_string());
    }
    for (i, (_, name)) in tributes.iter().enumerate() {
        lines.push(format!("{}. {}", i + 1, escape(name)));
    }
    lines.push(String::new());
    lines.push(format!("The Reaping closes <t:{closes_at}:R>."));
    let menu = CreateSelectMenu::new(
        custom_id(game, Action::Pick),
        CreateSelectMenuKind::User {
            default_users: Some(picked.iter().map(|id| UserId::new(*id)).collect()),
        },
    )
    .placeholder("Host: pick the tributes")
    .min_values(0)
    .max_values(max.clamp(1, 25) as u8);
    let buttons = vec![
        CreateButton::new(custom_id(game, Action::Volunteer))
            .label("Volunteer")
            .emoji('🙋')
            .style(ButtonStyle::Primary),
        CreateButton::new(custom_id(game, Action::Start))
            .label("Start")
            .style(ButtonStyle::Success),
        CreateButton::new(custom_id(game, Action::Cancel))
            .label("Cancel")
            .style(ButtonStyle::Danger),
    ];
    let embed = CreateEmbed::new()
        .title("🏹 The Hunger Games: the Reaping")
        .description(lines.join("\n"))
        .color(COLOR);
    (
        embed,
        vec![
            CreateActionRow::SelectMenu(menu),
            CreateActionRow::Buttons(buttons),
        ],
    )
}

/// The Reaping once it's over: who entered, and no more buttons.
pub fn lobby_closed(tributes: &[(u64, String)], why: &str) -> CreateEmbed {
    let names: Vec<String> = tributes.iter().map(|(_, n)| escape(n)).collect();
    let mut text = why.to_string();
    if !names.is_empty() {
        text.push_str(&format!("\n\n**Tributes:** {}", names.join(", ")));
    }
    CreateEmbed::new()
        .title("🏹 The Hunger Games: the Reaping")
        .description(text)
        .color(COLOR)
}

/// A round as played: her Gamemaker line, the events with names filled in, and who's left.
pub fn round(game: &Game, round: &Round) -> CreateEmbed {
    let emoji = match round.kind {
        Kind::Bloodbath => "🩸",
        Kind::Day(_) => "☀️",
        Kind::Night(_) => "🌙",
        Kind::Feast(_) => "🍖",
        Kind::Finale => "⚔️",
    };
    let name = |i: usize| {
        game.tributes.get(i).map_or_else(
            || "someone".to_string(),
            |t| format!("**{}**", escape(&t.name)),
        )
    };
    let mut events: Vec<String> = round
        .events
        .iter()
        .map(|e| {
            let skull = if e.deaths.is_empty() {
                "▫️"
            } else {
                "💀"
            };
            format!("{skull} {}", fill(&e.text, name))
        })
        .collect();
    let mut head = String::new();
    if let Some(line) = &round.gamemaker {
        head = format!("🎙️ *{}*\n\n", shorten_at_word(line, 600));
    }
    // Very wordy rounds are cut to fit, each event getting an equal share.
    let total: usize =
        head.chars().count() + events.iter().map(|e| e.chars().count() + 1).sum::<usize>();
    if total > MAX_ROUND {
        let each = (MAX_ROUND - head.chars().count()) / events.len().max(1) - 1;
        events = events.iter().map(|e| shorten_at_word(e, each)).collect();
    }
    let left = game.alive();
    CreateEmbed::new()
        .title(format!("{emoji} {}", title(round.kind)))
        .description(format!("{head}{}", events.join("\n")))
        .footer(CreateEmbedFooter::new(if left == 1 {
            "1 tribute left".to_string()
        } else {
            format!("{left} tributes left")
        }))
        .color(COLOR)
}

/// The button to move on to the next round.
pub fn next_button(game: u32, round: usize) -> Vec<CreateActionRow> {
    vec![CreateActionRow::Buttons(vec![
        CreateButton::new(custom_id(game, Action::Next(round)))
            .label("Next")
            .emoji('⏭')
            .style(ButtonStyle::Primary),
    ])]
}

/// The cannons at the end of a day: one for each tribute who fell.
pub fn cannons(day: u32, fallen: &[String]) -> CreateEmbed {
    let text = if fallen.is_empty() {
        "No cannons today. Everyone made it through.".to_string()
    } else {
        let names: Vec<String> = fallen.iter().map(|n| format!("💥 {}", escape(n))).collect();
        format!(
            "{} cannon{} sound{} across the arena.\n\n{}",
            fallen.len(),
            if fallen.len() == 1 { "" } else { "s" },
            if fallen.len() == 1 { "s" } else { "" },
            names.join("\n")
        )
    };
    CreateEmbed::new()
        .title(format!("The fallen of day {day}"))
        .description(text)
        .color(COLOR)
}

/// The end: the winner, her line to them, and everyone's placing.
pub fn victor(game: &Game, victory: Option<&str>) -> CreateEmbed {
    let placings = game.placings();
    let mut lines = Vec::new();
    if let Some(line) = victory {
        lines.push(format!("🎙️ *{}*", shorten_at_word(line, 600)));
        lines.push(String::new());
    }
    for (i, t) in placings.iter().enumerate() {
        let place = if i == 0 {
            "👑".to_string()
        } else {
            format!("{}.", i + 1)
        };
        let kills = match t.kills {
            0 => String::new(),
            1 => ", 1 kill".to_string(),
            n => format!(", {n} kills"),
        };
        lines.push(format!("{place} {}{kills}", escape(&t.name)));
    }
    let name = placings.first().map_or("Nobody", |t| t.name.as_str());
    CreateEmbed::new()
        .title(format!("👑 {} wins the Hunger Games!", name))
        .description(lines.join("\n"))
        .color(COLOR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::who_said_it::hunger::game::{Death, Event};

    #[test]
    fn ids_read_back() {
        for action in [
            Action::Pick,
            Action::Volunteer,
            Action::Start,
            Action::Cancel,
            Action::Next(4),
        ] {
            let id = custom_id(9, action);
            let rest = id.strip_prefix("who_said_it:").unwrap();
            assert_eq!(parse_id(rest), Some((9, action)));
        }
        assert_eq!(parse_id("pick:1:2:3"), None);
        assert_eq!(parse_id("hunger:9:next:x"), None);
        assert_eq!(parse_id("hunger:9:dance"), None);
    }

    #[test]
    fn long_rounds_are_cut_to_fit() {
        let mut game = Game::new((1..=24).map(|id| (id, format!("P{id}"))).collect());
        let events = (0..24)
            .map(|i| Event {
                text: format!("{{T{}}} {}", i + 1, "walks a long way ".repeat(23)),
                deaths: if i == 0 {
                    vec![Death {
                        victim: 0,
                        by: None,
                    }]
                } else {
                    Vec::new()
                },
            })
            .collect();
        let played = Round {
            kind: Kind::Day(1),
            gamemaker: Some("Hello".into()),
            events,
            victory: None,
        };
        game.apply(played.clone());
        let embed = serde_json::to_value(round(&game, &played)).unwrap();
        let text = embed["description"].as_str().unwrap();
        assert!(text.chars().count() <= 4096);
        assert!(text.starts_with("🎙️ *Hello*\n\n💀 **P1** walks"));
        assert_eq!(embed["footer"]["text"], "23 tributes left");
    }
}
