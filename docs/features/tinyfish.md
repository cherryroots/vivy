# TinyFish

Vivy can search the web with [TinyFish](https://www.tinyfish.ai)'s Search API. It's a chat tool, so it works the same with Claude, OpenAI and Gemini, next to each provider's own built-in web search. TinyFish search is free up to 12,000 requests a day per key (30 a minute); past that it needs a TinyFish wallet balance.

## Using it

There are no commands. In chat, ask Vivy to look something up; the chat model decides when to use the `tinyfish_search` tool. While it runs, the status line under her answer says "🐟 Searching the web…".

The tool is only offered when `TINYFISH_API_KEY` is set in `.env` (make one at <https://agent.tinyfish.ai/api-keys>). It's also on the chime-in allow-list in `src/features/chat/chime.rs`, since it only reads the web.

Settings live under `[features.tinyfish]`: `max_results` (1 to 10, default 8), and `location` and `language` for when she doesn't name them. Like every feature, `enabled`, `guilds`, `deny_guilds` and the channel keys turn it off or limit it. `config.example.toml` explains each one.

## Files

All in `src/features/tinyfish/mod.rs`:

| Item | What it does |
|---|---|
| `Settings` | The `[features.tinyfish]` keys |
| `TinyFish` | The feature: the tool definition, its status line, and `run_tool`, which sends the request |
| `search_query` | Turns the tool's arguments into the request's query string, filling in `location` and `language` from the settings. Pure, so it's tested on its own. |
| `format_results` | Turns TinyFish's JSON answer into a numbered list for the model. Pure and tested too. |

## How it works

1. She calls `tinyfish_search` with a `query`, and optionally `purpose` (why she's searching; TinyFish uses it to rank), `kind` (`web`, `news` or `research_paper`, sent as `domain_type`), `recency_minutes`, `include_domains`, `exclude_domains`, `location`, `language` and `page` (0 to 10).
2. `run_tool` sends `GET https://api.search.tinyfish.ai?query=...` with the key in the `X-API-Key` header, waiting at most 15 seconds (TinyFish suggests 10; it usually answers in 1 to 3).
3. The answer is `{"query", "results": [...], "total_results", "page"}`. Each result has `position`, `site_name`, `title`, `snippet` and `url`, plus `date` and `publisher` for news and `authors`, `venue`, `year` and `citation_count` for papers.
4. `format_results` keeps the first `max_results` and writes each as:

   ```
   1. Title (site, publisher, date)
   https://link
   The snippet, cut at 400 characters
   ```

5. An error answer goes back to her as the tool's result ("TinyFish answered 429 Too Many Requests: ..."), so she can fix a bad argument or answer without searching. 402 means the day's free searches are used up and the wallet is empty; 429 means more than 30 a minute.

Nothing is stored and there are no stats: TinyFish's dashboard shows usage.

## Not built

- TinyFish's Fetch API (`POST https://api.fetch.tinyfish.ai` with a list of URLs, returning each page as markdown) would give OpenAI a way to read whole pages; Claude and Gemini already fetch pages themselves. It would be a second tool in this file.
- The Agent and Browser APIs (TinyFish driving a real browser) aren't used.
