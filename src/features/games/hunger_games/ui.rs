//! The game's messages: the Reaping (the lobby), each round, the cannons for the fallen,
//! and the victor. Everything here takes plain values and builds messages; nothing talks
//! to Discord.

use serenity::all::{
    ButtonStyle, CreateActionRow, CreateButton, CreateEmbed, CreateSelectMenu,
    CreateSelectMenuKind, ReactionType, UserId,
};

use super::game::{Blow, Endgame, FEWEST_FOR_TEAMS, Game, Kind, Reward, fill};
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
    /// The host turns a team Games (pairs who win together) on or off.
    Teams,
    /// A vote for the prize of the coming Octagon.
    Prize(Reward),
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
        Action::Teams => "teams".to_string(),
        Action::Prize(reward) => format!("prize-{}", reward_key(reward)),
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
        "teams" => Action::Teams,
        _ => Action::Prize(
            Reward::ALL
                .into_iter()
                .find(|&r| what.strip_prefix("prize-") == Some(reward_key(r)))?,
        ),
    };
    Some((game, action))
}

fn reward_key(reward: Reward) -> &'static str {
    match reward {
        Reward::Weapon => "weapon",
        Reward::Armour => "armour",
        Reward::Hunt => "hunt",
    }
}

/// The vote for the coming Octagon's prize: each prize with what it does and its votes so
/// far, a countdown to `closes_at` (a Unix time), and a button for each. Plain text, like
/// the rounds.
pub fn prize_vote(
    game: u32,
    votes: &[(Reward, usize)],
    closes_at: i64,
) -> (String, Vec<CreateActionRow>) {
    let mut text = format!(
        "## 🏟️ The Octagon opens next!\nVote for the prize its champion takes. Voting closes <t:{closes_at}:R>.\n"
    );
    for &(reward, count) in votes {
        text.push_str(&format!(
            "\n{} **{}**: {} · {}",
            reward.emoji(),
            reward.name(),
            reward.blurb(),
            count_votes(count)
        ));
    }
    let buttons = Reward::ALL
        .into_iter()
        .map(|r| {
            CreateButton::new(custom_id(game, Action::Prize(r)))
                .label(r.name())
                .emoji(ReactionType::Unicode(r.emoji().to_string()))
                .style(ButtonStyle::Primary)
        })
        .collect();
    (text, vec![CreateActionRow::Buttons(buttons)])
}

/// The vote once it's closed: what won, and the count for each.
pub fn prize_chosen(won: Reward, votes: &[(Reward, usize)], random: bool) -> String {
    let counts: Vec<String> = votes
        .iter()
        .map(|&(r, n)| format!("{} {}", r.emoji(), n))
        .collect();
    let how = if random {
        "Nobody could agree, so Vivy picked"
    } else {
        "The crowd has spoken"
    };
    format!(
        "## 🏟️ The Octagon's prize\n{how}: {} **{}** ({}).\n-# Votes: {}",
        won.emoji(),
        won.name(),
        won.blurb(),
        counts.join(" · ")
    )
}

fn count_votes(n: usize) -> String {
    match n {
        1 => "1 vote".to_string(),
        n => format!("{n} votes"),
    }
}

/// What the host turned on in the Reaping. All start off.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Extras {
    /// Vivy may use her memory notes about the tributes.
    pub memories: bool,
    /// She draws pictures during the game.
    pub pictures: bool,
    /// The tributes fight in random pairs and win together.
    pub teams: bool,
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
    lines.push(if extras.teams {
        format!(
            "👥 Teams **on**: the tributes fight in random pairs, and a pair wins together (with {FEWEST_FOR_TEAMS} or more tributes)."
        )
    } else {
        "👥 Teams **off**: every tribute for themselves.".to_string()
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
    // Discord fits five buttons in a row: entering and starting in one, the host's
    // toggles in the next.
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
    let toggles = vec![
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
        CreateButton::new(custom_id(game, Action::Teams))
            .label(if extras.teams {
                "Teams: on"
            } else {
                "Teams: off"
            })
            .emoji('👥')
            .style(ButtonStyle::Secondary),
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
            CreateActionRow::Buttons(toggles),
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
    let places = longest_first(places);
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find('#') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        match place_at(after, &places) {
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

/// The channel ID of the first place `text` names as `#name`, if any (see
/// [`link_places`]).
pub fn first_place(text: &str, places: &[(String, u64)]) -> Option<u64> {
    let places = longest_first(places);
    text.match_indices('#')
        .find_map(|(at, _)| place_at(&text[at + 1..], &places))
        .map(|(_, id)| *id)
}

/// The places with names, longest first, so `#memes-2` isn't read as `#memes`.
fn longest_first(places: &[(String, u64)]) -> Vec<&(String, u64)> {
    let mut places: Vec<&(String, u64)> = places.iter().filter(|(n, _)| !n.is_empty()).collect();
    places.sort_by_key(|(name, _)| std::cmp::Reverse(name.len()));
    places
}

/// The place whose name `after` (the text after a `#`) starts with, when the name isn't
/// the start of a longer word.
fn place_at<'a>(after: &str, places: &[&'a (String, u64)]) -> Option<&'a (String, u64)> {
    places.iter().copied().find(|(name, _)| {
        after.starts_with(name.as_str())
            && !after[name.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_')
    })
}

/// The round at `index` as played: her Gamemaker line, the places that close, who steps
/// into the Octagon, the events with tributes and places as blue links, and who's left.
/// `places` are the arena's places (name and channel ID). Plain message text rather than
/// an embed, because embeds use Discord's smaller font, with a blank line between events so
/// each stands on its own. Long rounds come in several parts.
pub fn round(game: &Game, index: usize, places: &[(String, u64)]) -> Vec<String> {
    let round = &game.rounds[index];
    // The showdown, or the round before it where the last two met and settled it.
    let ended = index + 1 == game.rounds.len() && !game.winners().is_empty();
    let showdown = ended || game.endgame(index) == Some(Endgame::Showdown);
    let emoji = match round.kind {
        _ if showdown => "⚔️",
        Kind::Scatter => "🏃",
        Kind::Day(_) => "☀️",
        Kind::Night(_) => "🌙",
        Kind::Octagon(_) => "🏟️",
    };
    let name = |i: usize| {
        game.tributes
            .get(i)
            .map_or_else(|| "someone".to_string(), |t| mention(t.id))
    };
    let mut heading = format!("## {emoji} {}", title(round.kind));
    if showdown {
        heading.push_str(": the showdown");
    }
    let mut blocks = vec![heading];
    // The season's theme as the Games open; her surprise stays hers.
    if let Some(theme) = &round.theme {
        let mut line = format!("🎬 **{}**", escape(&theme.name));
        if !theme.twist.is_empty() {
            line.push_str(&format!(": {}", link_places(&theme.twist, places)));
        }
        blocks.push(line);
    }
    if let Some(line) = &round.gamemaker {
        let line = shorten_at_word(&unspoken(line), 600);
        blocks.push(format!("> 🎙️ *{}*", link_places(&line, places)));
    }
    let closing: Vec<String> = places
        .get(game.closing(index))
        .unwrap_or_default()
        .iter()
        .map(|(_, id)| format!("<#{id}>"))
        .collect();
    if !closing.is_empty() {
        blocks.push(format!(
            "🔥 The arena shrinks: walls of fire close off {}.",
            and_list(&closing)
        ));
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
        let mut block = format!("{skull} {line}");
        // A grudge from earlier Games, settled.
        for death in &event.deaths {
            if let Some(grudge) = death.by.and_then(|by| game.revenge(by, death.victim)) {
                let how = how(grudge.last.as_ref());
                let how = if how.is_empty() {
                    how
                } else {
                    format!(",{how}")
                };
                block.push_str(&format!(
                    "\n-# 😈 Revenge: {} killed {} in an earlier Games{how}.",
                    name(grudge.killer),
                    name(grudge.victim)
                ));
            }
        }
        blocks.push(block);
    }
    if let Some(champion) = round.champion {
        let prize = round
            .reward
            .map(|r| format!(", winning {} {}", r.emoji(), r.name().to_lowercase()))
            .unwrap_or_default();
        blocks.push(format!(
            "🏆 Champion of the Octagon: {}{prize}",
            name(champion)
        ));
    }
    let left = game.alive();
    let mut count = if left == 1 {
        "-# 1 tribute left".to_string()
    } else {
        format!("-# {left} tributes left")
    };
    if game.teams() {
        count.push_str(&match game.sides() {
            1 => " (1 team)".to_string(),
            n => format!(" ({n} teams)"),
        });
    }
    blocks.push(count);
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

/// The end: the winners (one, or a team of two), her line to them, everyone's placing
/// (with kills and Octagons won), and the cowards who never stepped into an Octagon that
/// opened.
pub fn victor(game: &Game, victory: Option<&str>) -> Vec<String> {
    let winners = game.winners();
    let mut text = if winners.len() > 1 {
        "# 👑 The Hunger Games have a winning team!\n".to_string()
    } else {
        "# 👑 The Hunger Games have a victor!\n".to_string()
    };
    if let Some(line) = victory {
        let line = shorten_at_word(&unspoken(line), 600);
        text.push_str(&format!("> 🎙️ *{line}*\n\n"));
    }
    for (place, i) in game.placings().into_iter().enumerate() {
        let t = &game.tributes[i];
        let won = winners.contains(&i);
        let place = if won {
            "👑".to_string()
        } else {
            format!("{}.", place + 1)
        };
        let octagons = match game.octagon_wins(i) {
            0 => String::new(),
            1 => ", 🏟️ won the Octagon".to_string(),
            n => format!(", 🏟️ won {n} Octagons"),
        };
        // A winner whose teammate carried the team home.
        let fell = if won && !t.alive() {
            ", fell on the way"
        } else {
            ""
        };
        text.push_str(&format!(
            "{place} {}{}{octagons}{fell}\n",
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

/// The teams as the Games begin, like "👥 <@1> & <@2>", with the odd one out on their
/// own, or `None` in a solo Games.
pub fn teams(game: &Game) -> Option<String> {
    if !game.teams() {
        return None;
    }
    let mut lines = Vec::new();
    for (i, t) in game.tributes.iter().enumerate() {
        match t.partner {
            Some(p) if p > i => lines.push(format!(
                "👥 {} & {}",
                mention(t.id),
                mention(game.tributes[p].id)
            )),
            Some(_) => {}
            None => lines.push(format!("👤 {} fights alone", mention(t.id))),
        }
    }
    Some(format!("**The teams:**\n{}", lines.join("\n")))
}

/// The grudges from earlier Games as the Games begin, like "😈 <@1> has killed <@2> twice
/// before", or `None` when there are none.
pub fn grudges(game: &Game) -> Option<String> {
    let lines: Vec<String> = game
        .open_grudges()
        .iter()
        .map(|g| {
            let last = how(g.last.as_ref());
            let last = if last.is_empty() {
                last
            } else {
                format!(", the last time{last}")
            };
            format!(
                "😈 {} has killed {} {} before{last}.",
                mention(game.tributes[g.killer].id),
                mention(game.tributes[g.victim].id),
                g.how_often()
            )
        })
        .collect();
    (!lines.is_empty()).then(|| format!("**Old grudges:**\n{}", lines.join("\n")))
}

/// How a weapon reads after a kill: "with a spoon", "with an axe", "with Excalibur",
/// "with Moss's spoon", or "with their bare hands" for none.
pub fn with_weapon(weapon: &str) -> String {
    let weapon = weapon.trim();
    if weapon.is_empty() {
        return "with their bare hands".to_string();
    }
    let first = weapon.split_whitespace().next().unwrap_or_default();
    let named = ["a", "an", "the", "their", "his", "her", "its"]
        .contains(&first.to_lowercase().as_str())
        || first.contains(['\'', '’'])
        || first.starts_with(char::is_uppercase);
    if named {
        return format!("with {weapon}");
    }
    let article = if weapon.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    };
    format!("with {article} {weapon}")
}

/// How a kill from earlier Games happened, like " with a spoon in <#5>" (with a leading
/// space), or "" when it's from before kills kept it.
fn how(blow: Option<&Blow>) -> String {
    let Some(blow) = blow else {
        return String::new();
    };
    let mut text = format!(
        " {}",
        with_weapon(blow.weapon.as_deref().unwrap_or_default())
    );
    if blow.octagon {
        text.push_str(" in the Octagon");
    } else if let Some(place) = blow.place {
        text.push_str(&format!(" in <#{place}>"));
    }
    text
}

/// `a`, `a and b`, or `a, b and c`.
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
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
    use crate::features::games::hunger_games::game::{Death, Event, Round};

    #[test]
    fn ids_read_back() {
        for action in [
            Action::Pick,
            Action::Volunteer,
            Action::Start,
            Action::Cancel,
            Action::Memories,
            Action::Pictures,
            Action::Teams,
            Action::Prize(Reward::Weapon),
            Action::Prize(Reward::Armour),
            Action::Prize(Reward::Hunt),
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
            reward: None,
            theme: None,
        };
        game.apply(played);
        let parts = round(&game, 0, &[]);
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
            reward: Some(Reward::Armour),
            theme: None,
        };
        game.apply(played);
        let text = round(&game, 0, &[("general".into(), 5)]).join("");
        assert!(text.starts_with("## 🏟️ Day 2: the Octagon\n\n> 🎙️ *The Octagon opens in <#5>!*"));
        assert!(text.contains("🥊 Stepping into the Octagon: <@1>, <@3>"));
        assert!(text.contains("💀 <@1> beats <@3> in the cage."));
        assert!(text.contains("🏆 Champion of the Octagon: <@1>, winning 🛡️ golden armour"));
    }

    #[test]
    fn weapons_read_naturally() {
        assert_eq!(with_weapon("spoon"), "with a spoon");
        assert_eq!(with_weapon("axe"), "with an axe");
        assert_eq!(with_weapon("Excalibur"), "with Excalibur");
        assert_eq!(with_weapon("Moss's spoon"), "with Moss's spoon");
        assert_eq!(with_weapon("the ban hammer"), "with the ban hammer");
        assert_eq!(with_weapon(" "), "with their bare hands");
        let cage = Blow {
            weapon: None,
            place: Some(5),
            octagon: true,
        };
        assert_eq!(how(Some(&cage)), " with their bare hands in the Octagon");
        assert_eq!(how(None), "");
    }

    #[test]
    fn the_scatter_shows_the_theme_but_not_the_surprise() {
        let mut game = Game::new(vec![(1, "Moss".into()), (2, "Ash".into())]);
        game.apply(Round {
            kind: Kind::Scatter,
            gamemaker: None,
            events: vec![Event {
                text: "{T1} and {T2} run.".into(),
                ..Default::default()
            }],
            victory: None,
            picture: None,
            entrants: Vec::new(),
            champion: None,
            reward: None,
            theme: Some(crate::features::games::hunger_games::game::Theme {
                name: "The *Static* Season".into(),
                twist: "Acid rain falls on #memes.".into(),
                surprise: "The Octagon floods.".into(),
            }),
        });
        let text = round(&game, 0, &[("memes".into(), 5)]).join("");
        assert!(
            text.contains("🎬 **The \\*Static\\* Season**: Acid rain falls on <#5>."),
            "{text}"
        );
        assert!(!text.contains("floods"));
    }

    #[test]
    fn rounds_show_the_arena_shrinking_and_revenge() {
        let mut game = Game::new((1..=8).map(|id| (id, format!("P{id}"))).collect());
        let places: Vec<(String, u64)> = (0..8).map(|i| (format!("room-{i}"), 10 + i)).collect();
        game.places = places.len();
        // P7 killed P6 in an earlier Games, with a pushpin spike in #room-2.
        let pushpin = Blow {
            weapon: Some("pushpin spike".into()),
            place: Some(12),
            octagon: false,
        };
        game.remember(&[(7, 6, 1, Some(pushpin))]);
        assert_eq!(
            grudges(&game).unwrap(),
            "**Old grudges:**\n😈 <@7> has killed <@6> once before, the last time with a pushpin spike in <#12>."
        );
        let day = |kind: Kind, events: Vec<Event>| Round {
            kind,
            gamemaker: None,
            events,
            victory: None,
            picture: None,
            entrants: Vec::new(),
            champion: None,
            reward: None,
            theme: None,
        };
        let dies = |victim: usize, by: Option<usize>| Event {
            text: format!("{{T{}}} falls.", victim + 1),
            deaths: vec![Death { victim, by }],
            ..Default::default()
        };
        game.apply(day(Kind::Night(1), (0..5).map(|i| dies(i, None)).collect()));
        // Now P6 gets their revenge, as day 2 closes five places.
        game.apply(day(
            Kind::Day(2),
            vec![Event {
                text: "{T6} gets {T7} back.".into(),
                deaths: vec![Death {
                    victim: 6,
                    by: Some(5),
                }],
                ..Default::default()
            }],
        ));
        let text = round(&game, 1, &places).join("");
        assert!(text.contains(
            "🔥 The arena shrinks: walls of fire close off <#13>, <#14>, <#15>, <#16> and <#17>."
        ));
        assert!(text.contains(
            "💀 <@6> gets <@7> back.\n-# 😈 Revenge: <@7> killed <@6> in an earlier Games, with a pushpin spike in <#12>."
        ));
        // Nothing closed the night before.
        assert!(!round(&game, 0, &places).join("").contains("🔥"));
        // The grudge is settled, so it's no longer brought up.
        assert_eq!(grudges(&game), None);
    }

    #[test]
    fn a_team_wins_together() {
        let mut game = Game::new((1..=7).map(|id| (id, format!("P{id}"))).collect());
        for (a, b) in [(0, 1), (2, 3), (4, 5)] {
            game.tributes[a].partner = Some(b);
            game.tributes[b].partner = Some(a);
        }
        assert_eq!(
            teams(&game).unwrap(),
            "**The teams:**\n👥 <@1> & <@2>\n👥 <@3> & <@4>\n👥 <@5> & <@6>\n👤 <@7> fights alone"
        );
        // P1 and P2 win; P2 fell on the way.
        for (i, round) in [(1, 0), (2, 0), (3, 1), (4, 1), (5, 2), (6, 2)] {
            game.tributes[i].died_in = Some(round);
        }
        let text = victor(&game, None).join("");
        assert!(
            text.starts_with(
                "# 👑 The Hunger Games have a winning team!\n👑 <@1>\n👑 <@2>, fell on the way\n3. <@6>"
            ),
            "{text}"
        );
        assert_eq!(teams(&Game::new(vec![(1, "P1".into())])), None);
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
            reward: None,
            theme: None,
        };
        // Sage dies before the Octagon, so isn't a coward; Reed steps in and dies there.
        game.apply(round(Kind::Day(1), vec![death(4, None)]));
        game.apply(Round {
            entrants: vec![0, 3],
            champion: Some(0),
            ..round(Kind::Octagon(1), vec![death(3, Some(0))])
        });
        game.apply(round(Kind::Night(1), vec![death(2, None)]));
        game.apply(round(Kind::Day(2), vec![death(1, Some(0))]));
        assert_eq!(game.cowards(), vec![1, 2]);
        let text = victor(&game, None).join("");
        assert!(text.contains("👑 <@1>, 2 kills, 🏟️ won the Octagon\n"));
        assert!(text.contains("🐔 Never dared to step into the Octagon: <@2>, <@3>\n"));
        assert!(text.ends_with("Octagons won are counted on `/games leaderboard`."));
    }

    #[test]
    fn the_prize_vote_shows_counts_then_the_winner() {
        let counts = [(Reward::Weapon, 1), (Reward::Armour, 0), (Reward::Hunt, 2)];
        let (text, rows) = prize_vote(4, &counts, 1700);
        assert!(text.contains("Voting closes <t:1700:R>"));
        assert!(text.contains("🛡️ **Golden armour**: can't die for the next two rounds · 0 votes"));
        assert!(text.contains("🐺 **The hunt**"));
        assert!(text.contains("· 1 vote"));
        assert_eq!(rows.len(), 1);
        let text = prize_chosen(Reward::Hunt, &counts, false);
        assert!(text.contains("The crowd has spoken: 🐺 **The hunt**"));
        assert!(text.contains("Votes: ⚔️ 1 · 🛡️ 0 · 🐺 2"));
        assert!(prize_chosen(Reward::Weapon, &counts, true).contains("Vivy picked"));
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
