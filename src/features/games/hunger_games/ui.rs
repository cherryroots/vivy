//! The game's messages: the Reaping (the lobby), each round, the cannons for the fallen,
//! and the victor. Everything here takes plain values and builds messages; nothing talks
//! to Discord.

use serenity::all::{
    ButtonStyle, CreateActionRow, CreateButton, CreateEmbed, CreateSelectMenu,
    CreateSelectMenuKind, UserId,
};

use super::game::{Game, Kind, Round, fill};
use super::last_words::LastWords;
use super::writer::title;
use crate::features::games::who_said_it::ui::{COLOR, escape};
use crate::util::shorten_at_word;
use crate::util::split::{DISCORD_LIMIT, split_message};

/// What a button or menu does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// The host's menu of tributes.
    Pick,
    Volunteer,
    Start,
    Cancel,
    /// The host turns Vivy's memory notes about the tributes on or off.
    Memories,
    /// The host turns the pictures she draws during the game on or off.
    Pictures,
}

/// A button's or menu's ID: `games:hunger:<game>:<action>`. `game` tells a game from
/// an older one in the same channel.
pub fn custom_id(game: u32, action: Action) -> String {
    let action = match action {
        Action::Pick => "pick".to_string(),
        Action::Volunteer => "volunteer".to_string(),
        Action::Start => "start".to_string(),
        Action::Cancel => "cancel".to_string(),
        Action::Memories => "memories".to_string(),
        Action::Pictures => "pictures".to_string(),
    };
    format!("games:hunger:{game}:{action}")
}

/// Reads what [`custom_id`] made, without the `games:` the dispatcher took off.
pub fn parse_id(action: &str) -> Option<(u32, Action)> {
    let rest = action.strip_prefix("hunger:")?;
    let (game, what) = rest.split_once(':')?;
    let game = game.parse().ok()?;
    let action = match what {
        "pick" => Action::Pick,
        "volunteer" => Action::Volunteer,
        "start" => Action::Start,
        "cancel" => Action::Cancel,
        "memories" => Action::Memories,
        "pictures" => Action::Pictures,
        _ => return None,
    };
    Some((game, action))
}

/// What the host turned on in the Reaping. Both start off.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Extras {
    /// Vivy may use her memory notes about the tributes.
    pub memories: bool,
    /// She draws pictures during the game.
    pub pictures: bool,
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
    extras: Extras,
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
    lines.push(if extras.memories {
        "🧠 Memories **on**: Vivy may work in fun things she remembers about the tributes."
            .to_string()
    } else {
        "🧠 Memories **off**: Vivy knows the tributes only by name.".to_string()
    });
    lines.push(if extras.pictures {
        "🎨 Pictures **on**: Vivy draws a map of the arena after every round.".to_string()
    } else {
        "🎨 Pictures **off**: the Games are told in words only.".to_string()
    });
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
        CreateButton::new(custom_id(game, Action::Memories))
            .label(if extras.memories {
                "Memories: on"
            } else {
                "Memories: off"
            })
            .emoji('🧠')
            .style(ButtonStyle::Secondary),
        CreateButton::new(custom_id(game, Action::Pictures))
            .label(if extras.pictures {
                "Pictures: on"
            } else {
                "Pictures: off"
            })
            .emoji('🎨')
            .style(ButtonStyle::Secondary),
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
/// Plain message text rather than an embed, because embeds use Discord's smaller font, with a
/// blank line between events so each stands on its own. Long rounds come in several parts.
pub fn round(game: &Game, round: &Round) -> Vec<String> {
    let emoji = match round.kind {
        Kind::Scatter => "🏃",
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
    let mut blocks = vec![format!("## {emoji} {}", title(round.kind))];
    if let Some(line) = &round.gamemaker {
        blocks.push(format!("> 🎙️ *{}*", shorten_at_word(&unspoken(line), 600)));
    }
    for event in &round.events {
        // 💀 when someone dies, 🗡️ when someone gets a weapon.
        let skull = if !event.deaths.is_empty() {
            "💀"
        } else if !event.finds.is_empty() {
            "🗡️"
        } else {
            "▫️"
        };
        blocks.push(format!("{skull} {}", fill(&event.text, name)));
    }
    let left = game.alive();
    blocks.push(if left == 1 {
        "-# 1 tribute left".to_string()
    } else {
        format!("-# {left} tributes left")
    });
    split_message(&blocks.join("\n\n"), DISCORD_LIMIT)
}

/// A Gamemaker line as it's shown: without the audio tags (`[booming]`) that steer how
/// it's spoken.
pub fn unspoken(line: &str) -> String {
    let mut shown = String::new();
    let mut rest = line;
    while let Some(start) = rest.find('[') {
        let Some(end) = rest[start..].find(']') else {
            break;
        };
        shown.push_str(&rest[..start]);
        rest = &rest[start + end + 1..];
    }
    shown.push_str(rest);
    shown.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The cannons at the end of a day (`title` like "The fallen of day 2"): one for each
/// tribute who fell (name, kills, last words), with their kills and their last words when
/// they have some, all behind spoilers so people still reading the rounds don't find out
/// early.
pub fn cannons(title: &str, fallen: &[(String, u32, Option<LastWords>)]) -> String {
    let mut text = format!("### 💥 {title}\n");
    if fallen.is_empty() {
        text.push_str("No cannons today. Everyone made it through.");
    } else {
        let lines: Vec<String> = fallen
            .iter()
            .map(|(name, count, words)| match words {
                Some(words) => format!(
                    "💥 ||**{}**{}: “{}” [↗](<{}>)||",
                    escape(name),
                    kills(*count),
                    words.text,
                    words.link
                ),
                None => format!("💥 ||**{}**{}||", escape(name), kills(*count)),
            })
            .collect();
        text.push_str(&format!(
            "{} cannon{} sound{} across the arena.\n\n{}",
            fallen.len(),
            if fallen.len() == 1 { "" } else { "s" },
            if fallen.len() == 1 { "s" } else { "" },
            lines.join("\n")
        ));
    }
    text
}

/// The end: the winner, her line to them, and everyone's placing, all behind spoilers so
/// people still reading the last rounds don't find out who won.
pub fn victor(game: &Game, victory: Option<&str>) -> Vec<String> {
    let placings = game.placings();
    let mut text = "# 👑 The Hunger Games have a victor!\n".to_string();
    if let Some(line) = victory {
        // A `||` of her own would end the spoiler early.
        let line = shorten_at_word(&unspoken(line), 600).replace("||", "|");
        text.push_str(&format!("> 🎙️ ||*{line}*||\n\n"));
    }
    for (i, t) in placings.iter().enumerate() {
        let place = if i == 0 {
            "👑".to_string()
        } else {
            format!("{}.", i + 1)
        };
        text.push_str(&format!(
            "{place} ||{}{}||\n",
            escape(&t.name),
            kills(t.kills)
        ));
    }
    text.push_str("-# Wins and kills are counted on `/games leaderboard`.");
    split_message(&text, DISCORD_LIMIT)
}

/// ", 2 kills" after a name, or nothing for none.
fn kills(count: u32) -> String {
    match count {
        0 => String::new(),
        1 => ", 1 kill".to_string(),
        n => format!(", {n} kills"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::games::hunger_games::game::{Death, Event};

    #[test]
    fn ids_read_back() {
        for action in [
            Action::Pick,
            Action::Volunteer,
            Action::Start,
            Action::Cancel,
            Action::Memories,
            Action::Pictures,
        ] {
            let id = custom_id(9, action);
            let rest = id.strip_prefix("games:").unwrap();
            assert_eq!(parse_id(rest), Some((9, action)));
        }
        assert_eq!(parse_id("pick:1:2:3"), None);
        assert_eq!(parse_id("hunger:9:next:4"), None);
        assert_eq!(parse_id("hunger:9:dance"), None);
    }

    #[test]
    fn the_fallen_and_the_victor_are_spoilers() {
        let mut game = Game::new(vec![(1, "Moss".into()), (2, "Ash".into())]);
        game.tributes[1].died_in = Some(0);
        game.tributes[0].kills = 1;
        let text = victor(&game, Some("Well done, Moss || hi")).join("");
        assert!(!text.lines().next().unwrap().contains("Moss"));
        assert!(text.contains("> 🎙️ ||*Well done, Moss | hi*||"));
        assert!(text.contains("👑 ||Moss, 1 kill||"));
        assert!(text.contains("2. ||Ash||"));
        let words = LastWords {
            text: "brb".into(),
            link: "https://discord.com/channels/1/2/3".into(),
        };
        let text = cannons(
            "The fallen of day 1",
            &[("Ash".into(), 0, Some(words)), ("Moss".into(), 2, None)],
        );
        assert!(text.starts_with("### 💥 The fallen of day 1\n2 cannons sound"));
        assert!(text.contains("💥 ||**Ash**: “brb” [↗](<https://discord.com/channels/1/2/3>)||"));
        assert!(text.contains("💥 ||**Moss**, 2 kills||"));
    }

    #[test]
    fn long_rounds_come_in_parts() {
        let mut game = Game::new((1..=24).map(|id| (id, format!("P{id}"))).collect());
        let events = (0..24)
            .map(|i| Event {
                text: format!("{{T{}}} {}", i + 1, "walks a long way ".repeat(10)),
                deaths: if i == 0 {
                    vec![Death {
                        victim: 0,
                        by: None,
                    }]
                } else {
                    Vec::new()
                },
                ..Default::default()
            })
            .collect();
        let played = Round {
            kind: Kind::Day(1),
            gamemaker: Some("Hello".into()),
            events,
            victory: None,
            picture: None,
        };
        game.apply(played.clone());
        let parts = round(&game, &played);
        assert!(parts.len() > 1);
        assert!(parts.iter().all(|p| p.chars().count() <= DISCORD_LIMIT));
        assert!(parts[0].starts_with("## ☀️ Day 1\n\n> 🎙️ *Hello*\n\n💀 **P1** walks"));
        assert!(parts.last().unwrap().ends_with("-# 23 tributes left"));
    }

    #[test]
    fn audio_tags_arent_shown() {
        assert_eq!(
            unspoken("[booming] Welcome, tributes! [dramatic pause] Begin."),
            "Welcome, tributes! Begin."
        );
        assert_eq!(unspoken("No tags [at all"), "No tags [at all");
    }
}
