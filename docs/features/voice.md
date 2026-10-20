# Voice

Vivy can answer with a real Discord voice message (the kind with a play button and a waveform) in her own voice. She writes a short script, ElevenLabs speaks it, and the bot posts it as a reply to the person she's answering. It's for when someone asks to hear her, or now and then when saying something fits better than typing it: a greeting, a laugh, a sleepy good night. Voice channels aren't part of it.

## Using it

There are no commands. In chat, ask Vivy to say something out loud; the chat model decides when to use the `send_voice_message` tool.

The tool is only offered when `ELEVENLABS_API_KEY` is set in `.env`, and only in normal chat answers (not in chime-ins, whose tool list is an allow-list in `src/features/chat/chime.rs` that leaves it out). Sending also needs `ffmpeg` installed and the Send Voice Messages permission in the channel.

Settings live under `[features.voice]`: `voice_id`, `voices`, `model`, `stability`, `similarity`, `language`, `monthly_characters` and `max_characters`. `config.example.toml` explains each one. Setup is in the README.

## Files

All under `src/features/voice/`.

| File | What it holds |
|---|---|
| `store.rs` | The `voice_messages` table and two queries: record a sent message, and count messages and characters since a time. |
| `speech.rs` | `speak`: the one ElevenLabs text-to-speech request. |
| `audio.rs` | `prepare`: turns the audio into an OGG Opus file with ffmpeg and measures its length and waveform. `waveform` is pure and tested on its own. |
| `mod.rs` | The `Voice` feature: settings, the tool definition, and `run_tool`, which checks the limits, calls the two files above, and posts the message. |

## How it works

### The script

The tool takes one argument, `script`: the spoken words, with ElevenLabs v4 audio tags in square brackets before the words they're for, like `[whispers]`, `[laughs]`, `[sleepy, slow]`. The tool description tells her: a tag lasts until the next one, several go in one bracket with commas, opposite ones (`[whispers, shouting]`) shouldn't be mixed, write spoken words only (no markdown, links, emoji or lists), keep it to one to four sentences, and let her current mood show in how she sounds. Only the script is sent to ElevenLabs, nothing else from the conversation.

### Other voices, for other features

`[features.voice.voices]` names other ElevenLabs voices. The default has `gamemaker` (`DucUc05HgR5M3wbY6WkG`), the voice the [Hunger Games](hunger-games.md) Gamemaker speaks with; an empty ID turns a voice off. Other features send voice messages through the same tool, by finding the feature that offers `send_voice_message` and calling `run_tool` (they don't import this feature), so the gate, the length limit and the monthly limit apply to them too. They pass two arguments the chat model isn't told about: `voice`, a name from `voices` (instead of `voice_id`), and `reply: false`, which posts the voice message on its own instead of as a reply.

### Steps in `run_tool`

1. Checks the key and that `voice_id` is set, and that the script isn't empty.
2. Checks the limits (below). Past one, she gets an error telling her to say less or answer in text.
3. `speech::speak` sends `POST https://api.elevenlabs.io/v1/text-to-speech/{voice_id}?output_format=opus_48000_128` with the key in the `xi-api-key` header and a JSON body: `text`, `model_id` (from `model`, default `eleven_v4`), `voice_settings` with `stability` and `similarity_boost` (from `similarity`), and `language_code` when `language` is set. The request may take 90 seconds. An error shows ElevenLabs' status and the start of its answer.
4. `audio::prepare` writes the audio to a temporary folder (some formats can't be read from a pipe) and runs ffmpeg twice, each with a 60 second limit:
   - re-encodes it as mono OGG Opus, 48 kHz, 64 kbit/s;
   - decodes it to raw 16-bit samples at 8 kHz, which give the length in seconds and the waveform: one bar per tenth of a second (at most 256, at least 1), each the loudest sample in its slice, scaled so the loudest bar is 255.
5. Posts the voice message (below).
6. Adds it to `Asker::posted` with the text `[Voice message] <script>`, and records it in `voice_messages`.
7. Returns a short text telling her the message is sent, and that it is her answer: no text after it, unless something can only be shown in writing (a link, code).

### Posting a voice message

A Discord voice message can't have text. It needs the message flag `IS_VOICE_MESSAGE` (`1 << 13`, a 1 shifted left 13 places, so bit 13) and exactly one audio attachment carrying `duration_secs` and a base64 `waveform`. Twilight's message builder can't carry those two fields, so `mod.rs` writes the JSON payload by hand and sends it with `util::discord::send_raw` together with the file `voice-message.ogg`. The payload also makes it a reply to the asker's message (`fail_if_not_exists: false`, so a deleted question doesn't fail it) and allows no mentions except the reply ping.

### How chat treats it

`Asker::posted` (in `src/core/feature.rs`) is the list of messages tools sent as part of an answer. Chat treats them like its own (see [chat](chat.md)):

- **The voice message can be the whole answer** (Cherry's choice). When she writes no text with it, chat deletes its text message instead of showing an empty answer.
- **A reply to it continues the conversation**, since its message ID is stored with the answer.
- **❌ deletes it** with the rest of the answer.
- **In the history** the answer's text is followed by `[Voice message] <script>`, so other models know what she said. The provider that wrote the answer gets its own raw rounds back instead (`native_json`).
- **🔁 deletes old voice messages** before writing the answer again, since a voice message can't be edited into text. Text messages are reused as usual.

### Limits

- `max_characters` (default 1000): the most characters in one script.
- `monthly_characters` (default 50000, 0 for no limit): all scripts in the current UTC month together, counted from `voice_messages`. ElevenLabs bills per character, audio tags included, and the count uses the whole script too.

## Chat tools

- **`send_voice_message`**: `script` (required). Offered only with `ELEVENLABS_API_KEY` set. Its description says when to use it: when someone asks to hear her, and now and then when saying it fits better than typing it. The chat system prompt (`src/features/chat/prompt.md`) doesn't repeat it: it only says each tool's description tells what it's for.

## Feature hooks it uses

`Voice` in `mod.rs` implements these methods of the `Feature` trait (see [architecture](../architecture.md)):

- `name`: "voice".
- `migrations`: `store::MIGRATIONS`.
- `tools`: `send_voice_message`, or nothing without the key.
- `tool_status`: "🎙️ Recording a voice message…" in chat's status line.
- `run_tool`: the steps above.
- `stats`: "Sent this month" and "Characters" (used, or used / limit), for the control panel. Nothing without the key.

It has no commands, no `chat_context` and no `start`.

## Tables

From `store.rs`:

1. **`voice_messages`**: the Discord message ID (key), guild ID (none in DMs), channel ID, the user she answered, characters sent to ElevenLabs, length in seconds, and when it was sent. Used for the monthly limit and the stats. Indexed by time.

## Background loops

None.
