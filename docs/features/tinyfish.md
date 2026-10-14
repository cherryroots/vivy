# TinyFish

Vivy can search the web and read whole pages with [TinyFish](https://www.tinyfish.ai)'s Search and Fetch APIs. They're chat tools, so they work the same with Claude, OpenAI and Gemini, next to each provider's own built-in web tools. The point is reach: Claude's web search and fetch can't get into Reddit, and TinyFish can (its fetch opens pages in a real browser when they need one).

Both are free per key: 12,000 searches a day (30 a minute) and 1,000 fetched pages a day (150 a minute), reset at 00:00 UTC. Past that they need a TinyFish wallet balance.

## Using it

There are no commands. In chat, ask Vivy to look something up or read a link; the chat model decides when to use `tinyfish_search` and `tinyfish_fetch`. While they run, the status line under her answer says "🐟 Searching the web…" or "🐟 Reading the page…".

Her system prompt (`src/features/chat/prompt.md`) tells her when to pick them over her built-in web tools: TinyFish for anything on Reddit, and `tinyfish_fetch` again when her own fetch can't read a page. Otherwise she chooses from the tool descriptions.

The tools are only offered when `TINYFISH_API_KEY` is set in `.env` (make one at <https://agent.tinyfish.ai/api-keys>). Both are on the chime-in allow-list in `src/features/chat/chime.rs`, since they only read the web.

Settings live under `[features.tinyfish]`: `max_results` (1 to 10, default 8), `location` and `language` for searches where she doesn't name them, and `max_page_characters` (default 15000) for how much of each page she reads. Like every feature, `enabled`, `guilds`, `deny_guilds` and the channel keys turn it off or limit it. `config.example.toml` explains each one.

## Files

All under `src/features/tinyfish/`.

| File | What it holds |
|---|---|
| `mod.rs` | `Settings`, the `TinyFish` feature (tool list, status lines, `run_tool` sending each call to its file), and `read_answer`, which sends a request and turns an error answer into a message for the model |
| `search.rs` | `tinyfish_search`: the tool definition, `run`, and two pure, tested helpers: `search_query` (arguments to query string) and `format_results` (the answer as a numbered list) |
| `fetch.rs` | `tinyfish_fetch`: the tool definition, `run`, and two pure, tested helpers: `fetch_body` (arguments to request body) and `format_pages` (the answer as text) |

## Search

1. She calls `tinyfish_search` with a `query`, and optionally `purpose` (why she's searching; TinyFish uses it to rank), `kind` (`web`, `news` or `research_paper`, sent as `domain_type`), `recency_minutes`, `include_domains` (like `reddit.com`), `exclude_domains`, `location`, `language` and `page` (0 to 10).
2. `GET https://api.search.tinyfish.ai?query=...` with the key in the `X-API-Key` header, waiting at most 15 seconds (it usually answers in 1 to 3).
3. The answer is `{"query", "results": [...], "total_results", "page"}`. Each result has `position`, `site_name`, `title`, `snippet` and `url`, plus `date` and `publisher` for news and `authors`, `venue`, `year` and `citation_count` for papers.
4. `format_results` keeps the first `max_results` and writes each as:

   ```
   1. Title (site, publisher, date)
   https://link
   The snippet, cut at 400 characters
   ```

## Fetch

1. She calls `tinyfish_fetch` with `urls` (1 to 3 links; a single string works too) and optionally `purpose` (what she's looking for).
2. `POST https://api.fetch.tinyfish.ai` with `{"urls": [...], "format": "markdown", "purpose": ...}`, waiting at most 150 seconds. Most pages take 1 to 20 seconds; Reddit takes about 20, since TinyFish has to get past its JavaScript check. TinyFish gives up on one page after 110 seconds.
3. The answer is `{"results": [...], "errors": [...]}`. A result has `url`, `final_url` (after redirects), `title`, `published_date` and `text` (the page as markdown). An error has `url`, `error` (a code like `page_not_found`, `bot_blocked`, `login_required`, `timeout`) and sometimes the site's HTTP `status`. One page failing doesn't fail the others.
4. `format_pages` writes each page as `# Title`, its link, its date, and its text cut at `max_page_characters` (with a note when cut), then a `Couldn't read <url>: <code>` line for each failure.

## Errors

An error answer from either API goes back to her as the tool's result ("TinyFish answered 429 Too Many Requests: ..."), so she can fix a bad argument or answer without it. 402 means the day's free requests are used up and the wallet is empty; 429 means too many a minute.

Nothing is stored and there are no stats: TinyFish's dashboard shows usage.

## Not built

- Fetch's `include_selectors` (only part of a page) and `highlights` (passages answering a question; beta, needs TinyFish to turn it on for the account).
- The Agent and Browser APIs (TinyFish driving a browser to do things on a site) aren't used.
