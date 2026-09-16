# Per-user API tokens for tasks

Long-lived bearer tokens a user mints for themselves so scripts, CLIs, and
other agents can read and write that user's tasks without a browser session.
Tokens reach the task routes only. Everything else stays behind the session
cookie.

## Scope

- Tokens authenticate the existing `/api/tasks` routes plus a new
  `DELETE /api/tasks/{id}`.
- Tokens are minted, listed, and revoked over cookie-authenticated routes and
  from a new group on the Settings page.
- No token scopes, no expiry, no admin issuance. Revocation is the only
  lifecycle event.

## Token format and storage

Creation draws 32 random bytes, base64url-encodes them without padding, and
prefixes `note_`. The server stores only the SHA-256 hex digest. The plaintext
appears once, in the create response.

Verification hashes the presented secret and looks the digest up by its unique
index, so a wrong token costs the same single query as a right one and no
argon2 work is involved. Random 256-bit secrets need no salting.

## Schema (migration v11)

```sql
CREATE TABLE api_tokens (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id),
    name TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    last_used_at TEXT
);
CREATE INDEX idx_api_tokens_user ON api_tokens(user_id, id);
```

Revocation deletes the row. Sessions and tokens are independent: revoking a
user's sessions leaves their tokens alone. Disabling a user stops both from
resolving.

## Authentication

A new extractor `TaskPrincipal` in `auth.rs` is the only thing that reads the
`Authorization` header. `CurrentUser` is untouched, so a token can never reach
a route that was not deliberately switched to `TaskPrincipal`.

Resolution order:

1. If an `Authorization: Bearer <secret>` header is present, it must resolve
   to a stored token whose user is not disabled. Otherwise the request is 401.
   The cookie is not consulted on this path, so a stale token is never masked
   by a live browser session.
2. Otherwise the request resolves exactly as `CurrentUser` does.

`TaskPrincipal` carries the user id and username, and which credential
authenticated the request (`Session` or `Token(id)`).

On a successful token resolution the extractor updates `last_used_at`, but
only when the stored value is absent or older than one minute, so a busy
client does not write on every call.

## Task routes

`tasks_list`, `tasks_create`, `tasks_update`, `task_split`, and
`task_flatten` take `TaskPrincipal` instead of `CurrentUser`. Behaviour is
otherwise unchanged. Writes over a token use `Actor::User`, the existing rule
that the HTTP surface is always the user.

New route:

- `DELETE /api/tasks/{id}` → 204 when the task belonged to the caller, 404
  otherwise.

`tasks::delete(conn, user_id, id) -> rusqlite::Result<bool>` runs one
transaction: confirm ownership, remove `event_tasks` rows for the task and its
steps, delete the steps, delete the task. Nothing else references tasks. A
task in Now simply frees its Now slot.

## Token management routes

Cookie-only, via `CurrentUser`:

- `GET /api/tokens` → `[{id, name, created_at, last_used_at}]`, ordered by id.
- `POST /api/tokens {name}` → the same shape plus `token`, the plaintext
  secret. `name` is trimmed and must be 1 to 64 characters, else 422
  `{"error": ...}`. A user holds at most 20 tokens; past that, 409.
- `DELETE /api/tokens/{id}` → 204, or 404 when the id is not the caller's.

Create and revoke append to `event_log` as `token_created` and
`token_revoked` with the acting `user_id` and the token id and name in the
detail.

## Web client

`api.ts` gains `tokens()`, `createToken(name)`, and `revokeToken(id)`;
`types.ts` gains `Token` and `TokenCreated`.

Settings gets a new group, "API tokens", after the existing groups, in the
page's current row style:

- A name field and a "Create" button. On success the secret appears once in a
  selectable block under the field with a line saying it will not be shown
  again. Creating another token or leaving the page clears it.
- One row per token showing the name, the creation date, and last use
  ("never" when null), with a "Revoke" action that asks for confirmation
  before calling the API.
- Errors go through the page's existing notify mechanism.

No admin panel changes.

## Docs

README gains an "API tokens" subsection next to the login section: how to
mint one in Settings or over the API, the `Authorization: Bearer` header,
that only the task routes accept it, and the new DELETE route.

## Testing

Unit tests in `auth.rs`: generated secrets carry the prefix and are unique,
the hash lookup round-trips, a disabled user's token does not resolve, and
the last-used write is throttled.

Unit tests in `tasks.rs`: delete removes a leaf, a parent with its steps, and
event links; another user's task returns false and is untouched.

Integration suite `server/tests/tokens_api.rs`:

- mint, list, revoke; 404 for another user's token id; 422 on an empty or
  over-long name; 409 past the cap.
- a bearer token reaches every task route including delete.
- a bearer token on a non-task route such as `/api/me` is 401.
- a revoked token is 401 on its next use.
- a bad bearer alongside a valid cookie is 401.
- bearer on `/api/tokens` is 401.

Delete cases for the session path go in the existing `tasks_api.rs`.

The web build must pass its type check and lint as it does today.
