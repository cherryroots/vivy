//! `/wheel`, with `status` for everyone and `add`, `movie`, `bet` and `reset` for admins.
//! The subcommands are the variants of [`WheelCommand`]; each has a struct with its options.

use anyhow::Context as _;
use chrono::Utc;
use tracing::info;
use twilight_interactions::command::{CommandModel, CreateCommand};
use twilight_model::user::User;

use super::actions::{Game, active_game, redraw_last, remember_status, status_message};
use super::ledger::Bet;
use super::{names, store, ui};
use crate::core::{Command, CommandCtx, Result, user_error};
use crate::util::discord::{Draft, GuildId};

#[derive(CommandModel, CreateCommand)]
#[command(name = "wheel", desc = "The movie wheel", contexts = "guild")]
#[allow(
    clippy::large_enum_variant,
    reason = "read once per command; a picked user is big, and that's fine"
)]
pub enum WheelCommand {
    #[command(name = "status")]
    Status(WheelStatus),
    #[command(name = "add")]
    Add(WheelAdd),
    #[command(name = "movie")]
    Movie(WheelMovie),
    #[command(name = "bet")]
    Bet(WheelBet),
    #[command(name = "reset")]
    Reset(WheelReset),
}

pub fn wheel() -> Command {
    Command::slash::<WheelCommand>(|ctx| Box::pin(run(ctx)))
}

async fn run(ctx: CommandCtx) -> Result<()> {
    match ctx.parse::<WheelCommand>()? {
        WheelCommand::Status(args) => wheel_status(ctx, args).await,
        // The admin commands answer privately.
        WheelCommand::Add(args) => wheel_add(ctx.ephemeral(), args).await,
        WheelCommand::Movie(args) => wheel_movie(ctx.ephemeral(), args).await,
        WheelCommand::Bet(args) => insert_bet(ctx.ephemeral(), args).await,
        WheelCommand::Reset(args) => reset_wheel(ctx.ephemeral(), args).await,
    }
}

#[derive(CommandModel, CreateCommand)]
#[command(name = "status", desc = "The current round, or an earlier one")]
pub struct WheelStatus {
    /// Which round to show (default: the current one)
    #[command(min_value = 1)]
    round: Option<i64>,
    /// Which season (default: the one being played)
    #[command(min_value = 1)]
    season: Option<i64>,
}

async fn wheel_status(ctx: CommandCtx, args: WheelStatus) -> Result<()> {
    let WheelStatus { round, season } = args;
    let guild = guild(&ctx)?;
    // Looking up names and drawing can take longer than Discord waits for an answer.
    ctx.defer().await?;
    let now = Utc::now().timestamp();
    let game = ctx
        .bot
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            let (id, active) = match season {
                None => (store::ensure_season(&tx, guild.get(), now)?, true),
                Some(number) => {
                    let list = store::seasons(&tx, guild.get())?;
                    let found = usize::try_from(number - 1)
                        .ok()
                        .and_then(|n| list.get(n))
                        .ok_or_else(|| {
                            user_error(format!("This server has {} seasons.", list.len()))
                        })?;
                    (found.id, found.ended_at.is_none())
                }
            };
            let season = store::load_season(&tx, id)?;
            tx.commit()?;
            Ok(Game { season, active })
        })
        .await?;

    let count = game.season.rounds.len();
    let index = match round {
        None => count - 1,
        Some(number) => usize::try_from(number - 1)
            .ok()
            .filter(|&n| n < count)
            .ok_or_else(|| user_error(format!("Pick a round from 1 to {count}.")))?,
    };
    let status = status_message(&ctx.bot, guild, &game, index).await?;
    let (name, png) = status.picture;
    let handle = ctx
        .send(Draft::new().file(name, png).components(status.buttons))
        .await?;
    // The current round's message is the one admin commands redraw later.
    if index == count - 1 && game.active {
        let sent = handle.message(&ctx).await?;
        remember_status(&ctx.bot, guild, sent.channel_id, sent.id).await;
    }
    Ok(())
}

/// The last line of an admin command's reply: whether the public picture was updated.
async fn redraw_note(bot: &crate::core::BotCtx, guild: GuildId) -> String {
    match redraw_last(bot, guild).await {
        Some(true) => "The wheel picture is updated.".to_string(),
        _ => "Run /wheel status to show the new wheel.".to_string(),
    }
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "add",
    desc = "Add users to the wheel with the movies they picked, or take them off (admins)"
)]
pub struct WheelAdd {
    /// User to add to the wheel
    user: User,
    /// The movie they picked
    movie: Option<String>,
    /// Another user
    user2: Option<User>,
    /// The movie user2 picked
    movie2: Option<String>,
    /// Another user
    user3: Option<User>,
    /// The movie user3 picked
    movie3: Option<String>,
    /// Another user
    user4: Option<User>,
    /// The movie user4 picked
    movie4: Option<String>,
    /// Another user
    user5: Option<User>,
    /// The movie user5 picked
    movie5: Option<String>,
    /// Remove the users from the wheel
    remove: Option<bool>,
}

async fn wheel_add(ctx: CommandCtx, args: WheelAdd) -> Result<()> {
    let WheelAdd {
        user,
        movie,
        user2,
        movie2,
        user3,
        movie3,
        user4,
        movie4,
        user5,
        movie5,
        remove,
    } = args;
    require_admin(&ctx)?;
    let guild = guild(&ctx)?;
    let remove = remove.unwrap_or(false);
    // Each user once, in the order given, with their movie. A movie without its user is a
    // mistake worth pointing out rather than dropping.
    let pairs = [
        (Some(user), movie, ""),
        (user2, movie2, "2"),
        (user3, movie3, "3"),
        (user4, movie4, "4"),
        (user5, movie5, "5"),
    ];
    let mut added: Vec<(u64, Option<String>)> = Vec::new();
    for (user, movie, n) in pairs {
        let movie = match movie {
            Some(text) => ui::clean_movie(&text).map_err(user_error)?,
            None => None,
        };
        let Some(user) = user else {
            if movie.is_some() {
                return Err(user_error(format!("movie{n} needs a user{n}.")));
            }
            continue;
        };
        if !added.iter().any(|(id, _)| *id == user.id.get()) {
            added.push((user.id.get(), movie));
        }
    }
    let now = Utc::now().timestamp();
    let list = added.clone();
    // Whether each user was changed (added or removed), in the same order.
    let changed: Vec<bool> = ctx
        .bot
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            let season = store::ensure_season(&tx, guild.get(), now)?;
            let mut changed = Vec::new();
            for (id, movie) in list {
                if remove {
                    changed.push(store::remove_option(&tx, season, id)?);
                } else {
                    changed.push(store::add_option(&tx, season, id)?);
                    // Someone already on the wheel gets the new movie too.
                    if movie.is_some() {
                        store::set_movie(&tx, season, id, movie.as_deref())?;
                    }
                }
            }
            tx.commit()?;
            Ok(changed)
        })
        .await?;

    let ids: Vec<u64> = added.iter().map(|(id, _)| *id).collect();
    let names = names::lookup(&ctx.bot, guild, &ids).await;
    let mut lines = Vec::new();
    for ((id, movie), &changed) in added.iter().zip(&changed) {
        let name = ui::name(&names, *id);
        let line = match (remove, changed, movie) {
            (false, true, Some(movie)) => format!("Added {name} to the wheel with {movie}!"),
            (false, true, None) => format!("Added {name} to the wheel!"),
            (false, false, Some(movie)) => {
                format!("{name} is already on the wheel; their movie is now {movie}.")
            }
            (false, false, None) => format!("{name} is already on the wheel."),
            (true, true, _) => format!("Removed {name} from the wheel!"),
            (true, false, _) => format!("{name} isn't on the wheel."),
        };
        if changed || (!remove && movie.is_some()) {
            info!("{line}");
        }
        lines.push(line);
    }
    lines.push(redraw_note(&ctx.bot, guild).await);
    ctx.say(lines.join("\n")).await?;
    Ok(())
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "movie",
    desc = "Set or change the movie someone on the wheel picked (admins)"
)]
pub struct WheelMovie {
    /// Who picked the movie
    user: User,
    /// The movie; leave it out to clear it
    movie: Option<String>,
}

async fn wheel_movie(ctx: CommandCtx, args: WheelMovie) -> Result<()> {
    let WheelMovie { user, movie } = args;
    require_admin(&ctx)?;
    let guild = guild(&ctx)?;
    let movie = ui::clean_movie(movie.as_deref().unwrap_or("")).map_err(user_error)?;
    let id = user.id.get();
    let saved = movie.clone();
    let now = Utc::now().timestamp();
    let on_wheel = ctx
        .bot
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            let season = store::ensure_season(&tx, guild.get(), now)?;
            let on_wheel = store::set_movie(&tx, season, id, saved.as_deref())?;
            tx.commit()?;
            Ok(on_wheel)
        })
        .await?;

    let names = names::lookup(&ctx.bot, guild, &[id]).await;
    let name = ui::name(&names, id);
    let text = match (on_wheel, movie) {
        (false, _) => format!("{name} isn't on the wheel. Add them with /wheel add."),
        (true, Some(movie)) => format!("{name}'s movie is now {movie}."),
        (true, None) => format!("Cleared {name}'s movie."),
    };
    if on_wheel {
        info!("{text}");
    }
    let text = format!("{text}\n{}", redraw_note(&ctx.bot, guild).await);
    ctx.say(text).await?;
    Ok(())
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "bet",
    desc = "Add, change or remove a bet in any round of the current season (admins)"
)]
pub struct WheelBet {
    /// Who made the bet
    by: User,
    /// Who the bet is on
    on: User,
    /// How much was bet; 0 removes the bet
    #[command(min_value = 0, max_value = 1_000_000_000)]
    amount: i64,
    /// Which round
    #[command(min_value = 1)]
    round: i64,
}

async fn insert_bet(ctx: CommandCtx, args: WheelBet) -> Result<()> {
    let WheelBet {
        by,
        on,
        amount,
        round,
    } = args;
    require_admin(&ctx)?;
    let guild = guild(&ctx)?;
    // Discord enforces the range too; this guards the ledger's math either way.
    if !(0..=1_000_000_000).contains(&amount) {
        return Err(user_error("The amount must be from 0 to 1,000,000,000."));
    }
    let bet = Bet {
        by: by.id.get(),
        on: on.id.get(),
        amount,
    };
    let saved = bet.clone();
    let changed =
        ctx.bot
            .db
            .call(move |conn| {
                let tx = conn.transaction()?;
                let game = active_game(&tx, guild.get())?;
                let rounds = &game.season.rounds;
                let found = rounds.iter().find(|r| r.number == round).ok_or_else(|| {
                    user_error(format!("Pick a round from 1 to {}.", rounds.len()))
                })?;
                let changed = if saved.amount == 0 {
                    store::remove_bet(&tx, found.id, saved.by, saved.on)?
                } else {
                    store::place_bet(&tx, found.id, &saved)?;
                    true
                };
                tx.commit()?;
                Ok(changed)
            })
            .await?;

    let names = names::lookup(&ctx.bot, guild, &[bet.by, bet.on]).await;
    let (by, on) = (ui::name(&names, bet.by), ui::name(&names, bet.on));
    let text = match (amount, changed) {
        (0, true) => format!("Removed the bet by {by} on {on} in round {round}."),
        (0, false) => format!("{by} has no bet on {on} in round {round}."),
        _ => format!("Set the bet by {by} on {on} in round {round} to {amount}."),
    };
    if changed {
        info!("{text}");
    }
    ctx.say(text).await?;
    Ok(())
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "reset",
    desc = "Start a new season of the wheel; the old one stays viewable (admins)"
)]
pub struct WheelReset {
    /// Keep the current wheel options
    keep_options: Option<bool>,
}

async fn reset_wheel(ctx: CommandCtx, args: WheelReset) -> Result<()> {
    let WheelReset { keep_options } = args;
    require_admin(&ctx)?;
    let guild = guild(&ctx)?;
    // The button remembers which season it ends (0: none), so it can't end a later one.
    let season = ctx
        .bot
        .db
        .call(move |conn| Ok(store::active_season(conn, guild.get())?))
        .await?
        .unwrap_or(0);
    let (text, button) = ui::reset_confirmation(keep_options.unwrap_or(false), season);
    ctx.send(Draft::new().content(text).components(button))
        .await?;
    Ok(())
}

fn guild(ctx: &CommandCtx) -> Result<GuildId> {
    ctx.guild_id()
        .context("guild_only command without a server")
}

fn require_admin(ctx: &CommandCtx) -> Result<()> {
    if ctx.bot.is_admin(ctx.author_id()) {
        Ok(())
    } else {
        Err(user_error("Only admins can use this command."))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn subcommands_have_short_names() {
        let wheel = super::wheel().definition;
        let names: Vec<&str> = wheel.options.iter().map(|c| c.name.as_ref()).collect();
        assert_eq!(wheel.name, "wheel");
        assert_eq!(names, ["status", "add", "movie", "bet", "reset"]);
    }
}
