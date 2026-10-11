# Movie wheel

The movie wheel is a betting game for movie night. Admins put people (the "options") on the wheel. Each round, players claim 100, bet on which option the wheel will pick, and an admin sets the winner. Players who bet too little are taxed. The round is shown as a picture with buttons under it for claiming, betting and setting the winner. Games are split into seasons, so an old game stays viewable after a new one starts. Each server has its own game.

## Using it

- **`/wheel_status [round] [season]`**: posts the status picture of a round (default: the current one) of a season (default: the one being played; seasons count from 1 per server). If the server has no game yet, this starts one. Past seasons are shown without buttons.
- **`/wheel_add user [user2 … user5] [remove]`** (admins): puts up to five people on the wheel at once, or takes them off with `remove:true`. The answer has a line per person.
- **`/insert_bet by on amount round`** (admins): sets a bet in any round of the current season by round number; `amount:0` removes it. This is for fixing mistakes, so it skips the betting rules.
- **`/reset_wheel [keep_options]`** (admins): asks for confirmation, then ends the season and starts a new one, empty or with the same options.
- **Buttons under an open round**: Claim!, Place Bet!, Remove Bet!, Set Winner! (admins), and in a second row Change Name and Help.
- **Buttons under a resolved round**: View Current Round, Undo Winner (admins, only when allowed), Change Name, Help.

Admins are the `admins` list at the top of `config.toml`. The feature's own section is `[features.wheel]` (only the common keys); the import uses the top-level `main_server`. `config.example.toml` explains them. Setup is in the README.

## Files

All in `src/features/wheel/`, starting with the pure logic.

| File | What it holds |
|---|---|
| `ledger.rs` | The money rules. Pure functions over plain structs (`Season`, `Round`, `Bet`), no Discord or database, with most of the unit tests. |
| `store.rs` | The migrations and every SQL query, as synchronous `rusqlite` functions. `load_season` reads a season into the `ledger` structs. |
| `ui.rs` | The custom IDs (the `Action` enum), the status buttons, the pick menus, the bet slip, the modals, the Help text and the name check. Builds messages from plain values; never calls Discord. |
| `render.rs` | The round as a picture: `round_svg` writes SVG text, `round_png` turns it into a PNG with the shared helpers in `src/util/svg.rs`. |
| `names.rs` | Display names for the stored user IDs, with a cache. |
| `actions.rs` | The handlers for buttons, menus and modals, and `status_message`, which builds a status picture with its buttons. |
| `commands.rs` | The four slash commands, written with `poise`. |
| `tools.rs` | The chat tool `get_wheel_status`. |
| `import.rs` | voltgpt's running game, read from its `game_state` JSON. |
| `sim.rs` | Simulated games that compare the payout rules. Test-only (`#[cfg(test)]`, an ignored test). |
| `mod.rs` | The `Wheel` struct and its `Feature` implementation. |

## How it works

### The game

A **season** has a list of options (user IDs) and numbered rounds. The active season is the one with no end time; there is at most one per server. A **round** has claims, bets and, once resolved, a winner. Setting a winner starts the next round. An option leaves the wheel once it has won, so the options left in a round are the season's options minus earlier winners (`options_left`). **Players** are everyone who claimed or bet in the season; there is no separate players table.

Options are stored per season, not per round. Adding or removing one with `/wheel_add` therefore also changes `options_left` of earlier rounds, which under classic rules changes what their winning bets paid.

### The money rules (`ledger.rs`)

Balances are never stored. `ledger(season)` folds over the rounds once (a "fold" walks the list while carrying a running total) and returns a `RoundLedger` per round: each player's `Standing` (money, bet, bet percentage, tax, payout), the totals bet on each option, and the pot. The status picture, the bet checks and the chat tool all read from it.

All division is integer division, which truncates (rounds toward zero), as in Go. The constants are `CLAIM = 100`, `TAX_THRESHOLD = 10` and `BET_CAP = 50`.

- **Money**: a player's money in a round is what they had after the previous round, plus 100 if they claimed this round. Claiming works once per player per round.
- **Bet percentage**: `bet * 100 / money` (0 when money is 0), where `bet` is the total of the player's bets this round.
- **Tax**: if the bet percentage is under 10, the tax is `money * 3 * (10 - percent) / 100`. That is 3% of the money per missing percentage point, so up to 30% for no bet.
- **How many options**: a player may bet on at most half of the options left, rounded up (`max_bets`).
- **How much**: `usable` is what a player can still bet this round. Classic: their money minus what they already bet. Pool: `(money * 50 + 99) / 100` (50% rounded up) minus what they already bet. Changing an existing bet frees its old amount first. A bet must be more than 0.
- **Typed amounts**: `parse_amount` reads `50` or `25%`. A percentage is of the round's money, rounded up (`(money * percent + 99) / 100`), so `10%` always avoids the tax.
- **Every round before the latest counts as over**, as in voltgpt: tax and payouts of earlier rounds are applied even if a round never got a winner.

Each season has **rules** (`Rules` in `ledger.rs`, stored as `classic` or `pool`):

- **Classic** (voltgpt's): a winning bet pays `amount * (options left - 1)` on top of keeping its stake; a losing bet loses its amount. Taxes simply disappear.
- **Pool**: every bet and every tax of the round goes into a pot, together with what earlier rounds carried over: `pot = carried + all bets + all taxes`. The bets on the winner share the pot by stake: a winning bet gets back `pot * amount / total bet on the winner` (integer division, in `i128` so it can't overflow), so its payout is that share minus its amount. A losing bet loses its amount. What is not paid out stays in the pot for the next round: the rounding rest, or the whole pot if nobody bet on the winner. (A past round with no winner, which only voltgpt's games have, gives the bets back and keeps the taxes in the pot.)

`odds(option)` is the pool payout per 1 bet if the round ended now: `pot / total on that option`. New seasons always use pool rules (`store::start_season`). Seasons from before pool betting, including voltgpt's import, stay classic.

**Why pool and the cap** (Cherry, 2026-10-09): with classic payouts, early rounds paid ×10 and more, and a lost claim cost nothing, so everyone bet everything every round. A simulation of 12-player games in `sim.rs` showed half of the all-in players ending broke without the 50% cap and none with it. Run it with `cargo test --release wheel::sim -- --ignored --nocapture`.

`check_bet` applies all of this to a bet in the latest round and returns the amount or a message for the player. `outcomes` lists a resolved round's results line by line (won, lost, taxed, with before and after). `can_undo` says whether a winner can be undone.

### One action, one transaction

Every handler in `actions.rs` and `commands.rs` loads the season, checks the action and writes the change inside one database transaction, and only talks to Discord after it committed. Nothing about the game is kept in memory between actions, so there is no global game lock. Buttons carry the round's database ID, and `open_round` refuses any button whose round is no longer the open, latest round of the active season ("Only the current round can be changed from this message."). So a button on an old message can never change a newer round. That error comes with a **View Current Round** button (`wheel:refresh`), which shows the current round on the status message the press was about.

### Playing a round

- **Claim!** adds a claim and redraws the status message. A private follow-up says how much the player has this round and the smallest bet that avoids the tax.
- **Place Bet!** needs money above 0 (otherwise "Press Claim!"). It opens a private menu of the options left, sorted by name. Discord fits 25 options in a menu, so a bigger wheel gets up to five menus (one per row), each placeholder naming the first and last name it holds, like "(Alice … Dana)"; past 125 the message says how many didn't fit. Remove Bet! and Set Winner! work the same. Picking one opens the **bet slip**: a private message with the option's current payout and pot (pool) or multiplier (classic), the player's money, what they already bet and can still bet, their current bet on this option and what it would return now, and a tax hint. It has buttons for 10%, 25% and 50% of the round's money (classic: also 100%), each showing its amount. A button is disabled when that bet isn't allowed, and the button matching the current bet is green and disabled. Each click places the bet, redraws the slip and redraws the public status message. "Other amount…" opens a modal (a plain serenity `CreateModal`, since it opens from a button) whose label shows the limit. Betting on the same option again changes that bet (`INSERT ... ON CONFLICT DO UPDATE`).
- **Remove Bet!** opens a private menu of your bets in this round; picking one deletes it.
- **Set Winner!** (admins) opens a menu of the options left. Picking one sets the winner and starts the next round. Nobody has to have bet. The public status message then shows the resolved round, and `BotEvent::WheelRoundResolved` is published.
- **Undo Winner** (admins) is shown on a resolved round while it is the round before the latest and nobody has bet in the latest round yet. It deletes the latest round (with its claims) and reopens the resolved one. The check runs again on press.
- **Change Name** opens a modal for the name the wheel shows for you: up to 32 characters, spaces collapsed, no `@`, `<`, `>`, backticks or control characters. An empty name goes back to your Discord name. The status message is redrawn.
- **Help** sends the season's rules privately (`ui::help_text`). The chat tool includes the same text.

When a private menu changes the game, the public status message is edited by its ID, which the menu's custom ID carries. If that edit fails (say the message was deleted), it is only logged; the change is saved either way.

### New seasons

`/reset_wheel` replies privately with a "Start a new season" button. The button's ID remembers which season was active when the command ran (0 for none). Pressing it opens a modal where the admin types `START` (any case). On submit, in one transaction, the bot checks that the active season is still the one the button was made for, ends it, and starts the next one with round 1, keeping the options if `keep_options` was set. So a double submit, or an old confirmation, can't end the season that was just started. A unique index also stops a server from ever having two active seasons.

### Names

The database stores only user IDs. `names::lookup` turns them into names when something is shown: the name the player chose with Change Name (`wheel_names`) wins, then the server nickname, global name or username, and for someone who left the server their user name or `Unknown (<id>)`. Names are cached in memory per server. A cached name older than 10 minutes is still shown, and looked up again in the background for next time; only users never seen before are waited for. This matters because Discord waits only 3 seconds for an interaction answer. On `start`, the feature looks up the names of every active game's players ahead of time.

### The status picture (`render.rs`)

The status is a PNG, not an embed. `round_svg` lays the round out as SVG and `resvg` draws it, through `src/util/svg.rs`, which is shared with the control panel. The Inter font is built into the binary; system fonts are the fallback for characters Inter lacks, such as emoji in names. Drawing runs on tokio's blocking threads (`spawn_blocking`) because it takes CPU time.

- **An open round** shows a header (round number, OPEN or ENDED, the season), a line with options left, how many you may bet on, and the payout (classic multiplier, or the pot and what was carried over). Then the standings sorted by money, with each player's money, bet, bet percentage and the tax they would pay if the round ended now, the bets grouped by option with each option's total and current pool payout, a "Waiting on" box with players who haven't claimed or have claimed but not bet, and the earlier winners.
- **A resolved round** shows the winner and a results table: before, bets, tax, after and change for each player.

Resolved rounds are uploaded as `SPOILER_round-<n>.png`, so Discord blurs them and the winner stays hidden, as voltgpt's `||spoiler||` did. Open rounds are `round-<n>.png`.

## Chat tools

`get_wheel_status` (in `tools.rs`): read only. It takes an optional `round` number (default: the current one) and returns the active season of the asker's server as plain text: the round's state, the Help text for the season's rules, the pot, the options left and the bet limit, earlier winners, every player's money, bet and tax (marking the asker), the claims and bets, current pool payouts, and the outcome of a resolved round. It refuses in DMs.

## Feature hooks it uses

`Wheel` implements these `Feature` trait methods (see `src/core/feature.rs`):

| Method | What it does here |
|---|---|
| `name` | `"wheel"`: the config section, the custom ID prefix and the log label. |
| `commands` | `/wheel_status`, `/wheel_add`, `/insert_bet`, `/reset_wheel`. |
| `migrations` | `store::MIGRATIONS`. |
| `legacy_import` | The `wheel` part, run by `import::import`. |
| `tools`, `run_tool` | `get_wheel_status`. |
| `tool_status` | "🎡 Checking the wheel…" in chat's status line. |
| `start` | Loads the fonts and looks up the players' names in the background, so the first button press is fast. |
| `stats` | Control panel lines: Games (active seasons), Open bets (in open rounds of active seasons), Rounds played (resolved rounds of active seasons). |
| `on_component` | Every button and select menu (`actions::on_component`). |
| `on_modal` | The bet amount, name and new season modals (`actions::on_modal`). |

Admin checks happen inside each admin command and button with `ctx.is_admin`, not with a poise `check` (a failed check would answer with the "turned off" message).

It publishes `BotEvent::WheelRoundResolved { guild_id, round, winner }` when a winner is set; `round` is the round number, not its database ID. No feature listens to it yet.

## Custom IDs

Built and parsed only in `ui::Action`. The dispatcher strips the `wheel:` prefix before the handler sees the rest. `<round>` is a round's database ID, `<message>` the ID of the public status message to redraw, `<on>` the option's user ID, and flags are `0` or `1`.

| ID | What it is |
|---|---|
| `wheel:current` | View Current Round. |
| `wheel:claim:<round>` | Claim!. |
| `wheel:bet:<round>` | Place Bet!: opens the options menu. |
| `wheel:unbet:<round>` | Remove Bet!: opens the menu of your bets. |
| `wheel:winner:<round>` | Set Winner!: opens the winner menu. |
| `wheel:undo:<round>` | Undo Winner. |
| `wheel:pick:<place\|remove\|winner>:<round>:<message>:<part>` | The three select menus; the picked user ID is the selected value. `<part>` (0 to 4) tells apart the menus of a big wheel. Menus from before it had no `<part>` still work. |
| `wheel:refresh:<message>` | View Current Round under the "Only the current round" error. |
| `wheel:stake:<round>:<on>:<message>:<percent>` | A bet slip percentage button. |
| `wheel:other:<round>:<on>:<message>` | The bet slip's "Other amount…" button. |
| `wheel:amount:<round>:<on>:<message>` | The bet amount modal. |
| `wheel:reset:<keep options>:<season id>` | The `/reset_wheel` button (season 0 when none was active). |
| `wheel:newseason:<keep options>:<season id>` | The `START` confirmation modal. |
| `wheel:help:<season id>` | Help. |
| `wheel:rename:<round>` | Change Name; the round is the one the status message shows, to redraw it. |
| `wheel:name:<round>` | The name modal. |

An older `reset:` button without a season ID gets "The season changed since this confirmation was made".

## Tables

From `store::MIGRATIONS`. Every child table uses `ON DELETE CASCADE`.

**Migration 1**

- `wheel_seasons`: `id`, `guild_id`, `started_at`, `ended_at` (NULL while it is being played). The partial unique index `wheel_one_active_season` on `guild_id WHERE ended_at IS NULL` allows one active season per server.
- `wheel_options`: `season_id`, `user_id`. Primary key on both, so nobody is on the wheel twice.
- `wheel_rounds`: `id`, `season_id`, `number` (1, 2, 3 ... per season, unique per season), `winner_id`, `resolved_at`.
- `wheel_claims`: `round_id`, `user_id`. The primary key allows one claim per player per round.
- `wheel_bets`: `round_id`, `by_id`, `on_id`, `amount` (`CHECK (amount > 0)`). The primary key allows one bet per player per option per round; placing it again updates the amount and keeps the row's place.

**Migration 2** adds `rules` to `wheel_seasons` (`classic` or `pool`, default `classic`, so older seasons kept voltgpt's payouts).

**Migration 3** adds `wheel_names` (`guild_id`, `user_id`, `name`): the names players chose with Change Name, per server.

**Migration 4** adds `wheel_round_ids`, a single row holding the highest round ID ever used. Undo Winner deletes the newest round, and SQLite would hand its ID to the next round, so a button made for the deleted round would work on its replacement. `store::add_round` takes new IDs from this counter instead.

## Background loops

None. `start` only runs a one-off name lookup and font load.

## Importing from voltgpt

voltgpt keeps the whole game as one JSON value in `game_state (id = 1, data)`, with full Discord user objects for every player, option, claim and bet. `import.rs` reads that JSON and keeps only the user IDs. Go writes empty lists as `null`, so every field reads `null` as empty.

voltgpt has one game for the whole bot, so it goes to the server in `main_server` in `config.toml`. Without `main_server` the import fails with a message saying so; `old.db` is kept and the next start tries again. The game becomes that server's active season with classic rules: its options, then each round's claims and bets, and each winner (which starts the next round, as it did in voltgpt). If the server already has a game, that game stays active and the imported one is filed as an ended season. Bets of 0 or less and IDs that are not numbers are logged and skipped. voltgpt's `players` list is not imported, because players are derived from claims and bets. A test (`same_balances_as_voltgpt`) checks that the imported season gives the same balances the Go bot showed.

The old.db import code is planned for removal once the imports are confirmed: `src/core/legacy.rs`, `Feature::legacy_import`, each feature's `import.rs`, the call in `main.rs`, and the `old_database` config key. The `legacy_imports` table stays. See [architecture](../architecture.md) for how the import runs.

## Compared to voltgpt

- **Tables instead of a JSON blob.** voltgpt saved the whole game as one JSON value and rewrote it after every change. Seasons, Undo Winner, `/insert_bet` and the chat tool all need to find or change one claim or bet, and the keys enforce rules for free (one claim per round, one bet per option).
- **One ledger function.** voltgpt recomputed a player's money from round 0 for every row of its status embed. Here `ledger` runs once and everything reads from it. It is pure, so it is tested without Discord or a database.
- **User IDs, not user objects.** voltgpt saved whole Discord users, so names and avatars went stale. Names are looked up when shown.
- **Undo Winner.** In voltgpt a mis-clicked winner was permanent.
- **Winner without bets.** voltgpt refused to set a winner when nobody bet ("No bets!"), which blocked the wheel if a movie was watched without bets.
- **Seasons instead of a hard reset.** voltgpt's `/reset_wheel` deleted everything without asking. Here it asks for `START` and keeps the old season viewable.
- **One game per server** instead of one global game.
- **No lock held during Discord calls.** voltgpt held `gamble.Mu` while calling the Discord API. Here each action is a short transaction and Discord comes after it.
- **New**: pool betting with the 50% bet cap, the bet slip, Change Name, Help, a picture instead of an embed, and the chat tool.
- **Small difference**: the player list is everyone who claimed or bet this season. voltgpt also listed people who only opened the bet menu.

## Ideas not built yet

- A `/wheel_spin` command that picks the winner at random with an animation.
- A balance history per player.
