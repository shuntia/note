# Idle conversation summaries and the nightly memory harvest

Date: 2026-09-17. Status: approved for implementation (autonomous session).

## Goal

Two background passes turn conversations into durable memory:

1. **Idle summary** — a conversation that has gone quiet for an hour (or for
   as long as the provider's prompt cache lives, whichever is shorter) gets a
   short summary written onto it while its context is still cheap.
2. **Nightly harvest** — before the nightly planning session, the agent reads
   the day's conversations (their summaries, or the raw turns when no summary
   covers them) and writes the durable facts into persistent memory.

## Data model (migration appended to `db::MIGRATIONS`)

```sql
ALTER TABLE conversations ADD COLUMN summary TEXT;
ALTER TABLE conversations ADD COLUMN summarized_at TEXT;
ALTER TABLE conversations ADD COLUMN summary_through INTEGER;  -- last talk_messages.id covered
CREATE TABLE harvests (
    user_id INTEGER NOT NULL REFERENCES users(id),
    date TEXT NOT NULL,
    facts_written INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    PRIMARY KEY (user_id, date)
);
```

## Configuration

`server.toml`:

```toml
[agent]
idle_summary_min = 60      # default; a conversation quiet this long is summarised

[providers.llm]
cache_ttl_min = 60         # optional; when set, the idle threshold is min(idle_summary_min, cache_ttl_min)
```

`AgentConfig { idle_summary_min: u32 }` under `ServerConfig.agent` (serde
default, 0 disables the pass). `LlmSettings.cache_ttl_min: Option<u32>`.
`AppState.idle_summary_min` carries the effective value.

## Sessions

`SessionKind::Summarize` and `SessionKind::Harvest`.

- `Summarize` is single-call (`agent::single_call`, cap `IMPORT_MAX_TURNS`),
  system prompt `summarize.md`, registry `SUMMARIZE = ["summary_write"]`,
  terminal on `summary_write { summary }` (1..=1200 bytes, plain text, no
  markdown headers). No situational context block.
- `Harvest` runs multi-turn (`HARVEST_MAX_TURNS = 8`), system prompt
  `harvest.md` **plus** the standing context block (memory count, settings),
  registry `HARVEST = ["memory_query", "memory_read", "memory_write",
  "harvest_done"]`, terminal on `harvest_done { written: u32, note: String }`.
  `memory_write` calls made in a Harvest session record provenance in
  `memory_sources` with `source_id = "harvest:<date>"` (`ToolCtx` gains
  `memory_source: Option<String>`; `memory_ops::write` inserts the row when it
  is set).

Both prompts join `prompts::EDITABLE` and ship in `config/defaults/prompts/`.
`summarize.md`: third person, past tense, 2–6 sentences: what the user brought,
what was decided or done, anything left open, the mood if it was notable.
`harvest.md`: the digest is below; search memory before each write; add only
facts that will still matter next month (people, places, routines,
preferences, classes, decisions, how the user likes to be talked to); update
or supersede a fact that is now wrong; skip anything already held; skip plan
details that belong to a single day; finish with `harvest_done` naming the
count. The test `every_editable_prompt_ships_a_default` extends to both.

## Idle summary loop — `server/src/summaries.rs`

`spawn(state)` ticks every 60 s. Each tick, under the lock, selects up to 4
conversations:

```sql
SELECT c.id, c.user_id, u.username, u.category, c.summary, c.summary_through
FROM conversations c JOIN users u ON u.id = c.user_id
WHERE c.updated_at <= ?idle_cutoff
  AND EXISTS (SELECT 1 FROM talk_messages m WHERE m.conversation_id = c.id
              AND m.role = 'user' AND m.id > COALESCE(c.summary_through, 0))
ORDER BY c.updated_at LIMIT 4
```

Users whose `features.nightly` is off are skipped (test accounts stay
silent). For each, outside the lock, on `spawn_blocking`, respecting
`talk_gate` (skip and retry next tick when busy): history = the rows after
`summary_through` via a new `talk::history_after(conn, id, after_id, limit)`
(same shape as `history`), opening = "Summarise this conversation." with the
previous summary prepended as "Summary so far: …" when one exists. On a
successful `summary_write`, store `summary`, `summarized_at = now`,
`summary_through = MAX(id)` as of selection time. Failure logs
`summary_error` (throttled) and the row is retried next tick; three
consecutive failures for the same conversation back off for an hour
(in-memory map on the loop task).

Where the summary is read:

- `GET /api/conversations` rows gain `summary: string | null`; the chat drawer
  shows it as a second muted line under the title.
- `POST /api/talk` on an existing conversation whose history exceeds
  `TALK_HISTORY_LIMIT`: the summary is passed as `thread_note` prefixed
  "Earlier in this conversation: " so the model keeps the lost turns' gist.
- The harvest digest below.

## Nightly harvest — `server/src/harvest.rs`

`run_for_user(conn/deps, user, tz, date, now)` is called from
`nightly::run_for_user` after `archive_expired` and before `plan::generate`.
Idempotent on `harvests (user_id, date)`. Digest: every conversation of the
user with `updated_at` in the local day ending at the run (24 h window,
`[date-1 nightly_time, now]`), newest first, at most 12 conversations and
24 KiB total:

```
## <title> (<checkin YYYY-MM-DD | talk>, last active HH:MM)
<summary when summary_through covers the last message, else the last 20
user/assistant rows verbatim as "user: …" / "note: …", each row clipped to 600 chars>
```

An empty digest writes the `harvests` row with `facts_written = 0` and skips
the session. Otherwise run `SessionKind::Harvest` with the digest as the
opening message; `facts_written` = the number of successful `memory_write`
steps. A session error logs `harvest_error` (throttled) and still writes the
row so the nightly does not retry it every minute; the planning session
proceeds regardless. The context block's "Memory: n facts" line reflects the
new count for the planning session that follows.

`GET /api/memory` is unchanged; facts carry their `harvest:<date>` source in
`memory_sources`, which `GET /api/memory/{id}` exposes as `sources: []`.

## Testing

Server: config parsing (agent section default, cache_ttl_min min rule);
`summaries` selection query (idle, already covered, test user skipped,
partial re-summary picks rows after `summary_through`); a `MockLLM` that calls
`summary_write` end to end; `talk::history_after`; harvest digest builder
(window, cap, summary-vs-raw choice); harvest idempotency and error row;
`tests/conversations_api.rs` sees `summary`; `talk_api` thread_note on a long
thread; prompt test extended; registries invariant test extended (`HARVEST`
and `SUMMARIZE` disjoint from `IMPORT`, `HARVEST ⊂ NIGHTLY`).
