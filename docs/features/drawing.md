# Drawing

Vivy can draw pictures with OpenAI's GPT Image 2.6; each one goes out attached to her answer, so the words and the picture are one message. She can also change pictures or draw from them: someone attaches a photo and asks "put a hat on this", replies to a picture she drew with "make it night", or asks for one person's profile picture mixed with another's ("put Circle in raffle's picture"). She picks which pictures to use herself.

## Using it

There are no commands. In chat, ask Vivy for a picture, or to change one you show her; the chat model then uses the `draw_picture` tool. She doesn't draw on her own: the tool's description says to use it when someone asks for a picture or an edit, and the chat prompt leaves that to the tool.

The tool is offered when `OPENAI_TOKEN` is set in `.env`, whichever provider chats (Claude can call it too), and only in normal chat answers (not in chime-ins, whose tool list is an allow-list in `src/features/chat/chime.rs`).

Settings live under `[features.drawing]`: `model`, `edit_model`, `quality`, `format`, `moderation`, `monthly_pictures` (all servers together, default 100) and `per_user_monthly_pictures` (for one person, default 20, so nobody uses up the month alone; 0 turns either limit off). `config.example.toml` explains each one. The games' pictures (job `game_art`) use the less strict filter and the quality the game asks for (`quality` in the tool's arguments: the Hunger Games' `picture_quality` and `victor_picture_quality`, medium and high by default), or `high` when it doesn't say.

## Files

All under `src/features/drawing/`. The Image API call itself is in `src/ai/openai/images.rs`, so any feature could draw (see [ai.md](../ai.md#pictures)).

| File | What it holds |
|---|---|
| `store.rs` | The `drawings` table and three queries: record a posted picture, count pictures and their cost since a time, and count one person's pictures since a time. |
| `mod.rs` | The `Drawing` feature: settings, the tool definition, reading which pictures to draw from (`sources`) and loading them (`load_pictures`, `pictures_around`), and `run_tool_with_pictures`. |

Profile pictures are found and downloaded by `src/util/people.rs`, which chat's `look_at_profile_picture` uses too.

## How it works

### Steps in `run_tool_with_pictures`

1. Checks that `ctx.ai.images` exists (it does when `OPENAI_TOKEN` is set) and that the prompt isn't empty.
2. Checks `monthly_pictures` and `per_user_monthly_pictures` (the asker's pictures) against `drawings` for this UTC month (`over_limit`). Past either, she gets an error telling her to say so in text.
3. Loads the pictures she listed in `pictures`, in her order (each source once, at most 4 pictures in all; the first is the base, and her prompt says what to do with each):
   - `"message"`: fetches the asker's message and takes the still pictures (not GIFs or videos) from it and from the message it replies to, through `util::media` (`pictures_around`). When the reply is to one of Vivy's own answers, her messages right next to that one count too (up to 4 each way, stopping at anyone else's message): a long answer spans several messages, and answers from before #160 had their picture in a message of its own. None found is an error she can read.
   - anything else is a person: a mention, user ID or name (searched in the server), or `"me"` for her own face. Their profile picture for this server if they set one, else their main one, as a 1024 px PNG (`util::people::profile_picture`). Nobody found is an error she can read.
4. Draws it with `Images::draw`: `model` for a new picture, `edit_model` when there are pictures to draw from. `shape` picks the size: square 1024x1024 (the default), landscape 1536x1024, portrait 1024x1536.
5. Hands the picture (`drawing.png`) to chat as a file of the answer (`ToolReply::files`), with the note `[Picture you drew] <prompt>` for the history. Chat attaches it to the answer's last message when the answer ends (even when it stopped or failed), so a reply to it continues the conversation, ❌ deletes it with the answer, and 🔁 replaces it. Records it in `drawings`.
6. Tells her the picture goes out with her answer, so she writes at most a short line with it.

Before #160 the picture was posted as a message of its own, right after the start of her reply, so an answer with a picture came as two messages: her text (edited as it streamed) and then the picture.

### The two models

GPT Image 2.6 (since #155; it replaced 2.5 and draws about 50% faster) comes in two versions at the same price. OpenAI suggests Flare for "fast, high-quality everyday image generation" and Sunburst "where editing precision matters most" (2.6's Sunburst also keeps details more consistent from one edit to the next), so the defaults are `model = "gpt-image-2.6-flare"` and `edit_model = "gpt-image-2.6-sunburst"`.

### Cost

Pictures are priced per token: text in $5, pictures in $8 and pictures out $30 per million (checked for 2.5 on 2026-10-11; 2.6's output price is reported unchanged, its input prices weren't rechecked). Higher `quality` makes more output tokens. Each picture's estimate is added to OpenAI's spend as the job `drawing`, or `game_art` for the games' pictures (each its own line on the status picture under "OpenAI spend this month") and kept per picture in `drawings`. With `OPENAI_ADMIN_KEY`, OpenAI's real bill replaces the month's total.

A refusal by OpenAI's safety system (a 400) goes back to her as the tool's result, so she can explain or try a different prompt; it isn't logged as an error.

## Chat tools

- **`draw_picture`**: `prompt` (required), `shape` (`square`, `landscape` or `portrait`), `pictures` (a list, in order: `"message"` for the pictures in the asker's message and the one it replies to, or a person, whose profile picture is used). `use_pictures` (before #160) is gone; `["message"]` does the same.

Other features run the same tool, so the limits count their pictures too, and can pass arguments the chat model isn't told about: `post: false` hands the picture back in the tool's reply (`run_tool_with_pictures`, as an image) instead of posting it, so the feature can post it with its own message (it's still counted, under a made-up message ID from the time it was drawn), and `job: "game_art"` counts the picture's cost under `game_art` instead of `drawing` and uses the less strict safety filter (`moderation` "low") whatever the setting, since games with deaths in them are chosen by their host. The [Hunger Games](hunger-games.md#pictures) use them.

## Feature hooks it uses

- `name`: "drawing".
- `migrations`: `store::MIGRATIONS`.
- `tools`: `draw_picture`, or nothing without `OPENAI_TOKEN`.
- `tool_status`: "🎨 Drawing, this can take a minute…" in chat's status line.
- `run_tool_with_pictures`: the steps above.
- `stats`: "Drawn this month" (drawn, or drawn / limit) and "Cost". Nothing without the key.

## Tables

From `store.rs`:

1. **`drawings`**: an ID (key; a made-up one from the time it was drawn since #160, the picture's Discord message ID before), guild ID (none in DMs), channel ID, who asked, the prompt, the estimated cost in USD, and when it was drawn. Indexed by time.

## Background loops

None.
