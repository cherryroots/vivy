# Who said it?

A game that runs as a Discord Activity: a window inside Discord that everyone in a channel (or voice channel) can open together. Vivy picks messages from that channel's history, and everyone races to pick who wrote each one. Faster right answers score more. At the end, the scores are posted in the channel.

## Playing it

Open the Activity from the rocket button (the App Launcher) in a channel or voice channel and press **Start**. Everyone else who opens it in the same channel joins the same game. Each game has 10 questions with 20 seconds each, a few seconds to see the answer (with who picked what and a link to the message), and a final board with **Play again**.

**Who can play.** Until the app is verified by Discord, an Activity can only be opened by the members of its developer team and up to 50 invited app testers, and only in servers with fewer than 25 members ([Discord's article](https://support-dev.discord.com/hc/en-us/articles/26576097154199)). Verified Activities can be played by anyone in any server.

## Where the quotes come from

Only from the channel the Activity runs in. Everyone in the game can already scroll up to those messages, so it never shows anything someone couldn't see. Vivy never trusts the page to say which channel that is: she asks Discord, with the bot's token, which channel the Activity instance is in and who is in it.

Each game reads 6 random moments of the last `lookback_days` (100 messages each, nothing from the last hour), and keeps messages that make a good quote:

- written by a person (not a bot, a webhook or a system message)
- between `min_length` and `max_length` characters and at least 3 words
- without links (they're rarely funny without the picture)
- not by anyone in `exclude_users`

Mentions, channels, custom emoji and times are turned into what Discord shows (`@Cherry`, `#channel`, `:wave:`, "(a time)"). Nobody is the answer more than twice while someone else could be. The other choices are other people who wrote quotes, shown with their server nickname and avatar.

Nothing is saved: games live in memory and are gone when everyone has left for 2 minutes or the bot restarts.

## Setting it up

The Activity is a web page. Discord shows it in an iframe on `https://<application id>.discordsays.com` and sends its requests through its proxy to the addresses in the URL mappings. Vivy serves the page and the game on `listen` (default `127.0.0.1:8787`), and a web server in front of it gives it a public HTTPS address.

1. **Developer Portal → your application → Activities → Settings:** turn on **Enable Activities**. This adds a **Launch** command that opens the Activity. Vivy keeps it when she registers her slash commands (`register_commands` in `main.rs`): Discord refuses a command list that leaves it out.
2. **Installation:** tick **Guild Install** (and **User Install** if you like).
3. **OAuth2:** copy the **Client Secret** into `.env` as `DISCORD_CLIENT_SECRET`. Add the redirect `https://127.0.0.1` (the SDK doesn't use it, but the portal wants one).
4. **Your web server:** forward a path (or a subdomain) to `listen`, WebSockets included. With nginx, for `https://kirei.moe/vivy/`:

   ```nginx
   location /vivy/ {
       proxy_pass http://127.0.0.1:8787/;
       proxy_http_version 1.1;
       proxy_set_header Upgrade $http_upgrade;
       proxy_set_header Connection "upgrade";
       proxy_read_timeout 1h;
   }
   ```

   With Caddy: `handle_path /vivy/* { reverse_proxy 127.0.0.1:8787 }`. Opening `https://kirei.moe/vivy/` in a browser should show "Open this from Discord".
5. **Activities → URL Mappings:** add two.

   | Prefix | Target |
   |---|---|
   | `/cdn` | `cdn.discordapp.com` |
   | `/` | `kirei.moe/vivy` |

   The first lets the page load avatars (the page can only reach its own proxy). Without it the avatars show as first letters. If the portal won't take a path in a target, use a subdomain (`vivy.kirei.moe` pointing at `listen`) instead.
6. **App testers** (while unverified): **App Testers** in the portal, then each tester accepts the email and turns on **Application Test Mode** (Settings → Advanced) with the application ID.
7. Restart Vivy. The log says `Who said it? is served on http://127.0.0.1:8787`.

## Settings

Under `[features.who_said_it]`, each explained in `config.example.toml`: `listen`, `rounds` (10), `answer_seconds` (20), `lookback_days` (365), `min_length` (20), `max_length` (300), `exclude_users`, `post_results` (true). The gate keys work too: `channels = [...]` limits where it can be played. A channel it's turned off in gets "Who said it? is turned off in this channel."

## Files

All under `src/features/who_said_it/`.

| File | What it holds |
|---|---|
| `game.rs` | The rules, with the time passed in so the tests can play a whole game: the phases (lobby, loading, question, reveal, done), picks, points (100 for a right answer plus up to 100 for speed), the view sent to the page, and `make_rounds`. Pure. |
| `quotes.rs` | Reading the channel at random moments, `quote_text` (which messages make a quote), `clean` (Discord markup to text), and nicknames for the people shown. |
| `room.rs` | One task per Activity instance. Players' connections send it inputs (join, leave, start, pick) through a channel; it sends back the newest view through a `watch` channel. It closes 2 minutes after the last player leaves. |
| `web.rs` | The web server (`axum`): the page, `/api/token` (the login's code swapped for an access token with the client secret), and `/ws`, one WebSocket per player, which checks who they are and where with Discord before letting them in. |
| `mod.rs` | The `WhoSaidIt` feature: settings, and starting the web server when `DISCORD_CLIENT_SECRET` is set. |
| `page/` | `index.html`, `app.js` (logs in, then draws whatever view Vivy sends), `style.css` (Discord's dark colours with Vivy's teal), and `sdk.js`. |

### `page/sdk.js`

Discord's [Embedded App SDK](https://github.com/discord/embedded-app-sdk) (MIT) 2.5.0, bundled into one file so the bot needs no Node.js to build. To update it:

```bash
npm i @discord/embedded-app-sdk@<version> esbuild
echo 'export { DiscordSDK } from "@discord/embedded-app-sdk";' > entry.js
npx esbuild entry.js --bundle --format=esm --minify --legal-comments=eof --outfile=sdk.js
```

## How a player gets in

1. The page asks the SDK to log in (`authorize` with the `identify` scope). Discord gives it a one-time code.
2. The page posts the code to `/.proxy/api/token`; Vivy swaps it for an access token with the client secret, and the page hands the token to the SDK (`authenticate`).
3. The page opens `wss://…/.proxy/ws` and sends the token and the instance ID.
4. Vivy reads `users/@me` with the token (who it is), and the instance with the bot's token (`GET /applications/{id}/activity-instances/{instance}`: which channel, and who is in it). The player must be listed there (she asks again after 2 seconds, since Discord can be a moment behind), the channel must be in a server, and the feature must be allowed there.
5. The player joins the instance's room and gets every view from then on. Leaving keeps their score in case they come back.
