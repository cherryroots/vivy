//! Drawing: Vivy draws pictures with OpenAI's GPT Image 2.5.
//!
//! Chat gets one tool, `draw_picture`: she writes a prompt, the picture is drawn with the
//! Image API (`ctx.ai.images`, see `ai/openai/images.rs`) and posted as a reply to the
//! asker. With `use_pictures`, the pictures in the asker's message and in the message it
//! replies to go along, so she can change a picture or draw from one ("put a hat on
//! this", or replying to a picture she drew: "make it night").
//!
//! New pictures use `model` (Flare: fast, good for everyday pictures); pictures made from
//! others use `edit_model` (Sunburst: the most precise at edits). Each one's cost counts
//! toward OpenAI's spend as the job "drawing".
//!
//! - `store.rs`: what was drawn, for the monthly limits and the status
//!
//! It needs `OPENAI_TOKEN` in .env, whichever provider chats. Without it the tool isn't
//! offered.
//!
//! Other features draw through the same tool (`run_tool_with_pictures`), so the monthly
//! limits count their pictures too. They can pass arguments the chat model isn't told
//! about: `post: false` hands the picture back (in the reply's `images`) instead of posting
//! it, `job: "game_art"` counts its cost on a line of its own in OpenAI's spend, and with
//! that job `quality` picks how good it is. The Hunger Games use them, to post each round's
//! pictures with the round.

mod store;

use anyhow::Context as _;
use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::ai::{PictureRequest, ToolDef, ToolReply};
use crate::core::{Asker, BotCtx, Feature, Result, Stat, user_error};
use crate::util::discord::{Draft, Message, Page, get_message, get_messages, ping_replied};
use crate::util::media::{self, MediaKind, ModelImage};
use crate::util::search::snowflake_at;
use crate::util::{env_key, month_start};

const TOOL: &str = "draw_picture";
/// The most pictures sent along to draw from.
const MAX_PICTURES: usize = 4;
/// How many messages before and after one of Vivy's answers are checked for its pictures.
const NEIGHBOURS: u16 = 4;

/// `[features.drawing]` settings.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// For new pictures: "gpt-image-2.5-flare" or "gpt-image-2.5-sunburst".
    pub model: String,
    /// For pictures made from other pictures.
    pub edit_model: String,
    /// "low", "medium", "high", "xhigh" or "max". Higher is better and costs more.
    pub quality: String,
    /// "png", "jpeg" or "webp".
    pub format: String,
    /// "auto", or "low" for a less strict safety filter.
    pub moderation: String,
    /// The most pictures a month, all servers together. 0 for no limit.
    pub monthly_pictures: u64,
    /// The most pictures a month for one person, so nobody can use up `monthly_pictures`
    /// alone. 0 for no limit per person.
    pub per_user_monthly_pictures: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            model: "gpt-image-2.5-flare".to_string(),
            edit_model: "gpt-image-2.5-sunburst".to_string(),
            quality: "medium".to_string(),
            format: "png".to_string(),
            moderation: "auto".to_string(),
            monthly_pictures: 100,
            per_user_monthly_pictures: 20,
        }
    }
}

/// Whether `OPENAI_TOKEN` is set, which the Image API needs.
fn has_key() -> bool {
    env_key("OPENAI_TOKEN").is_some()
}

/// Why no picture can be drawn now: the month's pictures are used up, for everyone or for
/// the asker. `drawn` is this month's count and `by_asker` the asker's part of it.
fn over_limit(settings: &Settings, drawn: u64, by_asker: u64) -> Option<String> {
    let limit = settings.monthly_pictures;
    if limit > 0 && drawn >= limit {
        return Some(format!(
            "the month's pictures are used up ({drawn} of {limit}, all servers together); drawing starts again on the 1st. Tell the asker plainly, and don't draw with code instead."
        ));
    }
    let own = settings.per_user_monthly_pictures;
    if own > 0 && by_asker >= own {
        return Some(format!(
            "the asker has had their {own} pictures for this month ({by_asker} drawn); it starts again on the 1st. Tell them plainly, and don't draw with code instead."
        ));
    }
    None
}

/// The size for a shape the model asks for. Anything else is square.
fn size_for(shape: Option<&str>) -> &'static str {
    match shape {
        Some("landscape") => "1536x1024",
        Some("portrait") => "1024x1536",
        _ => "1024x1024",
    }
}

/// The still pictures in a message and in the message it replies to, in that order:
/// what "this picture" means to the asker. GIFs and videos are left out.
fn pictures_in(msg: &Message) -> Vec<media::Media> {
    let mut found = media::find_media(msg);
    if let Some(replied) = &msg.referenced_message {
        found.extend(media::find_media(replied));
    }
    found.retain(|media| media.kind == MediaKind::Image);
    found.truncate(MAX_PICTURES);
    found
}

/// [`pictures_in`], plus the rest of Vivy's answer when the message replies to one of hers:
/// an answer's text and its picture are separate messages, so "make it night" replying to
/// the text should still find the picture. The answer's other messages are her messages
/// right next to the one replied to, up to [`NEIGHBOURS`] each way, until someone else's.
async fn pictures_around(ctx: &BotCtx, msg: &Message) -> Vec<media::Media> {
    let mut found = pictures_in(msg);
    if let Some(replied) = &msg.referenced_message
        && replied.author.id == ctx.bot_id
    {
        let before = Page::Before(replied.id);
        let after = Page::After(replied.id);
        // Discord lists newest first; walking away from the replied message means going
        // backwards through "before" and forwards through "after".
        let before = neighbours(ctx, msg, before).await;
        let mut after = neighbours(ctx, msg, after).await;
        after.reverse();
        for message in own_run(ctx, before).chain(own_run(ctx, after)) {
            found.extend(
                media::find_media(&message)
                    .into_iter()
                    .filter(|media| media.kind == MediaKind::Image),
            );
        }
        found.truncate(MAX_PICTURES);
    }
    found
}

async fn neighbours(ctx: &BotCtx, msg: &Message, which: Page) -> Vec<Message> {
    match get_messages(&ctx.http, msg.channel_id, which, NEIGHBOURS).await {
        Ok(messages) => messages,
        Err(err) => {
            warn!("couldn't read the messages around an answer: {err:#}");
            Vec::new()
        }
    }
}

/// The bot's own messages at the start of `messages`, stopping at the first from anyone else.
fn own_run(ctx: &BotCtx, messages: Vec<Message>) -> impl Iterator<Item = Message> {
    let bot = ctx.bot_id;
    messages
        .into_iter()
        .take_while(move |message| message.author.id == bot)
}

pub struct Drawing;

#[async_trait]
impl Feature for Drawing {
    fn name(&self) -> &'static str {
        "drawing"
    }

    fn migrations(&self) -> &'static [&'static str] {
        store::MIGRATIONS
    }

    fn tools(&self) -> Vec<ToolDef> {
        if !has_key() {
            return Vec::new();
        }
        vec![ToolDef {
            name: TOOL,
            description: "Draw a picture and post it as a reply to the asker. Use it when someone asks you for a picture (to draw, paint or make one) or to change one they show you; not on your own. It makes real pictures: don't draw with code instead. \
Write the prompt in English as a clear description of the whole picture: the subject, what's happening, the style (photo, anime, watercolor, pixel art...), colors, light and framing. Put any words that should appear in the picture in quotes. \
To change a picture or draw from one (\"put a hat on this\", \"me as a knight\", a reply to a picture you drew: \"make it night\"), set use_pictures: the pictures in the asker's message and in the message it replies to are sent along, and the prompt says what to do with them. \
To draw yourself or someone here, look at their profile picture first (look_at_profile_picture) and describe their looks in the prompt: hair, eyes, clothes, colors. \
Drawing takes up to a minute. The picture is your answer, so write at most a short line with it.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "prompt": {"type": "string", "description": "What to draw, or what to change in the pictures."},
                    "shape": {"type": "string", "enum": ["square", "landscape", "portrait"], "description": "Square if left out."},
                    "use_pictures": {"type": "boolean", "description": "Send along the pictures in the asker's message and the one it replies to."}
                },
                "required": ["prompt"]
            }),
        }]
    }

    fn tool_status(&self, _tool: &str, _args: &Value) -> Option<&'static str> {
        Some("🎨 Drawing, this can take a minute…")
    }

    async fn run_tool(
        &self,
        ctx: &BotCtx,
        asker: &Asker,
        name: &str,
        args: &Value,
    ) -> Result<String> {
        Ok(self
            .run_tool_with_pictures(ctx, asker, name, args)
            .await?
            .text)
    }

    async fn run_tool_with_pictures(
        &self,
        ctx: &BotCtx,
        asker: &Asker,
        _name: &str,
        args: &Value,
    ) -> Result<ToolReply> {
        let images = ctx
            .ai
            .images
            .as_ref()
            .ok_or_else(|| user_error("drawing isn't set up (no OPENAI_TOKEN)"))?;
        let settings: Settings = ctx.config.feature("drawing")?;
        let prompt = args
            .get("prompt")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| user_error("give a prompt: what to draw"))?
            .to_string();
        let since = month_start(Utc::now());
        let user = asker.user.get();
        let ((drawn, _), by_asker) = ctx
            .db
            .call(move |conn| {
                Ok((
                    store::drawn_since(conn, since)?,
                    store::drawn_for_since(conn, user, since)?,
                ))
            })
            .await?;
        if let Some(refusal) = over_limit(&settings, drawn, by_asker) {
            return Err(user_error(refusal));
        }

        let mut pictures: Vec<ModelImage> = Vec::new();
        if args.get("use_pictures").and_then(Value::as_bool) == Some(true) {
            let msg = get_message(&ctx.http, asker.channel, asker.message)
                .await
                .context("reading the asker's message")?;
            for found in pictures_around(ctx, &msg).await {
                match media::load_for_model(&found).await {
                    Ok(loaded) => pictures.extend(loaded),
                    Err(err) => warn!("couldn't download a picture to draw from: {err:#}"),
                }
            }
            if pictures.is_empty() {
                return Err(user_error(
                    "there are no pictures in the asker's message or the one it replies to. Draw without use_pictures, or ask for the picture.",
                ));
            }
        }

        let shape = args.get("shape").and_then(Value::as_str);
        let model = match pictures.is_empty() {
            true => &settings.model,
            false => &settings.edit_model,
        };
        let request = PictureRequest {
            model,
            prompt: &prompt,
            pictures: &pictures,
            size: size_for(shape),
            quality: quality(&settings, args),
            format: &settings.format,
            moderation: moderation(&settings, args),
            job: job(args),
        };
        let picture = images.draw(&request).await?;
        info!(channel = %asker.channel, model, usd = picture.usd, "drew a picture");

        // Another feature posts the picture itself, with its own message.
        let handed_back = args.get("post").and_then(Value::as_bool) == Some(false);
        let (message_id, reply) = if handed_back {
            let mime = match picture.format.as_str() {
                "jpeg" => "image/jpeg",
                "webp" => "image/webp",
                _ => "image/png",
            };
            let reply = ToolReply {
                text: "The picture is drawn and handed back, not posted.".to_string(),
                images: vec![ModelImage {
                    mime,
                    data: picture.data,
                }],
            };
            // Not posted, so there's no message ID: a made-up one from the time, unique
            // enough to count it.
            let id = snowflake_at(Utc::now()) + fastrand::u64(..1 << 22);
            (id, reply)
        } else {
            let name = format!("drawing.{}", picture.format);
            let message = Draft::new()
                .file(name, picture.data)
                .reply_to(asker.message)
                .allowed_mentions(ping_replied())
                .send(&ctx.http, asker.channel)
                .await
                .context("posting the picture")?;
            asker
                .posted
                .add(message.id, format!("[Picture you drew] {prompt}"));
            (
                message.id.get(),
                ToolReply::from("The picture is posted as a reply to the asker. It is your answer: write at most a short line with it, and don't describe it.".to_string()),
            )
        };

        let drawn = store::Drawn {
            message_id,
            guild_id: asker.guild.map(|g| g.get()),
            channel_id: asker.channel.get(),
            user_id: asker.user.get(),
            prompt,
            usd: picture.usd,
            at: Utc::now().timestamp(),
        };
        ctx.db
            .call(move |conn| Ok(store::record(conn, &drawn)?))
            .await?;
        Ok(reply)
    }

    async fn stats(&self, ctx: &BotCtx) -> Result<Vec<Stat>> {
        if !has_key() {
            return Ok(Vec::new());
        }
        let settings: Settings = ctx.config.feature("drawing")?;
        let since = month_start(Utc::now());
        let (drawn, usd) = ctx
            .db
            .call(move |conn| Ok(store::drawn_since(conn, since)?))
            .await?;
        let pictures = match settings.monthly_pictures {
            0 => drawn.to_string(),
            limit => format!("{drawn} / {limit}"),
        };
        Ok(vec![
            Stat::new("Drawn this month", pictures),
            Stat::new("Cost", format!("${usd:.2}")),
        ])
    }
}

/// The spend job a picture is counted under: "drawing", unless a game asked for its own.
fn job(args: &Value) -> &'static str {
    match args.get("job").and_then(Value::as_str) {
        Some("game_art") => "game_art",
        _ => "drawing",
    }
}

/// OpenAI's safety filter for a picture: the setting, except that games ask for the less
/// strict one, since their host chose a game with deaths in it.
fn moderation<'a>(settings: &'a Settings, args: &Value) -> &'a str {
    match job(args) {
        "game_art" => "low",
        _ => &settings.moderation,
    }
}

/// How good a picture is: the setting, except that games pick their own: `quality` in the
/// arguments, or else high (their maps are full of small sprites).
fn quality<'a>(settings: &'a Settings, args: &Value) -> &'a str {
    match job(args) {
        "game_art" => match args.get("quality").and_then(Value::as_str) {
            Some("low") => "low",
            Some("medium") => "medium",
            Some("xhigh") => "xhigh",
            Some("max") => "max",
            _ => "high",
        },
        _ => &settings.quality,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::discord::test_message;
    use twilight_model::channel::Attachment;

    fn attachment(url: &str, content_type: &str) -> Attachment {
        serde_json::from_value(json!({
            "id": "1", "filename": "a", "size": 1, "url": url, "proxy_url": url,
            "content_type": content_type,
        }))
        .unwrap()
    }

    fn message(attachments: Vec<Attachment>) -> Message {
        let mut msg = test_message(json!({}));
        msg.attachments = attachments;
        msg
    }

    #[test]
    fn games_count_their_pictures_apart() {
        assert_eq!(job(&json!({"job": "game_art"})), "game_art");
        assert_eq!(job(&json!({"job": "chat"})), "drawing");
        assert_eq!(job(&json!({})), "drawing");
    }

    #[test]
    fn games_use_the_less_strict_filter() {
        let settings = Settings::default();
        assert_eq!(moderation(&settings, &json!({"job": "game_art"})), "low");
        assert_eq!(moderation(&settings, &json!({})), "auto");
        assert_eq!(quality(&settings, &json!({"job": "game_art"})), "high");
        let medium = json!({"job": "game_art", "quality": "medium"});
        assert_eq!(quality(&settings, &medium), "medium");
        assert_eq!(quality(&settings, &json!({"quality": "low"})), "medium");
        assert_eq!(quality(&settings, &json!({})), "medium");
    }

    #[test]
    fn finds_the_pictures_to_draw_from() {
        let mut msg = message(vec![
            attachment("https://cdn/a.png", "image/png"),
            attachment("https://cdn/b.gif", "image/gif"),
        ]);
        msg.referenced_message = Some(Box::new(message(vec![attachment(
            "https://cdn/c.webp",
            "image/webp",
        )])));
        let urls: Vec<String> = pictures_in(&msg).into_iter().map(|m| m.url).collect();
        assert_eq!(urls, ["https://cdn/a.png", "https://cdn/c.webp"]);
        assert!(pictures_in(&message(Vec::new())).is_empty());
    }

    #[test]
    fn limits_for_everyone_and_each_person() {
        let settings = Settings::default();
        assert_eq!(over_limit(&settings, 0, 0), None);
        assert_eq!(over_limit(&settings, 99, 19), None);
        assert!(over_limit(&settings, 100, 0).unwrap().contains("used up"));
        assert!(
            over_limit(&settings, 50, 20)
                .unwrap()
                .contains("for this month")
        );
        // 0 turns a limit off.
        let open = Settings {
            monthly_pictures: 0,
            per_user_monthly_pictures: 0,
            ..Settings::default()
        };
        assert_eq!(over_limit(&open, 1000, 1000), None);
    }

    #[test]
    fn shapes_and_defaults() {
        assert_eq!(size_for(Some("landscape")), "1536x1024");
        assert_eq!(size_for(Some("portrait")), "1024x1536");
        assert_eq!(size_for(Some("round")), "1024x1024");
        assert_eq!(size_for(None), "1024x1024");
        let settings = Settings::default();
        assert_eq!(settings.model, "gpt-image-2.5-flare");
        assert_eq!(settings.edit_model, "gpt-image-2.5-sunburst");
    }
}
