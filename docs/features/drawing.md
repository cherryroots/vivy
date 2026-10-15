# Drawing

Vivy can draw pictures with OpenAI's GPT Image 2.5 and post them as a reply. She can also change a picture or draw from one: someone attaches a photo and asks "put a hat on this", or replies to a picture she drew with "make it night".

## Using it

There are no commands. In chat, ask Vivy for a picture, or to change one you show her; the chat model then uses the `draw_picture` tool. She doesn't draw on her own: the tool's description says to use it when someone asks for a picture or an edit, and the chat prompt leaves that to the tool.

The tool is offered when `OPENAI_TOKEN` is set in `.env`, whichever provider chats (Claude can call it too), and only in normal chat answers (not in chime-ins, whose tool list is an allow-list in `src/features/chat/chime.rs`).

Settings live under `[features.drawing]`: `model`, `edit_model`, `quality`, `format`, `moderation`, `monthly_pictures` (all servers together, default 100) and `per_user_monthly_pictures` (for one person, default 20, so nobody uses up the month alone; 0 turns either limit off). `config.example.toml` explains each one.

## Files

All under `src/features/drawing/`. The Image API call itself is in `src/ai/openai/images.rs`, so any feature could draw (see [ai.md](../ai.md#pictures)).

| File | What it holds |
|---|---|
| `store.rs` | The `drawings` table and three queries: record a posted picture, count pictures and their cost since a time, and count one person's pictures since a time. |
| `mod.rs` | The `Drawing` feature: settings, the tool definition, finding the pictures to draw from (`pictures_in`), and `run_tool`. |

## How it works

### Steps in `run_tool`

1. Checks that `ctx.ai.images` exists (it does when `OPENAI_TOKEN` is set) and that the prompt isn't empty.
2. Checks `monthly_pictures` and `per_user_monthly_pictures` (the asker's pictures) against `drawings` for this UTC month (`over_limit`). Past either, she gets an error telling her to say so in text.
3. With `use_pictures`, fetches the asker's message and takes the still pictures (not GIFs or videos) from it and from the message it replies to, at most 4, through `util::media` (`pictures_around`). When the reply is to one of Vivy's own answers, her messages right next to that one count too (up to 4 each way, stopping at anyone else's message), because an answer's text and its picture are separate messages: "make it night" works replying to either. None found is an error she can read.
4. Draws it with `Images::draw`: `model` for a new picture, `edit_model` when there are pictures to draw from. `shape` picks the size: square 1024x1024 (the default), landscape 1536x1024, portrait 1024x1536.
5. Posts the picture (`drawing.png`) as a reply to the asker, pinging only them.
6. Adds it to `Asker::posted` as `[Picture you drew] <prompt>`, so chat treats it like part of the answer: a reply to it continues the conversation, ❌ deletes it, and the history keeps what she drew. Records it in `drawings`.
7. Tells her the picture is her answer, so she writes at most a short line with it.

### The two models

GPT Image 2.5 comes in two versions at the same price. OpenAI suggests Flare for "fast, high-quality everyday image generation" and Sunburst "where editing precision matters most", so the defaults are `model = "gpt-image-2.5-flare"` and `edit_model = "gpt-image-2.5-sunburst"`.

### Cost

Pictures are priced per token: text in $5, pictures in $8 and pictures out $30 per million (checked 2026-10-11). Higher `quality` makes more output tokens. Each picture's estimate is added to OpenAI's spend as the job `drawing` (shown on the status picture under "OpenAI spend this month") and kept per picture in `drawings`. With `OPENAI_ADMIN_KEY`, OpenAI's real bill replaces the month's total.

A refusal by OpenAI's safety system (a 400) goes back to her as the tool's result, so she can explain or try a different prompt; it isn't logged as an error.

## Chat tools

- **`draw_picture`**: `prompt` (required), `shape` (`square`, `landscape` or `portrait`), `use_pictures` (true to draw from the pictures in the asker's message and the one it replies to).

Other features run the same tool, so the limits count their pictures too, and can pass arguments the chat model isn't told about: `reply: false` posts the picture on its own instead of as a reply, `caption` puts a line of text with it, `spoiler: true` names the file `SPOILER_drawing.png` so Discord hides it, and `from_avatars` (user IDs, at most 4) sends those members' profile pictures (512 px, their server one when set) along to draw from, with `edit_model`. The [Hunger Games](hunger-games.md#pictures) use them.

## Feature hooks it uses

- `name`: "drawing".
- `migrations`: `store::MIGRATIONS`.
- `tools`: `draw_picture`, or nothing without `OPENAI_TOKEN`.
- `tool_status`: "🎨 Drawing, this can take a minute…" in chat's status line.
- `run_tool`: the steps above.
- `stats`: "Drawn this month" (drawn, or drawn / limit) and "Cost". Nothing without the key.

## Tables

From `store.rs`:

1. **`drawings`**: the Discord message ID (key), guild ID (none in DMs), channel ID, who asked, the prompt, the estimated cost in USD, and when it was posted. Indexed by time.

## Background loops

None.
