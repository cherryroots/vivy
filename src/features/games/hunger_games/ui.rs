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

/// A tribute as shown in game messages: their mention, which Discord shows as their name
/// in blue. Game messages ping nobody (see `play::text`), so it's only a link.
pub fn mention(id: u64) -> String {
    format!("<@{id}>")
}

/// `text` with every place Vivy wrote as `#name` turned into a link to its channel, shown
/// in blue. `places` are the arena's places (name and channel ID). Longer names are tried
/// first, so `#memes-2` isn't read as `#memes`, and a name only counts when it isn't the
/// start of a longer word.
pub fn link_places(text: &str, places: &[(String, u64)]) -> String {
    let mut places: Vec<&(String, u64)> = places.iter().filter(|(n, _)| !n.is_empty()).collect();
    places.sort_by_key(|(name, _)| std::cmp::Reverse(name.len()));
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find('#') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let found = places.iter().find(|(name, _)| {
            after.starts_with(name.as_str())
                && !after[name.len()..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_')
        });
        match found {
            Some((name, id)) => {
                out.push_str(&format!("<#{id}>"));
                rest = &after[name.len()..];
            }
            None => {
                out.push('#');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A round as played: her Gamemaker line, who steps into the Octagon, the events with
/// tributes and places as blue links, and who's left. `places` are the arena's places (name
/// and channel ID). Plain message text rather than an embed, because embeds use Discord's
/// smaller font, with a blank line between events so each stands on its own. Long rounds
/// come in several parts.
pub fn round(game: &Game, round: &Round, places: &[(String, u64)]) -> Vec<String> {
    let emoji = match round.kind {
        Kind::Scatter => "🏃",
        Kind::Day(_) => "☀️",
        Kind::Night(_) => "🌙",
        Kind::Octagon(_) => "🏟️",
        Kind::Finale => "⚔️",
    };
    let name = |i: usize| {
        game.tributes
            .get(i)
            .map_or_else(|| "someone".to_string(), |t| mention(t.id))
    };
    let mut blocks = vec![format!("## {emoji} {}", title(round.kind))];
    if let Some(line) = &round.gamemaker {
        let line = shorten_at_word(&unspoken(line), 600);
        blocks.push(format!("> 🎙️ *{}*", link_places(&line, places)));
    }
    if !round.entrants.is_empty() {
        let names: Vec<String> = round.entrants.iter().map(|&i| name(i)).collect();
        blocks.push(format!(
            "🥊 Stepping into the Octagon: {}",
            names.join(", ")
        ));
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
        let line = link_places(&fill(&event.text, name), places);
        blocks.push(format!("{skull} {line}"));
    }
    if let Some(champion) = round.champion {
        blocks.push(format!("🏆 Champion of the Octagon: {}", name(champion)));
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
/// tribute who fell (user ID, kills, last words), with their kills and their last words
/// when they have some.
pub fn cannons(title: &str, fallen: &[(u64, u32, Option<LastWords>)]) -> String {
    let mut text = format!("### 💥 {title}\n");
    if fallen.is_empty() {
        text.push_str("No cannons today. Everyone made it through.");
    } else {
        let lines: Vec<String> = fallen
            .iter()
            .map(|(id, count, words)| match words {
                Some(words) => format!(
                    "💥 {}{}: “{}” [↗](<{}>)",
                    mention(*id),
                    kills(*count),
                    words.text,
                    words.link
                ),
                None => format!("💥 {}{}", mention(*id), kills(*count)),
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

/// The end: the winner, her line to them, everyone's placing (with kills and Octagons
/// won), and the cowards who never stepped into an Octagon that opened.
pub fn victor(game: &Game, victory: Option<&str>) -> Vec<String> {
    let placings = game.placings();
    let mut text = "# 👑 The Hunger Games have a victor!\n".to_string();
    if let Some(line) = victory {
        let line = shorten_at_word(&unspoken(line), 600);
        text.push_str(&format!("> 🎙️ *{line}*\n\n"));
    }
    for (i, t) in placings.iter().enumerate() {
        let place = if i == 0 {
            "👑".to_string()
        } else {
            format!("{}.", i + 1)
        };
        let octagons = game
            .tributes
            .iter()
            .position(|other| other.id == t.id)
            .map_or(0, |i| game.octagon_wins(i));
        let octagons = match octagons {
            0 => String::new(),
            1 => ", 🏟️ won the Octagon".to_string(),
            n => format!(", 🏟️ won {n} Octagons"),
        };
        text.push_str(&format!(
            "{place} {}{}{octagons}\n",
            mention(t.id),
            kills(t.kills)
        ));
    }
    let cowards: Vec<String> = game
        .cowards()
        .into_iter()
        .map(|i| mention(game.tributes[i].id))
        .collect();
    if !cowards.is_empty() {
        text.push_str(&format!(
            "\n🐔 Never dared to step into the Octagon: {}\n",
            cowards.join(", ")
        ));
    }
    text.push_str("-# Wins, kills and Octagons won are counted on `/games leaderboard`.");
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
    fn the_fallen_and_the_victor_are_shown_plainly() {
        let mut game = Game::new(vec![(1, "Moss".into()), (2, "Ash".into())]);
        game.tributes[1].died_in = Some(0);
        game.tributes[0].kills = 1;
        let text = victor(&game, Some("[booming] Well done, Moss")).join("");
        assert!(!text.contains("||"));
        assert!(text.contains("> 🎙️ *Well done, Moss*"));
        assert!(text.contains("👑 <@1>, 1 kill"));
        assert!(text.contains("2. <@2>"));
        // No Octagon was held, so nobody is a coward.
        assert!(!text.contains("🐔"));
        let words = LastWords {
            text: "brb".into(),
            link: "https://discord.com/channels/1/2/3".into(),
        };
        let text = cannons("The fallen of day 1", &[(2, 0, Some(words)), (1, 2, None)]);
        assert!(text.starts_with("### 💥 The fallen of day 1\n2 cannons sound"));
        assert!(!text.contains("||"));
        assert!(text.contains("💥 <@2>: “brb” [↗](<https://discord.com/channels/1/2/3>)"));
        assert!(text.contains("💥 <@1>, 2 kills"));
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
            entrants: Vec::new(),
            champion: None,
        };
        game.apply(played.clone());
        let parts = round(&game, &played, &[]);
        assert!(parts.len() > 1);
        assert!(parts.iter().all(|p| p.chars().count() <= DISCORD_LIMIT));
        assert!(parts[0].starts_with("## ☀️ Day 1\n\n> 🎙️ *Hello*\n\n💀 <@1> walks"));
        assert!(parts.last().unwrap().ends_with("-# 23 tributes left"));
    }

    #[test]
    fn places_become_channel_links() {
        let places = vec![("memes".to_string(), 7), ("memes-2".to_string(), 8)];
        assert_eq!(
            link_places(
                "From #memes to #memes-2, past #memesters and #nowhere.",
                &places
            ),
            "From <#7> to <#8>, past #memesters and #nowhere."
        );
        assert_eq!(link_places("#memes", &places), "<#7>");
        assert_eq!(link_places("No places", &[]), "No places");
    }

    #[test]
    fn the_octagon_shows_who_stepped_in_and_who_won() {
        let mut game = Game::new(vec![
            (1, "Moss".into()),
            (2, "Ash".into()),
            (3, "Fern".into()),
        ]);
        let played = Round {
            kind: Kind::Octagon(2),
            gamemaker: Some("[booming] The Octagon opens in #general!".into()),
            events: vec![Event {
                text: "{T1} beats {T3} in the cage.".into(),
                deaths: vec![Death {
                    victim: 2,
                    by: Some(0),
                }],
                ..Default::default()
            }],
            victory: None,
            picture: None,
            entrants: vec![0, 2],
            champion: Some(0),
        };
        game.apply(played.clone());
        let text = round(&game, &played, &[("general".into(), 5)]).join("");
        assert!(text.starts_with("## 🏟️ Day 2: the Octagon\n\n> 🎙️ *The Octagon opens in <#5>!*"));
        assert!(text.contains("🥊 Stepping into the Octagon: <@1>, <@3>"));
        assert!(text.contains("💀 <@1> beats <@3> in the cage."));
        assert!(text.contains("🏆 Champion of the Octagon: <@1>"));
    }

    #[test]
    fn the_victor_names_octagon_winners_and_cowards() {
        let mut game = Game::new(vec![
            (1, "Moss".into()),
            (2, "Ash".into()),
            (3, "Fern".into()),
            (4, "Reed".into()),
            (5, "Sage".into()),
        ]);
        let death = |victim, by| Event {
            text: String::new(),
            deaths: vec![Death { victim, by }],
            ..Default::default()
        };
        let round = |kind, events| Round {
            kind,
            gamemaker: None,
            events,
            victory: None,
            picture: None,
            entrants: Vec::new(),
            champion: None,
        };
        // Sage dies before the Octagon, so isn't a coward; Reed steps in and dies there.
        game.apply(round(Kind::Day(1), vec![death(4, None)]));
        game.apply(Round {
            entrants: vec![0, 3],
            champion: Some(0),
            ..round(Kind::Octagon(1), vec![death(3, Some(0))])
        });
        game.apply(round(Kind::Night(1), vec![death(2, None)]));
        game.apply(round(Kind::Finale, vec![death(1, Some(0))]));
        assert_eq!(game.cowards(), vec![1, 2]);
        let text = victor(&game, None).join("");
        assert!(text.contains("👑 <@1>, 2 kills, 🏟️ won the Octagon\n"));
        assert!(text.contains("🐔 Never dared to step into the Octagon: <@2>, <@3>\n"));
        assert!(text.ends_with("Octagons won are counted on `/games leaderboard`."));
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
