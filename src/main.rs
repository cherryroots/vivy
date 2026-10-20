//! Vivy: load the config, open the database, connect to Discord, and hand events to the
//! features in `features::all()`.

mod ai;
mod core;
mod features;
mod util;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use chrono::Utc;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{error, info, warn};
use twilight_cache_inmemory::{DefaultInMemoryCache, ResourceType};
use twilight_gateway::{
    Config as ShardConfig, Event, EventTypeFlags, Intents, Shard, StreamExt as _,
};

use crate::core::config::Config;
use crate::core::db::{self, Db};
use crate::core::gateway::Gateway;
use crate::core::interaction::FeatureCommand;
use crate::core::legacy::{self, Outcome};
use crate::core::logging::{self, LogLine};
use crate::core::{BotCtx, Feature, Timers, dispatcher};
use crate::util::discord::{ChannelId, GuildId, Http};
use crate::util::env_key;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Secrets come from `.env` (or the real environment). A missing file is fine.
    let _ = dotenvy::dotenv();
    let config_path = std::env::var("VIVY_CONFIG").unwrap_or_else(|_| "config.toml".into());
    let config = Config::load(Path::new(&config_path))?;
    let log_queue = logging::init(&config.logging)?;

    if let Err(err) = run(config, log_queue).await {
        error!("startup failed: {err:#}");
        return Err(err);
    }
    Ok(())
}

async fn run(config: Config, log_queue: mpsc::Receiver<LogLine>) -> anyhow::Result<()> {
    let token =
        std::env::var("DISCORD_TOKEN").context("DISCORD_TOKEN is not set (put it in .env)")?;
    let config = Arc::new(config);
    let features = Arc::new(features::all());
    // Links from messages are downloaded with `media::safe_client` instead.
    let web = util::http::client_builder()
        .build()
        .context("creating the HTTP client")?;

    // Database: open, run every owner's migrations, then import voltgpt's data if present.
    let db = Db::open(&config.database)
        .await
        .context("opening the database")?;
    let migration_features = features.clone();
    db.call(move |conn| {
        db::migrate(conn, "core", db::CORE_MIGRATIONS)?;
        // Claude's saved uploads (see ai/claude/uploads.rs).
        db::migrate(conn, "claude", ai::claude::UPLOAD_MIGRATIONS)?;
        db::migrate(conn, "claude_keep_warm", ai::claude::KEEP_WARM_MIGRATIONS)?;
        for feature in migration_features.iter() {
            db::migrate(conn, feature.name(), feature.migrations())?;
        }
        Ok(())
    })
    .await
    .context("running migrations")?;
    let import_summary = import_legacy(&db, &features, &config).await;
    info!("database {} is ready", config.database);
    let (ai, keep_warm) = ai_from_env(&config, &web, &db);
    // Made before the bot connects, so the bill reader below can report to it too.
    let timers = Timers::default();
    // Cancelled when the bot stops; tasks spawned on `tasks` are waited for then.
    let shutdown = CancellationToken::new();
    let tasks = TaskTracker::new();
    // Keeps chat's cached prompt from expiring between quiet stretches.
    if let Some(warm) = keep_warm {
        tasks.spawn(warm.run(timers.clone(), shutdown.clone()));
    }
    // With an Admin API key, a provider's spend comes from its real bill (see ai/spend.rs).
    // Google has no Admin API for the bill, so Gemini's spend is always the estimate.
    for spend in &ai.spends {
        let (var, billed) = match spend.provider() {
            "claude" => ("ANTHROPIC_ADMIN_KEY", config.ai.claude.billed_spend),
            "openai" => ("OPENAI_ADMIN_KEY", config.ai.openai.billed_spend),
            _ => continue,
        };
        let Some(admin_key) = env_key(var) else {
            continue;
        };
        if !billed {
            spend.set_admin_key(ai::AdminKey::Unused);
        } else if spend.provider() == "claude" {
            let billing = ai::claude::billing::Billing::new(web.clone(), admin_key);
            tasks.spawn(
                spend
                    .clone()
                    .follow_bill(billing, timers.clone(), shutdown.clone()),
            );
        } else {
            let billing = ai::openai::billing::Billing::new(web.clone(), admin_key);
            tasks.spawn(
                spend
                    .clone()
                    .follow_bill(billing, timers.clone(), shutdown.clone()),
            );
        }
    }

    // Discord's REST API, and who the bot is.
    let http = Arc::new(Http::new(token.clone()));
    let me = http
        .current_user()
        .await
        .context("logging in to Discord (is DISCORD_TOKEN right?)")?
        .model()
        .await?;
    let application_id = http
        .current_user_application()
        .await
        .context("reading the bot's Discord application")?
        .model()
        .await?
        .id;
    let app_owners = crate::core::ctx::app_owners(&http).await;

    // Slash commands, each remembering its feature so it only runs where that feature is on.
    let mut commands = Vec::new();
    for feature in features.iter() {
        for command in feature.commands() {
            commands.push(FeatureCommand {
                feature: feature.name(),
                command,
            });
        }
    }

    // The gateway connections. Message content and presences are privileged: both need
    // turning on in the Developer Portal.
    let intents = Intents::GUILDS
        | Intents::GUILD_MESSAGES
        | Intents::GUILD_MESSAGE_REACTIONS
        | Intents::GUILD_EMOJIS_AND_STICKERS
        | Intents::DIRECT_MESSAGES
        | Intents::DIRECT_MESSAGE_REACTIONS
        | Intents::MESSAGE_CONTENT
        | Intents::GUILD_PRESENCES;
    let shards: Vec<Shard> = twilight_gateway::create_recommended(
        &http,
        ShardConfig::new(token, intents),
        |_, builder| builder.build(),
    )
    .await
    .context("asking Discord how to connect")?
    .collect();
    let gateway = Arc::new(Gateway::new(
        shards.iter().map(|shard| shard.sender()).collect(),
    ));
    // What the gateway tells us about servers, channels, members and emoji. Messages
    // aren't kept: features fetch the ones they need.
    let cache = Arc::new(
        DefaultInMemoryCache::builder()
            .resource_types(ResourceType::all() - ResourceType::MESSAGE)
            .build(),
    );

    let (events, _) = broadcast::channel(100);
    let bot = BotCtx {
        http,
        cache,
        gateway,
        application_id,
        commands: Arc::new(commands),
        db,
        config: config.clone(),
        ai,
        web,
        events,
        shutdown: shutdown.clone(),
        tasks: tasks.clone(),
        timers,
        features: features.clone(),
        bot_id: me.id,
        bot_name: me.name.clone(),
        app_owners: Arc::new(app_owners),
        started_at: Utc::now(),
    };
    if let Some(channel) = config.logging.log_channel {
        logging::spawn_discord_poster(
            bot.http.clone(),
            ChannelId::new(channel),
            log_queue,
            bot.shutdown.clone(),
            &bot.tasks,
        );
    }

    // One task per shard reads its events. The first Ready starts the features.
    let started = Arc::new(std::sync::Mutex::new(Some(import_summary)));
    info!("connecting to Discord");
    let mut shard_tasks = tokio::task::JoinSet::new();
    for (number, shard) in shards.into_iter().enumerate() {
        shard_tasks.spawn(run_shard(bot.clone(), shard, number, started.clone()));
    }

    // Run until Ctrl+C or SIGTERM, or until Discord refuses the connection for good (a
    // wrong token, or an intent that isn't turned on in the Developer Portal).
    let refused = tokio::select! {
        () = shutdown_signal() => false,
        _ = shard_tasks.join_next() => true,
    };
    if refused {
        error!("the Discord connection stopped and can't reconnect");
    }
    info!(target: "lifecycle", "🔴 **Shutting down**");
    // Tell background tasks to stop, then give them a moment to finish (the control
    // panel marks itself offline, the log poster sends its last lines).
    shutdown.cancel();
    tasks.close();
    if tokio::time::timeout(Duration::from_secs(10), tasks.wait())
        .await
        .is_err()
    {
        warn!("background tasks didn't stop within 10 seconds");
    }
    bot.gateway.close();
    let _ = tokio::time::timeout(Duration::from_secs(2), shard_tasks.join_all()).await;
    if refused {
        anyhow::bail!("the Discord connection stopped");
    }
    Ok(())
}

/// Reads one shard's events until the bot stops: the cache sees each one first, then the
/// dispatcher hands it to the features. `started` holds the import summary until the first
/// Ready, which runs [`on_first_ready`].
async fn run_shard(
    bot: BotCtx,
    mut shard: Shard,
    number: usize,
    started: Arc<std::sync::Mutex<Option<String>>>,
) {
    while let Some(item) = shard.next_event(EventTypeFlags::all()).await {
        let event = match item {
            Ok(event) => event,
            Err(err) => {
                warn!("error from the Discord connection: {err}");
                continue;
            }
        };
        if let Event::GatewayClose(frame) = &event {
            if bot.shutdown.is_cancelled() {
                break;
            }
            // Most closes are followed by a reconnect. 4014 means an intent (message
            // content or presences) isn't turned on in the Developer Portal.
            warn!("Discord closed the connection: {frame:?}");
        }
        bot.cache.update(&event);
        bot.gateway
            .record_latency(number, shard.latency().average());
        if let Event::Ready(ready) = &event {
            let summary = started.lock().unwrap().take();
            if let Some(summary) = summary {
                let guilds = ready.guilds.iter().map(|guild| guild.id).collect();
                on_first_ready(&bot, guilds, summary).await;
            }
        }
        dispatcher::handle_event(&bot, event).await;
    }
}

/// Runs once, when Discord first says we're connected: registers the slash commands, starts
/// delivering bot events, and starts every enabled feature. Then the 🟢 Started line.
async fn on_first_ready(bot: &BotCtx, guilds: Vec<GuildId>, import_summary: String) {
    // Failing to register isn't worth stopping for: the ones from the last start keep working.
    if let Err(err) = register_commands(bot, &guilds).await {
        error!("{err:#}");
    }
    dispatcher::spawn_bot_events(bot);

    let mut started = Vec::new();
    for feature in bot.features.iter() {
        if !bot.gate(feature.name()).enabled {
            continue;
        }
        match feature.start(bot).await {
            Ok(()) => started.push(feature.name()),
            Err(err) => error!(feature = feature.name(), "failed to start: {err:#}"),
        }
    }

    info!(
        target: "lifecycle",
        "🟢 **Started** Vivy {} (`{}`) as {} · features: {}{}",
        crate::core::VERSION,
        crate::core::GIT_COMMIT,
        bot.bot_name,
        started.join(", "),
        import_summary,
    );
}

/// Registers the slash commands globally, and removes per-server commands that voltgpt
/// registered under the same application, so they don't show up twice.
async fn register_commands(bot: &BotCtx, guilds: &[GuildId]) -> anyhow::Result<()> {
    let definitions: Vec<_> = bot
        .commands
        .iter()
        .map(|command| command.command.definition.clone())
        .collect();
    let interaction = bot.http.interaction(bot.application_id);
    interaction
        .set_global_commands(&definitions)
        .await
        .context("registering slash commands")?;
    for &guild in guilds {
        if let Err(err) = interaction.set_guild_commands(guild, &[]).await {
            warn!("couldn't clear old commands in server {guild}: {err}");
        }
    }
    Ok(())
}

/// Sets up the AI providers for chat: the one `provider` under `[ai]` names, and the
/// `fallback` one if it's set. Chat is off when the main provider's key is missing from
/// `.env`; a missing fallback key only means there's no fallback. With `OPENAI_TOKEN` set,
/// pictures can be drawn too, whichever provider chats.
/// The AI services, and what keeps Claude's chat cache warm when that's on.
fn ai_from_env(
    config: &Config,
    web: &reqwest::Client,
    db: &Db,
) -> (ai::Ai, Option<ai::claude::KeepWarm>) {
    // Each provider's spend is tracked whether it's the main provider or the fallback.
    let claude_spend = ai::Spend::new(db.clone(), "claude", config.ai.claude.monthly_budget);
    let openai_spend = ai::Spend::new(db.clone(), "openai", config.ai.openai.monthly_budget);
    let gemini_spend = ai::Spend::new(db.clone(), "gemini", config.ai.gemini.monthly_budget);
    let openai_key = env_key("OPENAI_TOKEN");
    let mut keep_warm = None;
    let mut make = |provider: ai::Provider| -> Option<Arc<dyn ai::ChatProvider>> {
        let name = match provider {
            ai::Provider::Claude => "ANTHROPIC_API_KEY",
            ai::Provider::Openai => "OPENAI_TOKEN",
            ai::Provider::Gemini => "GEMINI_API_KEY",
        };
        let Some(key) = env_key(name) else {
            warn!("{name} is not set in .env, so {provider:?} can't chat");
            return None;
        };
        Some(match provider {
            ai::Provider::Claude => {
                let claude = ai::Claude::new(
                    web.clone(),
                    key,
                    config.ai.claude.clone(),
                    Some(claude_spend.clone()),
                );
                keep_warm = claude.keep_warm();
                Arc::new(claude)
            }
            ai::Provider::Openai => Arc::new(ai::OpenAi::new(
                web.clone(),
                key,
                std::env::var("OPENAI_BASE").ok().as_deref(),
                config.ai.openai.clone(),
                Some(openai_spend.clone()),
            )),
            ai::Provider::Gemini => Arc::new(ai::Gemini::new(
                web.clone(),
                key,
                config.ai.gemini.clone(),
                Some(gemini_spend.clone()),
            )),
        })
    };
    let main = make(config.ai.provider);
    let backup = config
        .ai
        .fallback
        .filter(|&fallback| fallback != config.ai.provider)
        .and_then(make);
    if let Some(main) = &main {
        info!("chat uses {} ({})", main.name(), main.model());
    }
    if let Some(backup) = &backup {
        info!("chat falls back to {} ({})", backup.name(), backup.model());
    }
    let chats_with =
        |provider| config.ai.provider == provider || config.ai.fallback == Some(provider);
    let images =
        openai_key.map(|key| ai::Images::new(web.clone(), key, Some(openai_spend.clone())));
    let mut spends = Vec::new();
    if chats_with(ai::Provider::Claude) {
        spends.push(claude_spend);
    }
    // OpenAI's key is used for pictures too, so its spend shows whenever the key is set.
    if chats_with(ai::Provider::Openai) || images.is_some() {
        spends.push(openai_spend);
    }
    if chats_with(ai::Provider::Gemini) {
        spends.push(gemini_spend);
    }
    (ai::Ai::new(main, backup, spends, images), keep_warm)
}

/// Imports voltgpt's `old.db` if it's there. Returns a summary for the start notice.
async fn import_legacy(db: &Db, features: &[Arc<dyn Feature>], config: &Arc<Config>) -> String {
    let path = Path::new(&config.old_database);
    match legacy::import(db, features, path, config.clone()).await {
        Ok(None) => String::new(),
        Ok(Some(outcomes)) if outcomes.is_empty() => String::new(),
        Ok(Some(outcomes)) => {
            let mut parts = Vec::new();
            for outcome in outcomes {
                match outcome {
                    Outcome::Imported { part, rows } => parts.push(format!("{part} ({rows} rows)")),
                    Outcome::Failed { part, error } => {
                        error!("importing {part} from {} failed: {error}", path.display());
                        parts.push(format!("{part} failed"));
                    }
                }
            }
            format!(" · imported from {}: {}", path.display(), parts.join(", "))
        }
        Err(err) => {
            error!("importing {} failed: {err:#}", path.display());
            String::new()
        }
    }
}

/// Waits for Ctrl+C, or SIGTERM from `systemctl stop`.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
