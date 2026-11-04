# CLAUDE.md

Vivy is Cherry's Discord bot in Rust (Twilight, tokio, one SQLite file). It replaced [voltgpt](https://github.com/cherryroots/voltgpt), the older Go bot. The repo was called VoltBot until 2026-10-10; old links redirect.

Start with the docs, they are the source of truth for how things work:

- [`docs/feature-map.md`](docs/feature-map.md): every feature, its tables, the decisions, the PR history table, what's not built
- [`docs/architecture.md`](docs/architecture.md): startup, the `Feature` trait, events, config and gating, database, adding a feature
- [`docs/ai.md`](docs/ai.md): the providers (Claude, OpenAI, Gemini), jobs, caching, fallback, spend
- `docs/features/<feature>.md`: a page per feature
- `config.example.toml` explains every setting, `.env.example` every key

## Build and test

```bash
cargo build --release                 # what the server runs
cargo build --profile fast-test       # while changing code: incremental, 15 s to 2 min rebuilds
cargo test                            # no Discord or API key needed
cargo clippy && cargo fmt
```

Game Boy tests that need a ROM or models are `#[ignore]`d; see "Tests" in `docs/features/gameboy.md`.

## How Cherry likes to work

- **She is learning Rust.** Keep code simple, staged and easy to study. Features are event driven with guards. No clever abstractions.
- **Look for the elegant way.** "Don't keep brute forcing a method if something more elegant exists." When a fix turns into a pile of heuristics, step back and find the cleaner approach.
- **Questions get answers first.** Offer a PR rather than building something unasked. Bug reports (a panic, a log line, a pasted Vivy reply, a screenshot) and short change requests go straight to a PR. Server and sysadmin questions: just answer.
- **Pictures:** show a rendered sample before changing the wheel, status or any other picture.
- **Long text in Discord:** plain messages over embeds.
- **Admin commands:** gated by the `admins` list in `config.toml`, never by `default_member_permissions`. Cherry runs them as the bot's developer without server permissions, so Discord permissions would hide them from her.
- **Shared code** goes in a parent feature (like `games/`) or `util/`, never inside another feature. Features never import each other.

## Pull requests

- One PR per change, against `main`, never stacked on another PR.
- Every PR updates its feature's page in `docs/` and adds a row to the PR table in `docs/feature-map.md`. Parallel PRs conflict in that table: merge `main` into the branch and keep both rows.
- Never rebase or force-push. Merge `main` in instead.
- Cherry merges fast. Check whether the PR is already merged before pushing more to it; follow-ups after a merge go in a new PR from fresh `main`.
- Shipped migrations are never edited; add a new one.
- The sandbox can't reach Discord, so nothing is verified live until Cherry runs it. Say what was and wasn't checked.

## The server

- Cherry runs `./target/release/vivy` from `~/Vivy` on her server (`https://kirei.moe`), often from a phone over SSH. Web server is Caddy (not nginx); the Game Boy watch page is `https://kirei.moe/vivy/` through Caddy's `handle_path /vivy/*` to `web_bind = "127.0.0.1:8787"`.
- Vivy lives in one server (about 300 to 1000 messages a day). Her test server has every feature off with `deny_guilds`.
- Intents and permissions the Discord app needs are in the README's setup steps.
- The server is an Ubuntu 26.04 KVM VM: 8 AMD EPYC cores, 23.5 GB RAM, 295 GB disk, no GPU. Anything that needs a GPU runs on Cherry's own PC instead.
- Logs: under systemd, stdout lines carry a `<N>` syslog level prefix so `journalctl` colours them by level (#150); no `-o verbose` needed.
- Routine with code 4000 every few hours are Discord's doing, not a Vivy bug (Twilight resumes; see the control panel page).

## Money and tokens

- Claude is the main provider: Opus 5.5 for her voice (chat, chime-ins, reflection, diary), cheaper models for background jobs (`[ai.claude.jobs.*]`). Cherry has about $100 a month of API credit.
- OpenAI: Cherry is in the data-sharing program, so GPT-6 Astra, Luna and 6.1 Sol share 1M free tokens a day (UTC). The bot counts its own tokens because `store: false` calls don't show in OpenAI's logs. Free tokens train on what's sent, which is why OpenAI is only the chat fallback.
- **Test sessions share one count.** On 2026-10-22 several test sessions each kept their own count, went past 1M together and cost money. Gate tests now read and write one file, `VIVY_GB_TODAY` (default `/mnt/project-files/gameboy/today.json`), and stop at `VIVY_GB_FREE_TOKENS` (default 950,000, Cherry's cap). Check it before starting a run.

## Cloud sandbox keys

- The cloud environment holds test keys as env vars: `VIVY_ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY` (valid to about 2026-11-18). `ANTHROPIC_API_KEY` never reaches the sandbox, so the gate tests read `VIVY_ANTHROPIC_API_KEY` first.
- A session reads env vars only when it starts: after a key changes, start a new session.
- `api.openai.com` has to be in the environment's allowed domains. `ai.google.dev` was blocked by the proxy, so Gemini's API shapes came from docs summaries and weren't checked live (see `docs/ai.md`).
- Check key names only, never print values. Never write keys into the repo, docs or chat.

## Intended behaviour (don't "fix" these)

Cherry's rulings from the 2026-10-10 bug check and later:

- Self-reposts count as snails; different comments in one Reddit thread are the same post.
- `/memories/vivy/` (personality, mood, skills) is Vivy's alone: only she changes it, from what she thinks. People can't order her mood. Done through her instructions, not an author lock. Her mood line may mention server-specific people and things.
- Vivy may read and talk about any channel of her server that `[features.chat]` gating doesn't turn off, whether or not the asker can see it (Cherry's rule, #149). Other servers and other people's DMs stay refused.
- Chime-ins and check-ins only get the read-only `UNPROMPTED_TOOLS`; check-ins only with the person who asked.
- Channel gates cover threads (the thread id or its parent's).
- A missing `config.toml` or an unknown key warns, never stops the bot.
- A reminder mention that doesn't parse goes to chat.
- A new wheel season needs a confirm; buttons act only on the round they were made for. Classic seasons recomputing old payouts from current options is fine.
- Snooze buttons are 10 minutes, 1 hour and Pick a time. The "Tomorrow 9:00" button was removed on purpose.

## Decided against (don't re-propose without new facts)

- **Sonnet for her voice.** Opus stays for chat, chime-ins, diary and mood. Reflection is the only job worth trying on Sonnet (config only), undecided.
- **History on the 1-hour cache.** Most conversations are 1 to 7 messages, so it only pays if about 1 in 3 answers gets a reply 5 to 60 minutes later. Measure that on `chat_turns` before raising it again.
- **Splitting into crates.** Saves about 5 s per feature edit and nothing for core edits, and costs readability. Only `games/` might be worth it later. Building server deploys with `fast-test` was offered and declined.
- **Tool search / trimming tools.** The bot's tools are about 4k tokens and cached, below where tool search pays.
- **Tools as Anthropic Skills.** Skills are workspace-wide; per-server notes in `/memories/vivy/skills/` instead.
- **voltgpt's memory system, Wavespeed video.** Dropped in the port.

## Game Boy rules

The Game Boy feature (`src/features/games/gameboy/`, `docs/features/gameboy.md`) has rules from Cherry that shape every change:

- **No game-specific code.** The goal is that Vivy picks up any Game Boy game. Code gives her perception (screen, memory search, maps, motion); behaviour comes from skills. No game names or per-game addresses in the bot's code. Per-game data, if ever, only as data files. When one game fails, fix the general case and re-run the Red tests before pushing.
- **Skills are Vivy's own.** She and the strategist's review write them, not Cherry and not us.
- **Be persistent on essential tasks.** Stuck on something progression needs, she tries another way rather than wandering off.
- **Plan from where she ends up.** "Games aren't entirely deterministic so she should always go from where she ends up after an action and plan accordingly."
- **Test with replays and save states** when the free tokens are out. Replays replay without a model.
- **ROMs are Cherry's own** cartridges. Never download a ROM.
- Make it fun to watch. Cherry gave the Game Boy work free rein for incremental PRs that explore ideas.

Practical gotchas:

- Don't build into a second target folder during a live run; the disk filled once.
- Messages fetched over Discord's REST API have no `guild_id` or `member`; keep the gateway's when re-reading a message, or a server question looks like a DM (#153).
- #96 once committed a ~1 GB `target-run/` build folder (removed in #102, ignored as `target-*/`). It stays in git history; purging needs a force push, so only if Cherry asks.
- Adding frames to the look shifts Red's RNG, so old replays diverge.
- In waiting loops, never `pgrep -f` or `pkill -f` a string that's in the loop's own command line (it matches itself, and once killed the shell).
- Gate tests default to files in the project's shared folder: `/mnt/project-files/gameboy/tasks` (Red task states, `VIVY_GB_TASKS`), `gameboy/gate` (results), `gameboy/today.json` (the shared token count) and `gameboy/sessions/red-2026-10-27/red` (Cherry's Mt Moon saves for `rock_ledges_are_out_of_reach`). Start states for other games are in `gameboy/starts/`.
