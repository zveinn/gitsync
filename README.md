# gitsync

A local-first TUI for the GitHub issues and pull requests you are actually
involved in.

This is a ground-up rewrite of [github-feed](https://github.com/zveinn/git-feed).
The old tool was a one-shot CLI: eight Search API queries, a JSON blob cache,
then a printed dump. `gitsync` keeps a real SQLite model, opens instantly from
disk, and talks to GitHub only for what changed.

## Why rebuild

The original program is trying to answer one question well:

> What PRs and issues am I involved in — as author, assignee, reviewer,
> review-requested, commenter, or mention — and what is their current state?

That goal is right. The implementation fought GitHub's worst quota (Search, 30
requests/minute) every run:

| Old `github-feed` | `gitsync` |
|---|---|
| 8 Search queries on every invocation | 3 GraphQL searches **once**, then notifications |
| Incremental window is date-granularity (`updated:>=YYYY-MM-DD`) | `If-None-Match` / `since=` on notifications (304 is free) |
| Search `Issue` objects used as PRs — **merged is lost** | GraphQL `state` / `mergedAt` stored as `open` / `closed` / `merged` |
| One involvement label, last-writer-wins with a priority table | Every role is stored; the list shows the strongest |
| Cross-refs scrape review comments (extra REST pages) | `closingIssuesReferences` + body parse, no comment crawl |
| Stale "still open" items each get their own REST `GET` | Open items refreshed in GraphQL batches of 20 |
| BBolt of raw `go-github` JSON | SQLite with indexes, roles, labels, reviews, links |
| Print-and-exit CLI | Persistent TUI: inbox, reviews, detail, unread, live sync |

A live backfill for `zveinn` (90 days) used **5 search pages** and stored 113
distinct items with merge state, review decision, and 82 linked issues.

## Install

Tagged releases (`v*`) publish Linux, Windows, and macOS binaries on
[GitHub Releases](https://github.com/zveinn/gitsync/releases/latest).

```bash
cargo build --release
# binary: target/release/gitsync
```

Token (classic PAT or fine-grained):

- `repo` (private repos you care about)
- `notifications` (recommended — incremental poll)
- `read:user` is enough to resolve your login

`read:org` is **not** required. Team review-request slugs are skipped so a
personal token keeps working.

Set the token in any of:

1. `GITHUB_TOKEN` / `GH_TOKEN` / `GITHUB_ACTIVITY_TOKEN`
2. `~/.gitsync/token`
3. `token = "..."` in `~/.gitsync/config.toml`
4. The existing `~/.github-feed/.env` (read as a fallback, never overwritten)

Username is optional. GraphQL `viewer { login }` fills it in.

## Usage

```bash
gitsync                 # TUI (syncs in the background)
gitsync --offline       # TUI, local database only
gitsync sync            # one incremental poll, print a summary
gitsync sync --full     # force the 3-query backfill
gitsync stats           # local counts
```

First TUI launch (or `sync --full`) walks three GraphQL searches:

```
involves:YOU updated:>=DATE
is:pr reviewed-by:YOU updated:>=DATE
is:pr review-requested:YOU updated:>=DATE
```

`involves` already covers author / assignee / commenter / mention. The two PR
queries exist because GitHub's `involves` qualifier does **not** include
reviewers. After that, the TUI polls `/notifications` every ~90s (or whatever
`X-Poll-Interval` says). Unchanged polls return **304** and do not spend quota.
Changed subjects are hydrated in one GraphQL alias batch.

A 6-hour safety search plus a small refresh of stale open items stops merge/close
from rotting in the cache — the bug the old `--time` window had to special-case.

## TUI

```
 gitsync   zveinn                              30d · open   synced 12s
 Inbox 8   All PRs 12   My PRs 7   Closed PRs 40   All Issues 3   Closed Issues 18
╭ All PRs  1/12 ───────────────────────────────────────────────────────────╮
│   role        state    repository              title           update    │
│ ● Requested   Open     miniohq/aistor#6752     feat: add mem…      3m    │
│       closes  Open         miniohq/aistor#6671     memory analytics      │
╰──────────────────────────────────────────────────────────────────────────╯
```

| Key | Action |
|-----|--------|
| `h` `l` | Previous / next view |
| `j` `k` | Move in the list (or scroll the preview when it is focused) |
| `i` | Open / close the right preview (3/4 width) |
| `tab` | Toggle list / preview focus |
| `esc` | Close the preview |
| `/` | Filter title, repo, author, number |
| `r` | Sync menu: this item, created last 7/30/60/90 days, or all |
| `t` | Toggle linked items for the selected PR/issue |
| `T` | Toggle linked items for the whole list |
| `m` | Toggle local read/unread |
| `n` `N` | Next / previous unread |
| `y` | Copy the GitHub URL (OSC 52, works over SSH) |
| `o` / `enter` | Open in browser |
| `c` | Fetch comments for the selected item |
| `?` | Help |
| `q` | Quit |

**Inbox** is GitHub notifications plus open review requests and assignments waiting on you. Background polling still hydrates the matching PRs and issues. **All PRs** is open PRs you authored or were asked to review. **All Issues** nests PRs that close or mention each issue.

## Config

`~/.gitsync/config.toml` is created on first run:

```toml
api_url = "https://api.github.com"
allowed_repos = []          # empty = everything you are involved in
poll_seconds = 90
backfill_days = 90
participating_only = false  # true = only threads you participate in
```

Database: `~/.gitsync/gitsync.db` (SQLite, WAL).  
Logs: `~/.gitsync/gitsync.log`.

GitHub Enterprise: set `api_url` (or `GITHUB_API_URL`) to
`https://git.example.com/api/v3`. GraphQL is derived as
`https://git.example.com/api/graphql`.

## Data model

SQLite, not a key/value dump:

- `items` — one row per issue/PR, with state, draft, review decision, diffs
- `item_roles` — many roles per item
- `item_labels`, `item_assignees`, `reviews`, `comments`
- `links` — closes / mentions, from GraphQL first-class refs plus body parse
- `meta` — cursors (`notif_since`, `notif_etag`, `backfill_done`, …)

Comments are **not** fetched during list sync. Press `c` on an item.

## License

MIT
