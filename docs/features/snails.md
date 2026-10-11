# Snails

Snails spots reposts. A "snail" is someone posting a link or a picture the server has already seen. The bot quietly saves the links and pictures of every new message in a server. Anyone can right-click a message and pick **Check Snail** to see where it was posted before. New messages that repeat an earlier post are also counted, without any message in the channel, for the [control panel](control-panel.md). Reposting your own post counts too.

## Using it

- **Check Snail** (right-click a message, then Apps): a private reply that lists up to 10 earlier posts of the same link or picture, oldest first, each with the date, the author and a jump link, and whether it was the same link or the same picture. If a picture could not be downloaded, the reply says so and that it will be retried.
- **`/snails`**: the server's top 10 snailers this week and of all time, from the snails counted as they were posted (not what the backfill found in older history). Names show as mentions that ping nobody.
- **`/snail_backfill start | pause | status`** (admins only): reads the server's older history so older posts count too. It never starts on its own. `start` also continues a paused crawl. `status` shows how far it got, the pictures waiting for a retry grouped by host, and the pictures dropped after their last try (like `` `pbs.twimg.com` (50x) ``).

The only setting is the usual gate in `[features.snails]` (`enabled`, plus the shared `guilds` / `channels` keys); `config.example.toml` explains them. Setup is in the README.

## Files

All in `src/features/snails/`. The first three are pure functions (no Discord, no database), with tests at the bottom of each file, so they are the easiest place to start.

| File | What it holds |
|---|---|
| `links.rs` | Turns a link into a **link key**, so mirrors and tracking junk share one key. Lists known sites, embed-fixer mirrors and short-link hosts. |
| `fingerprint.rs` | The picture **fingerprint** (81 hashes), the `distance` between two fingerprints, and the **detail check** that confirms a match. |
| `collect.rs` | Which links and pictures of a message count, and the small media-proxy URL each picture is fetched from. |
| `store/` | The migrations and every SQL query. Synchronous functions that take a `rusqlite` connection, all reached as `store::name`. `mod.rs` has the tables and saving a message; `matches.rs` finds older messages with the same links or pictures and counts snails; `failures.rs` is the retry list; `crawl.rs` is the backfill's bookkeeping. |
| `index.rs` | Downloading, fingerprinting and saving one message (`sync_message`, `index_message`). |
| `check.rs` | The Check Snail command, and the quiet snail count for new messages (`on_new_message`). |
| `board.rs` | `/snails`, the leaderboard (`store::top_snailers`). |
| `backfill.rs` | `/snail_backfill`, the history crawl, and the startup catch-up. |
| `retry.rs` | The loop that tries failed pictures again. |
| `mod.rs` | The `Snails` struct and its `Feature` methods, which tie the rest together. |

## How it works

### Link keys (`links.rs`)

The same post can be shared through many URLs: `x.com/a/status/1`, `fxtwitter.com/a/status/1/photo/1`, `twitter.com/b/status/1?s=20`. `link_key` turns each of them into the same short string, and two messages match when they share a key. Step by step:

1. Only `http` and `https` links count. The host is lowercased and a leading `www.` or `m.` is dropped.
2. `site_of_host` names the site the host belongs to. Embed fixers (sites that make better Discord previews, like `fxtwitter.com`, `vxtwitter.com`, `fixupx.com`, `fxbsky.app`, `ddinstagram.com`, `rxddit.com`, `phixiv.net` and more) count as the site they mirror. So do their one-letter subdomains from the `MIRRORS` list, like `d.fxtwitter.com`. Known sites: X/Twitter (also nitter and xcancel), Bluesky, Threads, Instagram, TikTok, YouTube, Reddit, Facebook, Pixiv, Tumblr, Twitch, Imgur, Streamable, Spotify, Pinterest, Discord, Discord's CDN, and GIF sites.
3. Sites in `IGNORED_SITES` never get a key: GIFs from Discord's picker (`klipy.com`, `tenor.com`, `giphy.com`) are reused all the time, and a link to another Discord message is a reference, not a repost.
4. For a known site, `post_id` pulls out the id of the post, so the key looks like `x:1844012345678901234` or `youtube:dQw4w9WgXcQ`. Some ids keep a little more: Bluesky keeps the handle (`bsky:<handle>/<post>`) because its post ids are only unique per account, and Twitch, Imgur and Spotify say what kind of thing it is (`twitch:clip/<slug>`, `imgur:album/<id>`, `spotify:track/<id>`). Files on Discord's CDN key on their path, so the same file from `cdn.discordapp.com` and `media.discordapp.net` matches.
5. Anything else (an unknown site, or a known site's link that isn't one post, like a profile) gets `host/path` plus its query parameters, sorted, minus tracking junk. `JUNK_PARAMS` (and anything starting with `utm_`) is dropped everywhere. `site_junk` drops more only on sites where it is known to be junk, like `s` and `t` on X or `si` and `t` on YouTube. Elsewhere a short name like `t` can matter (a forum's `?t=123` is a thread).

Short links hide the post id (`vm.tiktok.com`, `t.co`, `pin.it`, `reddit.com/r/<sub>/s/...`, and others in `needs_resolving`). `index::link_keys` follows them first with one HEAD request (10 second timeout), then keys the link it lands on. If that fails, the short link itself is keyed.

### Which pictures count (`collect.rs`)

`collect::pictures` lists a message's pictures, always in the same order. The index in that list is stored as the picture's `position`.

- Attachments that have a width and height: images, GIFs and videos. Files without a size (a text file) are skipped.
- Link previews (Discord calls them embeds) of type `image`, which is what a direct link to a picture becomes, and previews from sites in `PICTURE_SITES` (X, Bluesky, Threads, Instagram, Reddit, Pixiv, Tumblr, Imgur, Pinterest). People often repost just the picture from a tweet, so those previews are worth hashing. On a picture site, a preview with no big image is a text post; its thumbnail is the author's avatar, so it is not used.
- Skipped: `gifv` previews (GIFs from Discord's picker) and previews of videos, music and articles. Their link key is enough.

Each picture is fetched small. `small_url` asks Discord's media proxy (`media.discordapp.net`) for a WebP whose longest side is at most `FETCH_SIZE` (256 pixels), with the picture's own shape. For a video, the proxy returns its first frame, so videos need no ffmpeg here. If the proxy can't serve it, `index::load_picture` downloads the original file instead (up to 20 MB). All downloads go through `util/media.rs`, which refuses local and private addresses, also after the DNS lookup and at every redirect. Decoding refuses pictures over 8192 pixels a side.

`Picture::source` is the original file's host and path, without the query. Discord signs its CDN links with a query that expires and changes on every read, so the source is what stays the same. It is used to recognise a picture that was already saved, and to name a failed one. `Picture::host` (like `pbs.twimg.com`) groups failures by who serves them.

### Fingerprints (`fingerprint.rs`)

A plain picture hash fails on crops: cut 10% off one side and the hash looks like a different picture. The approach here was picked with a benchmark (see the comment at the top of the file, which mentions `image-match-bench/` in the project files). It has two stages.

**1. Fingerprint.** `fingerprint` does this to each picture:

- `prepare` turns it grayscale, cuts away flat borders, and shrinks it so the longest side is at most `WORK_SIZE` (256). `trim` removes rows and columns whose pixels all sit within 12 shades of the top-left pixel (letterboxing, bars around a screenshot). If that would leave less than a third of a side, the picture is left as it is.
- It takes 81 regions of the picture. `AXIS` lists 9 positions per side: the whole side, 92% of it at 3 offsets, and 84% of it at 5 offsets. Crossing the two sides gives 9 × 9 = 81 regions (`HASHES`), with the whole picture first.
- Each region gets a 16×16 **difference hash** (dHash) from the `image_hasher` crate: the region is shrunk to a tiny grayscale grid, and each of the 256 bits says whether a pixel is brighter than its neighbour. That is 32 bytes (`HASH_BYTES`) per region.

The fingerprint is those 81 hashes in a row (2,592 bytes) plus the aspect ratio (width / height) after trimming.

`distance(a, b)` compares the **whole** of one picture with the whole **or any crop** of the other, in both directions. It counts the differing bits (the Hamming distance) and returns the smallest count. Pairs are only compared when their regions have about the same shape: the aspect ratios may differ by about 6% (`SHAPE_TOLERANCE`). Two pictures are a candidate match at `MATCH_DISTANCE` (41) differing bits or fewer, out of 256.

**2. Detail check.** Comparing against 81 crops gives look-alikes many chances to match: a meme template with a new caption is close almost everywhere. So `detail_difference` confirms each candidate on the actual pixels:

- Both pictures are prepared again. Of all 81 regions of each, it finds the region of one picture that best fits the whole of the other, at 64×64 pixels (`DETAIL_SIZE`). Each square is normalised to mean 0 and standard deviation 1, so brightness and contrast changes don't count.
- `refine` nudges that region's four edges by small steps (0.4%, 1%, 2%) for up to 3 rounds while the fit keeps improving.
- `worst_block` splits both squares into 8×8 blocks (`BLOCKS`) and returns the average difference of the most different block. Recompression changes every block a little (about 0.0 to 0.3). A new caption changes a few blocks a lot (0.5 and up).

The match is confirmed when that is at most `DETAIL_LIMIT` (0.5).

Hashing and the detail check are CPU work, so they run in `tokio::task::spawn_blocking`, which moves the work to a separate pool of threads so the async tasks that answer Discord keep running. The region list and the hasher are `LazyLock` statics: values built once, the first time they are used.

### Finding earlier posts (`check.rs`, `store/matches.rs`)

`check::earlier_posts` does the search for both Check Snail and the quiet count. "Earlier" means a smaller message ID: Discord IDs grow with time.

1. **Same link:** `store::same_links` finds older messages in the server with one of the keys.
2. **Same picture:** `store::pictures_after` reads the server's stored fingerprints, `SCAN_CHUNK` (5,000) rows per database call, so other database work runs in between. `store::close_to` then computes `distance` for each row on a blocking thread (`spawn_blocking`), so the database thread only reads. The candidates are sorted closest first, one per stored picture, and at most `MAX_CANDIDATES` (12) go on to the detail check.
3. The database stores fingerprints, not pixels. So for the detail check, `same_picture` reads the earlier message from Discord again, picks its picture at the stored `position`, and downloads it.

Check Snail downloads and fingerprints the checked message's pictures fresh, saves the message if it was never indexed (`save_if_new`), and lists the earlier posts it finds. Each one costs a Discord request (and a picture also a download), so it stops once `MAX_LISTED` (10) are confirmed; the earlier posts of the same link left over are counted as "…and N more post(s) of the same link, not checked one by one", and close pictures aren't looked for then. The quiet count only needs one, so it stops at the first hit.

Every earlier message is read from Discord before it is listed (`still_there`). One that was deleted (404) is forgotten on the spot. One the bot can't read right now (another 4xx, like a channel it lost access to) is skipped but kept.

### New messages, edits and deletes (`mod.rs`, `index.rs`)

- **New message** (`on_message`): if it has links, attachments or previews, `check::on_new_message` saves it and checks only what is new in it. Discord often adds link previews a few seconds late, so a message with fewer previews than links that can get one (`media::preview_links`: not `<link>` ones) is read again after `PREVIEW_RECHECK` (12 seconds) and goes through the same function. The recheck is skipped when an edit event read the message first (the `waiting` set on the `Snails` struct), since that read is newer.
- **Edit** (`on_message_edit`): Discord also sends late link previews as edits. If the edit touches the text, the previews or the attachments, the message is read again. Added links and pictures are saved and checked; removed ones are dropped. A message with nothing to index now that had nothing before is skipped without reading it.
- **Delete** (`on_messages_deleted`): `store::forget` deletes the message's links, pictures and failed pictures. A snail that gets deleted stays counted as caught: it was posted.

`index::sync_message` does the saving. It makes the stored rows match the message as it is now. A picture whose `source` is already saved keeps its fingerprint and is not downloaded again. A picture that is already waiting on the retry list is left to the retry loop. `store::save` replaces the message's rows in one transaction (a group of writes that all happen or none do). `sync_message` returns only the new links and pictures, so running it twice on the same message is harmless.

`check::count_if_snail` runs `earlier_posts` with `first_only` (it stops at the first hit) and writes the message to `snail_caught`. A message is counted at most once, however often it changes.

### Failed downloads (`retry.rs`)

A picture that fails to download goes into `snail_failures`; the rest of the message is saved as usual. `store::retry_delay` sets the wait before the next try: 10 minutes after the first failure, then 20. `MAX_ATTEMPTS` is 3 tries in all, counting the first one.

Every 5 minutes, `retry_due`:

1. Drops pictures that failed their last try (`drop_given_up`) and adds them to the count per server and host in `snail_dropped`.
2. Takes up to 25 (`BATCH`) failures that are due, grouped so each message is read once. Discord's attachment links expire, so the message is read again for fresh links. A deleted message (404) is forgotten. Any other error reading it counts as a failed try.
3. Downloads each failed picture again. A picture that an edit removed is taken off the list. One that works is saved (`retry_succeeded`, which first checks the failure is still listed, so a message deleted meanwhile stays deleted) and checked for snails like a new picture (`on_retried_picture`). One that fails again waits longer (`retry_failed`).

If snails was turned off in that channel since, the failure is pushed back an hour (`OFF_WAIT`) without counting a try, so it doesn't fill every batch.

### The backfill (`backfill.rs`)

`/snail_backfill start` lists every channel with messages: text, news, voice and stage chats, active threads, and public and private archived threads (forum posts are threads). Private archived threads need the Manage Threads permission; without it they are skipped. The archived-thread pages are requested by hand (`archived_threads`) because serenity's helper sends the paging timestamp in a form Discord doesn't accept. Each channel gets a row in `snail_crawl`, and the server is marked running in `snail_backfills`.

`crawl` then reads one channel at a time (lowest channel ID first), 100 messages per page (`PAGE`), from the newest back. `read_page` indexes up to 8 messages at once (`PARALLEL`) with `index::index_message`, which skips messages read before and does **not** count snails: old reposts are history, not new catches. After every page it saves its place (`before_id`), so pausing, a restart or a network failure loses nothing. A channel the bot can't read is marked finished with its error. Any other failure waits 60 seconds (`RETRY_AFTER`) and tries the page again.

A static `WORKERS` set (shared by the whole process, behind a `Mutex` lock) makes sure one server never has two crawls at once. A crawl that was running when the bot stopped carries on at the next start.

### Startup catch-up

On every start, `spawn_catch_up` reads what was posted while the bot was offline, but only in channels the backfill has read before (finished, or at least one page read, and no error). Channels never crawled are left alone until an admin starts a backfill. `store::catch_up_channels` picks where to start in each channel: the newest of `snail_crawl.newest_id` (the newest message the backfill or the catch-up read there) and the newest saved message in that channel. `catch_up_channel` then reads forward, oldest first, so a repost inside the missed stretch still finds its original. Missed messages go through `on_new_message`, so they are checked and counted like new ones.

## Feature hooks it uses

| Method | What it does here |
|---|---|
| `name` | `"snails"` |
| `commands` | Check Snail (`check::check_snail`), `/snail_backfill` (`backfill::snail_backfill`) and `/snails` (`board::snails`). |
| `migrations` | `store::MIGRATIONS` |
| `start` | Resumes crawls that were running (in servers the gate allows), starts the catch-up, and starts the retry loop. Never starts a new crawl. |
| `on_message` | Saves and checks a new message, then reads it again after 12 seconds for late previews, unless every link already has one or an edit was read first. |
| `on_message_edit` | Reads an edited message again and syncs its rows. |
| `on_messages_deleted` | Forgets the deleted messages. |
| `stats` | For the control panel: Caught this week (all servers), Links, Pictures, and Backfill (`<finished>/<total> channels`, only once a backfill exists). Links, Pictures and Backfill are counted again at most every 15 minutes (`TOTALS_FOR`), since counting reads both whole tables. |

The dispatcher already drops messages from bots and checks the feature's gate before calling these. See [architecture](../architecture.md).

## Tables

All created by `store::MIGRATIONS`.

**Migration 1**

- `snail_messages`: one row per indexed message that has links or pictures: message, server, channel, author. The other tables point at it, and deleting a row here deletes its links and pictures (`ON DELETE CASCADE`).
- `snail_links`: server, link key, message. Primary key is all three.
- `snail_pictures`: message, `position`, server, `aspect`, and `hashes` (the 81 hashes as one blob). Indexed by server for the picture scan.
- `snail_crawl`: one row per channel or thread in a backfill: server, channel name, `before_id` (where the next page starts), finished flag, messages read, pictures hashed, and an error if it couldn't be read.
- `snail_backfills`: server and whether its crawl is running.

**Migration 2**

- `snail_caught`: new messages that turned out to be snails: message, server, author, `caught_at` (unix seconds). Feeds "Caught this week" and `/snails`.

**Migration 3** (edits, deletes, failed downloads, catch-up)

- Adds `snail_pictures.source` (host and path of the original), so an edit keeps fingerprints of pictures that didn't change. Pictures saved before this column existed are matched by `position` instead.
- Adds `snail_crawl.newest_id`, the newest message read in the channel, for the catch-up.
- Indexes on `snail_links (message_id)` and `snail_messages (channel_id, message_id)`.
- `snail_failures`: pictures waiting for a retry: message, `source`, `position`, server, channel, author, host, last error, first failed, last tried, next try, attempts. Not tied to `snail_messages`, because a message whose only picture failed has no row there.

**Migration 4**

- `snail_dropped`: server, host, and how many pictures from that host were dropped after their last try.

**Migration 5** adds the index `snail_caught_guild` on `(guild_id, caught_at)` for `/snails`.

## Background loops

| Timer name | Every | What |
|---|---|---|
| `Snail picture retries` | `every 5m` | `retry::spawn`: drops given-up pictures, retries due ones. |

The backfill crawl, the catch-up and the 12-second recheck are background tasks too, but they are not loops on a timer, so they don't show on the status picture.

## Compared to voltgpt

voltgpt's `internal/hasher/hasher.go` hashed only attachments and media links. It took one 16×16 **average** hash (`goimagehash.ExtAverageHash`) of each whole picture, and for videos it ran ffmpeg on the video to grab frame 10. Hashes lived in a global in-memory map, written to `image_hashes` as hash → the message as JSON (one message per hash), and were not split by server. Its Check Snail listed stored hashes within 8 bits. `/hash_server` (with optional channel, threads and date options) hashed history.

What changed: link keys (voltgpt had none), crop-tolerant fingerprints plus the detail check, video stills from Discord's media proxy instead of ffmpeg, data kept per server in SQLite rows instead of a JSON map, edits, deletes, failed-download retries, a quiet snail count, and a resumable backfill. voltgpt's `image_hashes` is not imported: its hashes can't be compared with these fingerprints.
