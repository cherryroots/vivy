//! The wheel's Discord parts besides the picture: the status buttons, the menus, the bet
//! slip and modal, and the button IDs. Everything here takes plain values and builds messages;
//! nothing talks to Discord or the database.

use std::collections::HashMap;

use serenity::all::{
    ButtonStyle, CreateActionRow, CreateButton, CreateInputText, CreateModal, CreateSelectMenu,
    CreateSelectMenuKind, CreateSelectMenuOption, InputTextStyle,
};

use super::ledger::{
    BET_CAP, CLAIM, RoundLedger, Rules, Season, TAX_THRESHOLD, can_undo, check_bet,
};
use crate::util::shorten;

/// Display names by user ID, looked up before rendering.
pub type Names = HashMap<u64, String>;

/// What a select menu is for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PickKind {
    /// Pick an option to bet on; the bet slip follows.
    Place,
    /// Pick one of your bets to remove.
    Remove,
    /// Pick the winner (admins).
    Winner,
}

impl PickKind {
    fn as_str(self) -> &'static str {
        match self {
            PickKind::Place => "place",
            PickKind::Remove => "remove",
            PickKind::Winner => "winner",
        }
    }

    fn parse(text: &str) -> Option<PickKind> {
        match text {
            "place" => Some(PickKind::Place),
            "remove" => Some(PickKind::Remove),
            "winner" => Some(PickKind::Winner),
            _ => None,
        }
    }
}

/// Every button, menu and modal this feature makes. The ID text is built and parsed only
/// here. `round` is a round's database ID; `message` is the status message to update.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Show the server's current round on this status message.
    Current,
    Claim {
        round: i64,
    },
    /// Opens the menu of options to bet on.
    Bet {
        round: i64,
    },
    /// Opens the menu of your bets to remove.
    Unbet {
        round: i64,
    },
    /// Opens the menu to pick the winner (admins).
    Winner {
        round: i64,
    },
    /// Clears the winner of a resolved round (admins).
    Undo {
        round: i64,
    },
    /// A menu from [`Action::Bet`], [`Action::Unbet`] or [`Action::Winner`]. The picked user
    /// is the selected value. A big wheel needs several menus; `part` is which one (0 to 4),
    /// since the menus of one message need different IDs.
    Pick {
        kind: PickKind,
        round: i64,
        message: u64,
        part: u8,
    },
    /// The button under "This message shows an older round": shows the current round on
    /// status message `message`.
    Refresh {
        message: u64,
    },
    /// A bet slip button: bet `percent`% of the round's money on `on`.
    Stake {
        round: i64,
        on: u64,
        message: u64,
        percent: i64,
    },
    /// The bet slip's "Other amount" button, which opens the amount modal.
    Other {
        round: i64,
        on: u64,
        message: u64,
    },
    /// The bet amount modal.
    Amount {
        round: i64,
        on: u64,
        message: u64,
    },
    /// The button of `/reset_wheel`, which opens the confirm modal. `season` is the
    /// season that was active when the command ran (0 if there was none), so a
    /// confirmation from before another reset can't end the season that reset started.
    Reset {
        keep_options: bool,
        season: i64,
    },
    /// The confirm modal of `/reset_wheel`, with the same values as [`Action::Reset`].
    NewSeason {
        keep_options: bool,
        season: i64,
    },
    /// Explains the rules of a season, privately.
    Help {
        season: i64,
    },
    /// Opens the modal to change your name on the wheel. `round` is the round the status
    /// message shows, to redraw it after.
    Rename {
        round: i64,
    },
    /// The name modal.
    Name {
        round: i64,
    },
}

impl Action {
    pub fn custom_id(&self) -> String {
        match self {
            Action::Current => "wheel:current".to_string(),
            Action::Claim { round } => format!("wheel:claim:{round}"),
            Action::Bet { round } => format!("wheel:bet:{round}"),
            Action::Unbet { round } => format!("wheel:unbet:{round}"),
            Action::Winner { round } => format!("wheel:winner:{round}"),
            Action::Undo { round } => format!("wheel:undo:{round}"),
            Action::Pick {
                kind,
                round,
                message,
                part,
            } => format!("wheel:pick:{}:{round}:{message}:{part}", kind.as_str()),
            Action::Refresh { message } => format!("wheel:refresh:{message}"),
            Action::Stake {
                round,
                on,
                message,
                percent,
            } => format!("wheel:stake:{round}:{on}:{message}:{percent}"),
            Action::Other { round, on, message } => format!("wheel:other:{round}:{on}:{message}"),
            Action::Amount { round, on, message } => {
                format!("wheel:amount:{round}:{on}:{message}")
            }
            Action::Reset {
                keep_options,
                season,
            } => format!("wheel:reset:{}:{season}", u8::from(*keep_options)),
            Action::NewSeason {
                keep_options,
                season,
            } => format!("wheel:newseason:{}:{season}", u8::from(*keep_options)),
            Action::Help { season } => format!("wheel:help:{season}"),
            Action::Rename { round } => format!("wheel:rename:{round}"),
            Action::Name { round } => format!("wheel:name:{round}"),
        }
    }

    /// Parses what the dispatcher passes on: the ID without the `wheel:` prefix.
    pub fn parse(action: &str) -> Option<Action> {
        let parts: Vec<&str> = action.split(':').collect();
        Some(match parts.as_slice() {
            ["current"] => Action::Current,
            ["claim", round] => Action::Claim {
                round: round.parse().ok()?,
            },
            ["bet", round] => Action::Bet {
                round: round.parse().ok()?,
            },
            ["unbet", round] => Action::Unbet {
                round: round.parse().ok()?,
            },
            ["winner", round] => Action::Winner {
                round: round.parse().ok()?,
            },
            ["undo", round] => Action::Undo {
                round: round.parse().ok()?,
            },
            ["pick", kind, round, message, part] => Action::Pick {
                kind: PickKind::parse(kind)?,
                round: round.parse().ok()?,
                message: message.parse().ok()?,
                part: part.parse().ok()?,
            },
            // Menus from before there could be several.
            ["pick", kind, round, message] => Action::Pick {
                kind: PickKind::parse(kind)?,
                round: round.parse().ok()?,
                message: message.parse().ok()?,
                part: 0,
            },
            ["refresh", message] => Action::Refresh {
                message: message.parse().ok()?,
            },
            ["stake", round, on, message, percent] => Action::Stake {
                round: round.parse().ok()?,
                on: on.parse().ok()?,
                message: message.parse().ok()?,
                percent: percent.parse().ok()?,
            },
            ["other", round, on, message] => Action::Other {
                round: round.parse().ok()?,
                on: on.parse().ok()?,
                message: message.parse().ok()?,
            },
            ["amount", round, on, message] => Action::Amount {
                round: round.parse().ok()?,
                on: on.parse().ok()?,
                message: message.parse().ok()?,
            },
            ["reset", keep, season] => Action::Reset {
                keep_options: parse_flag(keep)?,
                season: season.parse().ok()?,
            },
            ["newseason", keep, season] => Action::NewSeason {
                keep_options: parse_flag(keep)?,
                season: season.parse().ok()?,
            },
            ["help", season] => Action::Help {
                season: season.parse().ok()?,
            },
            ["rename", round] => Action::Rename {
                round: round.parse().ok()?,
            },
            ["name", round] => Action::Name {
                round: round.parse().ok()?,
            },
            _ => return None,
        })
    }
}

/// A yes/no written as `1` or `0` in an ID.
fn parse_flag(text: &str) -> Option<bool> {
    match text {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

/// One round of a season, ready to show.
pub struct View<'a> {
    pub season: &'a Season,
    /// [`super::ledger::ledger`] of the season.
    pub ledger: &'a [RoundLedger],
    /// Which round: an index into `season.rounds`.
    pub index: usize,
    /// Whether the season is being played. Past seasons get no buttons.
    pub active: bool,
    pub names: &'a Names,
}

impl View<'_> {
    pub(super) fn is_latest(&self) -> bool {
        self.index + 1 == self.season.rounds.len()
    }

    /// Whether to offer "Undo Winner". See [`can_undo`].
    pub fn undoable(&self) -> bool {
        self.active && can_undo(self.season, self.index)
    }

    pub(super) fn name(&self, user: u64) -> &str {
        name(self.names, user)
    }

    /// The movie an option picked, if one is set.
    pub(super) fn movie(&self, user: u64) -> Option<&str> {
        self.season.movies.get(&user).map(String::as_str)
    }
}

/// "Alice (Alien)", or just "Alice" when no movie is set.
pub fn with_movie(name: &str, movie: Option<&str>) -> String {
    match movie {
        Some(movie) => format!("{name} ({movie})"),
        None => name.to_string(),
    }
}

pub fn name(names: &Names, user: u64) -> &str {
    names.get(&user).map_or("Unknown", String::as_str)
}

/// The buttons under a status message.
pub fn status_buttons(view: &View) -> Vec<CreateActionRow> {
    if !view.active {
        return Vec::new();
    }
    let round = view.season.rounds[view.index].id;
    let button = |action: Action, label: &str, emoji: char, style| {
        CreateButton::new(action.custom_id())
            .label(label)
            .emoji(emoji)
            .style(style)
    };
    let buttons = if view.is_latest() {
        vec![
            button(Action::Claim { round }, "Claim", '📈', ButtonStyle::Primary),
            button(Action::Bet { round }, "Bet", '💸', ButtonStyle::Secondary),
            button(
                Action::Unbet { round },
                "Remove bet",
                '↩',
                ButtonStyle::Secondary,
            ),
            button(
                Action::Winner { round },
                "Winner",
                '✨',
                ButtonStyle::Secondary,
            ),
        ]
    } else {
        let mut buttons = vec![button(
            Action::Current,
            "View Current Round",
            '🎯',
            ButtonStyle::Primary,
        )];
        if view.undoable() {
            buttons.push(button(
                Action::Undo { round },
                "Undo Winner",
                '⏪',
                ButtonStyle::Danger,
            ));
        }
        buttons
    };
    // Discord allows 5 buttons in a row.
    let more = vec![
        button(
            Action::Rename { round },
            "Change Name",
            '🏷',
            ButtonStyle::Secondary,
        ),
        button(
            Action::Help {
                season: view.season.id,
            },
            "Help",
            '❓',
            ButtonStyle::Secondary,
        ),
    ];
    vec![
        CreateActionRow::Buttons(buttons),
        CreateActionRow::Buttons(more),
    ]
}

/// Options in one Discord select menu, and menus (rows) in one message.
const MENU_OPTIONS: usize = 25;
const MAX_MENUS: usize = 5;

/// The menus after Place Bet, Remove Bet or Set Winner. `users` are the choices, sorted by
/// name, each with its movie under it if one is set. Discord shows at most 25 in a menu, so a big wheel gets several menus, each saying
/// which names it holds; past 125 the rest are left out and the text says so.
pub fn pick_menu(
    kind: PickKind,
    round: i64,
    message: u64,
    users: &[u64],
    names: &Names,
    movies: &HashMap<u64, String>,
) -> (String, Vec<CreateActionRow>) {
    let (title, placeholder) = match kind {
        PickKind::Place => ("Place a Bet", "Pick who to bet on"),
        PickKind::Remove => ("Remove a Bet", "Pick a bet to remove"),
        PickKind::Winner => ("Pick a Winner", "Select the winner"),
    };
    let mut sorted: Vec<u64> = users.to_vec();
    sorted.sort_by(|a, b| name(names, *a).cmp(name(names, *b)));
    let shown = sorted.len().min(MENU_OPTIONS * MAX_MENUS);
    let groups: Vec<&[u64]> = sorted[..shown].chunks(MENU_OPTIONS).collect();

    let mut rows = Vec::new();
    for (part, group) in groups.iter().enumerate() {
        let options = group
            .iter()
            .map(|&user| {
                let option =
                    CreateSelectMenuOption::new(shorten(name(names, user), 100), user.to_string());
                match movies.get(&user) {
                    Some(movie) => option.description(shorten(movie, 100)),
                    None => option,
                }
            })
            .collect();
        // With several menus, each placeholder starts with the names it holds ("Alice … Dana:
        // pick who to bet on"), since a phone shows only the first few words of a placeholder.
        let placeholder = match (groups.len(), group.first(), group.last()) {
            (2.., Some(&first), Some(&last)) => format!(
                "{} … {}: {}",
                shorten(name(names, first), 20),
                shorten(name(names, last), 20),
                placeholder.to_lowercase()
            ),
            _ => placeholder.to_string(),
        };
        let action = Action::Pick {
            kind,
            round,
            message,
            part: part as u8,
        };
        let menu =
            CreateSelectMenu::new(action.custom_id(), CreateSelectMenuKind::String { options })
                .placeholder(placeholder);
        rows.push(CreateActionRow::SelectMenu(menu));
    }

    let mut content = title.to_string();
    if shown < sorted.len() {
        content.push_str(&format!(
            "\n-# Discord's menus only fit {shown} of the {} names.",
            sorted.len()
        ));
    }
    (content, rows)
}

/// The answer to a button on a status message that shows an old round: the error, and a
/// button that shows the current round on that message.
pub fn not_current(text: &str, message: u64) -> (String, Vec<CreateActionRow>) {
    let button = CreateButton::new(Action::Refresh { message }.custom_id())
        .label("View Current Round")
        .emoji('🎯')
        .style(ButtonStyle::Primary);
    (
        text.to_string(),
        vec![CreateActionRow::Buttons(vec![button])],
    )
}

/// The bet slip: a private message about betting on one option, with a button per share
/// of the player's money. It shows the odds and the player's bet as they are now, and is
/// rebuilt after every bet. `note` says what just happened.
pub fn bet_slip(
    season: &Season,
    numbers: &RoundLedger,
    user: u64,
    on: u64,
    on_name: &str,
    message: u64,
    note: Option<&str>,
) -> (String, Vec<CreateActionRow>) {
    let round = season.rounds.last().map_or(0, |r| r.id);
    let mine = season
        .rounds
        .last()
        .and_then(|r| r.bets.iter().find(|b| b.by == user && b.on == on));
    let money = numbers.money(user);
    let standing = numbers.standing(user);
    let bet = standing.map_or(0, |s| s.bet);

    let mut lines = vec![format!("**Bet on {on_name}**")];
    if let Some(note) = note {
        lines.push(note.to_string());
    }
    lines.push(match (numbers.rules, numbers.odds(on)) {
        (Rules::Pool, Some(odds)) => format!(
            "{} is on it, which pays ×{odds:.1} right now. The pot is {}.",
            numbers.total_on(on),
            numbers.pot
        ),
        (Rules::Pool, None) => format!(
            "Nobody has bet on {on_name} yet: a bet here would take the whole pot of {}.",
            numbers.pot
        ),
        (Rules::Classic, _) => format!(
            "A winning bet pays ×{}.",
            numbers.options_left.len().saturating_sub(1)
        ),
    });
    // Changing a bet frees its old amount, so the cap counts the existing bet too (like the
    // buttons below do).
    let cap = numbers.usable(user) + mine.map_or(0, |b| b.amount);
    lines.push(format!(
        "You have {money} this round and have bet {bet} of it. You can put up to {cap} on {on_name}."
    ));
    if let Some(mine) = mine {
        let back = numbers.result(mine, on) + mine.amount;
        lines.push(format!(
            "Your bet on {on_name}: **{}**. If {on_name} won now, you'd get {back} back.",
            mine.amount
        ));
    }
    if standing.is_some_and(|s| s.under_threshold()) {
        lines.push(format!(
            "-# Bet at least {} in total ({TAX_THRESHOLD}%) to avoid the tax.",
            standing.map_or(0, |s| s.safe_bet())
        ));
    }
    if numbers.rules == Rules::Pool {
        lines.push("-# The odds change as others bet, until the winner is set.".to_string());
    }

    let percents: &[i64] = match numbers.rules {
        Rules::Pool => &[TAX_THRESHOLD, 25, BET_CAP],
        Rules::Classic => &[TAX_THRESHOLD, 25, 50, 100],
    };
    let mut buttons: Vec<CreateButton> = percents
        .iter()
        .map(|&percent| {
            let amount = (money * percent + 99) / 100;
            let current = mine.is_some_and(|b| b.amount == amount);
            let allowed = check_bet(season, user, on, &amount.to_string()).is_ok();
            CreateButton::new(
                Action::Stake {
                    round,
                    on,
                    message,
                    percent,
                }
                .custom_id(),
            )
            .label(format!("{percent}% · {amount}"))
            .style(if current {
                ButtonStyle::Success
            } else {
                ButtonStyle::Primary
            })
            .disabled(current || !allowed)
        })
        .collect();
    buttons.push(
        CreateButton::new(Action::Other { round, on, message }.custom_id())
            .label("Other amount…")
            .style(ButtonStyle::Secondary),
    );
    (lines.join("\n"), vec![CreateActionRow::Buttons(buttons)])
}

/// The modal asking how much to bet. `existing` is the player's current bet on this option.
pub fn amount_modal(
    round: i64,
    on: u64,
    message: u64,
    on_name: &str,
    money: i64,
    max: i64,
    existing: i64,
) -> CreateModal {
    // Discord allows 45 characters in a label and 100 in a placeholder.
    let label = shorten(&format!("Amount (up to {max})"), 45);
    let placeholder = shorten(
        &format!("A number like 50, or a share of your {money} like 25%"),
        100,
    );
    let mut input = CreateInputText::new(InputTextStyle::Short, label, "amount")
        .placeholder(placeholder)
        .required(true);
    if existing > 0 {
        input = input.value(existing.to_string());
    }
    let title = shorten(&format!("Bet on {on_name}"), 45);
    CreateModal::new(Action::Amount { round, on, message }.custom_id(), title)
        .components(vec![CreateActionRow::InputText(input)])
}

/// The longest name a player can choose, in characters.
pub const MAX_CHOSEN_NAME: usize = 32;

/// The modal to change your name on the wheel. `current` is the name you chose before.
pub fn name_modal(round: i64, current: Option<&str>) -> CreateModal {
    let mut input = CreateInputText::new(InputTextStyle::Short, "Name on the wheel", "name")
        .placeholder("Leave empty to use your Discord name")
        .max_length(MAX_CHOSEN_NAME as u16)
        .required(false);
    if let Some(current) = current {
        input = input.value(current);
    }
    CreateModal::new(Action::Name { round }.custom_id(), "Change your name")
        .components(vec![CreateActionRow::InputText(input)])
}

/// Checks a name typed in the name modal. `None` means back to the Discord name.
pub fn clean_name(input: &str) -> Result<Option<String>, String> {
    clean_text(input, MAX_CHOSEN_NAME, "name", "Names")
}

/// The longest movie title an admin can set, in characters. Discord's menus show it under
/// the name and allow 100 there.
pub const MAX_MOVIE: usize = 100;

/// Checks a movie title from `/wheel_add` or `/wheel_movie`. `None` means no movie.
pub fn clean_movie(input: &str) -> Result<Option<String>, String> {
    clean_text(input, MAX_MOVIE, "movie title", "Movie titles")
}

/// Collapses spaces and checks the length and the characters. `what` and `plural` name the
/// text in the error messages.
fn clean_text(input: &str, max: usize, what: &str, plural: &str) -> Result<Option<String>, String> {
    let text = input.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return Ok(None);
    }
    if text.chars().count() > max {
        return Err(format!(
            "That {what} is too long: {max} characters at most."
        ));
    }
    // These are also used in messages, where these would make mentions or code.
    if text.contains(['@', '<', '>', '`']) || text.chars().any(char::is_control) {
        return Err(format!("{plural} can't contain @, <, > or `."));
    }
    Ok(Some(text))
}

/// What the Help button says: how to play, under the season's rules.
pub fn help_text(rules: Rules) -> String {
    let payouts = match rules {
        Rules::Pool => {
            "- **The pot**: all bets of a round, and the taxes, go into one pot. When an admin sets the winner, everyone who bet on it shares the pot by how much they bet. The fewer people back an option, the more it pays: the picture shows each option's payout right now (×2.5 means a bet of 10 gets 25 back). If nobody bet on the winner, the pot carries over to the next round."
        }
        Rules::Classic => {
            "- **Payouts**: a winning bet pays its amount × (options left − 1). A losing bet is lost."
        }
    };
    let tax = match rules {
        Rules::Pool => " That tax goes into the pot.",
        Rules::Classic => "",
    };
    let cap = match rules {
        Rules::Pool => {
            format!(" Your bets in one round can add up to at most {BET_CAP}% of your money.")
        }
        Rules::Classic => String::new(),
    };
    let short = match rules {
        Rules::Pool => format!(
            "Each round: press **Claim**, then **Bet** {TAX_THRESHOLD}–{BET_CAP}% of your money on who you think wins. Winners share the pot."
        ),
        Rules::Classic => format!(
            "Each round: press **Claim**, then **Bet** at least {TAX_THRESHOLD}% of your money on who you think wins. Winning bets pay out, losing ones are lost."
        ),
    };
    format!(
        "**How the movie wheel works**
{short}

- **Claim** gives you {CLAIM} once every round.
- **Bet** on the options you think will win: pick an option, then a share of your money, or type an amount like `50` under Other amount. The bet slip shows what the option pays right now. You can bet on up to half of the options left.{cap} Betting on the same option again changes that bet, and **Remove bet** takes it back while the round is open.
- **Bet at least {TAX_THRESHOLD}%** of your money every round. Otherwise you lose 3% of your money for every missing percentage point when the round ends, up to 30%.{tax}
{payouts}
- **Change Name** sets the name the wheel shows for you; leave it empty to go back to your Discord name.
- An option leaves the wheel once it has won. Admins add options with `/wheel_add` and start a new season with `/reset_wheel`."
    )
}

/// The `/reset_wheel` confirmation. `season` is the active season (0 if none).
pub fn reset_confirmation(keep_options: bool, season: i64) -> (String, Vec<CreateActionRow>) {
    let kept = if keep_options {
        " with the same options"
    } else {
        " with an empty wheel"
    };
    let text = format!(
        "This ends the current season and starts a new one{kept}. The old season stays viewable with `/wheel_status season:`."
    );
    let action = Action::Reset {
        keep_options,
        season,
    };
    let button = CreateButton::new(action.custom_id())
        .label("Start a new season")
        .style(ButtonStyle::Danger);
    (text, vec![CreateActionRow::Buttons(vec![button])])
}

/// What an admin types in the confirm modal to start a new season.
pub const CONFIRM_WORD: &str = "START";

/// The modal that confirms a new season. Typing a word makes a double press harmless.
pub fn new_season_modal(keep_options: bool, season: i64) -> CreateModal {
    let input = CreateInputText::new(
        InputTextStyle::Short,
        format!("Type {CONFIRM_WORD} to end this season"),
        "confirm",
    )
    .placeholder(CONFIRM_WORD)
    .max_length(20)
    .required(true);
    let action = Action::NewSeason {
        keep_options,
        season,
    };
    CreateModal::new(action.custom_id(), "Start a new season?")
        .components(vec![CreateActionRow::InputText(input)])
}

/// Whether the text typed in the confirm modal is [`CONFIRM_WORD`], in any case.
pub fn confirmed(input: &str) -> bool {
    input.trim().eq_ignore_ascii_case(CONFIRM_WORD)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::wheel::ledger::{Bet, Round, ledger};

    fn names() -> Names {
        [(1, "Alice"), (2, "Bob"), (3, "Charlie"), (4, "Dana")]
            .into_iter()
            .map(|(id, name)| (id, name.to_string()))
            .collect()
    }

    fn bet(by: u64, on: u64, amount: i64) -> Bet {
        Bet { by, on, amount }
    }

    #[test]
    fn undo_only_while_the_next_round_has_no_bets() {
        let mut season = Season {
            id: 1,
            number: 1,
            rules: Default::default(),
            options: vec![1, 2],
            movies: Default::default(),
            rounds: vec![
                Round {
                    id: 1,
                    number: 1,
                    winner: Some(2),
                    ..Round::default()
                },
                Round {
                    id: 2,
                    number: 2,
                    claims: vec![1],
                    ..Round::default()
                },
            ],
        };
        let labels = |season: &Season, index, active| {
            let names = names();
            let numbers = ledger(season);
            let view = View {
                season,
                ledger: &numbers,
                index,
                active,
                names: &names,
            };
            let rows = serde_json::to_value(status_buttons(&view)).unwrap();
            rows.as_array()
                .unwrap()
                .iter()
                .flat_map(|row| row["components"].as_array().unwrap().clone())
                .map(|b| b["label"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            labels(&season, 0, true),
            ["View Current Round", "Undo Winner", "Change Name", "Help"]
        );
        assert_eq!(
            labels(&season, 1, true),
            [
                "Claim",
                "Bet",
                "Remove bet",
                "Winner",
                "Change Name",
                "Help"
            ]
        );
        assert!(labels(&season, 0, false).is_empty());
        season.rounds[1].bets.push(bet(1, 1, 10));
        assert_eq!(
            labels(&season, 0, true),
            ["View Current Round", "Change Name", "Help"]
        );
    }

    #[test]
    fn help_fits_a_message() {
        for rules in [Rules::Classic, Rules::Pool] {
            assert!(help_text(rules).len() < 2000);
        }
        assert!(help_text(Rules::Pool).contains("pot"));
        assert!(!help_text(Rules::Classic).contains("pot"));
    }

    #[test]
    fn action_round_trip() {
        for action in [
            Action::Current,
            Action::Claim { round: 3 },
            Action::Bet { round: 3 },
            Action::Unbet { round: 3 },
            Action::Winner { round: 3 },
            Action::Undo { round: 3 },
            Action::Pick {
                kind: PickKind::Remove,
                round: 3,
                message: 99,
                part: 4,
            },
            Action::Refresh { message: u64::MAX },
            Action::Amount {
                round: 3,
                on: 7,
                message: 99,
            },
            Action::Reset {
                keep_options: true,
                season: 4,
            },
            Action::Reset {
                keep_options: false,
                season: 0,
            },
            Action::NewSeason {
                keep_options: true,
                season: 4,
            },
            Action::Help { season: 4 },
            Action::Rename { round: 3 },
            Action::Name { round: 3 },
            Action::Stake {
                round: 3,
                on: u64::MAX,
                message: u64::MAX,
                percent: 25,
            },
            Action::Other {
                round: 3,
                on: 7,
                message: 99,
            },
        ] {
            let id = action.custom_id();
            assert!(id.len() <= 100, "{id}");
            let rest = id.strip_prefix("wheel:").unwrap();
            assert_eq!(Action::parse(rest), Some(action));
        }
        assert_eq!(Action::parse("pick:steal:1:2"), None);
        // Menus from before there were several menus.
        assert_eq!(
            Action::parse("pick:place:1:2"),
            Some(Action::Pick {
                kind: PickKind::Place,
                round: 1,
                message: 2,
                part: 0,
            })
        );
        assert_eq!(Action::parse("claim"), None);
        // Reset buttons from before the confirm modal carry no season.
        assert_eq!(Action::parse("reset:1"), None);
    }

    #[test]
    fn big_wheels_get_several_menus() {
        let users: Vec<u64> = (1..=130).collect();
        let names: Names = users.iter().map(|&u| (u, format!("P{u:03}"))).collect();
        let (text, rows) = pick_menu(PickKind::Place, 3, 99, &users, &names, &HashMap::new());
        let rows = serde_json::to_value(rows).unwrap();
        let menus: Vec<&serde_json::Value> = rows
            .as_array()
            .unwrap()
            .iter()
            .map(|row| &row["components"][0])
            .collect();
        assert_eq!(menus.len(), 5);
        let sizes: Vec<usize> = menus
            .iter()
            .map(|m| m["options"].as_array().unwrap().len())
            .collect();
        assert_eq!(sizes, [25, 25, 25, 25, 25]);
        // Every menu has its own ID, and they all parse.
        let ids: Vec<&str> = menus
            .iter()
            .map(|m| m["custom_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids[0], "wheel:pick:place:3:99:0");
        assert_eq!(ids[4], "wheel:pick:place:3:99:4");
        assert_eq!(
            menus[1]["placeholder"].as_str().unwrap(),
            "P026 … P050: pick who to bet on"
        );
        assert!(text.contains("only fit 125 of the 130"), "{text}");

        // A small wheel: one menu, no note, and the movies under the names.
        let movies = HashMap::from([(2, "Alien".to_string())]);
        let (text, rows) = pick_menu(PickKind::Winner, 3, 99, &users[..3], &names, &movies);
        assert_eq!(text, "Pick a Winner");
        let rows = serde_json::to_value(rows).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 1);
        let menu = &rows[0]["components"][0];
        assert_eq!(menu["placeholder"], "Select the winner");
        assert_eq!(menu["options"][1]["description"], "Alien");
        assert!(menu["options"][0].get("description").is_none());
    }

    #[test]
    fn new_season_needs_the_word() {
        assert!(confirmed("START"));
        assert!(confirmed(" start "));
        assert!(!confirmed(""));
        assert!(!confirmed("STAR"));
    }

    #[test]
    fn bet_slip_shows_odds_and_limits() {
        // Pool rules. Alice (1) has 200 and bet 40 on Bob (2); Charlie (3) bet 60 on Dana (4).
        let season = Season {
            id: 1,
            number: 2,
            rules: Rules::Pool,
            options: vec![1, 2, 3, 4],
            movies: Default::default(),
            rounds: vec![Round {
                id: 5,
                number: 1,
                claims: vec![1, 1, 3],
                bets: vec![bet(1, 2, 40), bet(3, 4, 60)],
                ..Round::default()
            }],
        };
        let numbers = ledger(&season);
        let (text, rows) = bet_slip(&season, &numbers[0], 1, 2, "Bob", 99, Some("✅ Bet 40."));
        assert!(text.contains("✅ Bet 40."), "{text}");
        assert!(text.contains("40 is on it, which pays ×2.5 right now. The pot is 100."));
        assert!(text.contains(
            "You have 200 this round and have bet 40 of it. You can put up to 100 on Bob."
        ));
        assert!(text.contains("Your bet on Bob: **40**. If Bob won now, you'd get 100 back."));
        let rows = serde_json::to_value(rows).unwrap();
        let buttons = rows[0]["components"].as_array().unwrap();
        let labels: Vec<&str> = buttons
            .iter()
            .map(|b| b["label"].as_str().unwrap())
            .collect();
        assert_eq!(
            labels,
            ["10% · 20", "25% · 50", "50% · 100", "Other amount…"]
        );
        let disabled: Vec<bool> = buttons
            .iter()
            .map(|b| b["disabled"].as_bool().unwrap_or(false))
            .collect();
        // Changing the bet on Bob frees its 40, so even 50% (100) fits under the cap.
        assert_eq!(disabled, [false, false, false, false]);

        // On Dana, Alice can add at most 60 more.
        let (text, rows) = bet_slip(&season, &numbers[0], 1, 4, "Dana", 99, None);
        assert!(
            text.contains("60 is on it, which pays ×1.7 right now."),
            "{text}"
        );
        assert!(!text.contains("Your bet on"));
        let rows = serde_json::to_value(rows).unwrap();
        let disabled: Vec<bool> = rows[0]["components"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["disabled"].as_bool().unwrap_or(false))
            .collect();
        assert_eq!(disabled, [false, false, true, false]);

        // Nobody bet on Alice yet.
        let (text, _) = bet_slip(&season, &numbers[0], 1, 1, "Alice", 99, None);
        assert!(
            text.contains("a bet here would take the whole pot of 100"),
            "{text}"
        );
    }

    #[test]
    fn chosen_names_are_checked() {
        assert_eq!(
            clean_name("  Movie   Buff "),
            Ok(Some("Movie Buff".to_string()))
        );
        assert_eq!(clean_name("   "), Ok(None));
        assert_eq!(clean_name("🎬 Ünïcødé"), Ok(Some("🎬 Ünïcødé".to_string())));
        assert!(clean_name(&"x".repeat(MAX_CHOSEN_NAME + 1)).is_err());
        assert!(clean_name(&"é".repeat(MAX_CHOSEN_NAME)).is_ok());
        assert!(clean_name("@everyone").is_err());
        assert!(clean_name("<@123>").is_err());
    }

    #[test]
    fn movie_titles_are_checked() {
        assert_eq!(
            clean_movie(" The  Thing (1982) "),
            Ok(Some("The Thing (1982)".to_string()))
        );
        assert_eq!(clean_movie(""), Ok(None));
        assert!(clean_movie(&"x".repeat(MAX_MOVIE)).is_ok());
        assert!(clean_movie(&"x".repeat(MAX_MOVIE + 1)).is_err());
        assert!(clean_movie("<@&1>").is_err());
        assert_eq!(with_movie("Alice", Some("Alien")), "Alice (Alien)");
        assert_eq!(with_movie("Alice", None), "Alice");
    }
}
