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
 Inbox 15   Reviews 13   Mine 11   Pulls 18   Issues 1   All 19   Closed 43
╭ Inbox  1/15 ─────────────────────────────────────────────────────────────╮
│   role        state    repository              title              age    │
│ ● Requested   Open     miniohq/aistor#6752     feat: add mem…      3m    │
│   Authored    Open     miniohq/act#20          Fix colors…        11m    │
╰──────────────────────────────────────────────────────────────────────────╯
```

| Key | Action |
|-----|--------|
| `h` `l` | Previous / next view |
| `j` `k` | Move (list, or detail when focused) |
| `tab` | Toggle list / detail |
| `/` | Filter title, repo, author, number |
| `t` | Cycle time window (`1d` → `3d` → `7d` → `30d` → `90d` → `1y` → all) |
| `s` | Cycle open / all / closed (Pulls, Issues, All) |
| `m` | Toggle local read/unread |
| `n` `N` | Next / previous unread |
| `o` / `enter` | Open in browser |
| `c` | Fetch comments for the selected item |
| `r` | Refresh this item |
| `R` | Poll notifications now |
| `F` | Confirm a full backfill |
| `?` | Help |
| `q` | Quit |

Inbox is "needs you": unread, or open and (`review requested` or `assigned`).

## Config

`~/.gitsync/config.toml` is created on first run:

```toml
api_url = "https://api.github.com"
allowed_repos = []          # empty = everything you are involved in
poll_seconds = 90
backfill_days = 90
participating_only = true   # notifications you participate in, not every watch
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
