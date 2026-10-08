# Security

## Sign-in and sessions

- `POST /api/login {username, password}` sets an HttpOnly, SameSite=Lax session
  cookie valid 30 days (`Secure` when `public_base_url` is `https://`).
  `POST /api/logout` deletes the session row and clears the cookie.
- Four password verifications run at once server-wide; a login arriving while
  all four are busy is `503` with `Retry-After: 2`.
- Failed sign-ins count against 10 per username per 15 minutes; the eleventh is
  `429`. The counter is consulted only after a verification fails, so the
  right password always signs in. A success clears it.
- `POST /api/talk` runs one session per user (`409` for a second) and four
  server-wide (`503`, `Retry-After: 5`). `POST /api/tasks/{id}/agent` and
  `POST /api/agent/inbox` share the gate: `429` per user, `503` server-wide.
- `[limits] agent_sessions_per_day` (default 200, `0` lifts it) caps agent
  sessions per account in any 24 hours; beyond it every agent route answers
  `429` `{"error": "daily session limit reached"}`. The count comes from the
  `agent_session` rows in `event_log`; a token-started session names it
  (`token=<id>`).

## API tokens

Long-lived bearer tokens for scripts and other agents. They reach the task
routes and the calendar's by-external routes only; everything else needs the
session cookie.

- Settings → API tokens, or `POST /api/tokens {name}` →
  `{id, name, created_at, last_used_at, token}`. The `token` (`note_…`) is shown
  once; only its SHA-256 is stored. `GET /api/tokens` lists them;
  `DELETE /api/tokens/{id}` revokes. Names are 1–64 characters (`422`); at most
  20 per user (`409`).
- `Authorization: Bearer note_…` works on `GET/POST /api/tasks`,
  `PATCH/DELETE /api/tasks/{id}`, `PUT/DELETE /api/tasks/by-external/{external_id}`,
  `PUT/DELETE /api/calendar/by-external/{external_id}`,
  `POST /api/tasks/{id}/split`, `POST /api/tasks/{id}/flatten`,
  `POST /api/tasks/{id}/agent` and `POST /api/agent/inbox`. A bearer header that
  does not resolve is `401` even with a valid cookie. Disabling the user stops
  their tokens.
- Minting and revoking log `token_created` / `token_revoked`.

## Second factors

Every account enrols its own in Settings → SECURITY:

- **Passkeys.** "Add a passkey" re-asks for the password, then runs a WebAuthn
  registration; the credential is named and stored, at most 10 per account.
  WebAuthn needs a secure origin: `public_base_url` on `https://` (or
  `http://localhost`); elsewhere the button stays off.
- **Authenticator app.** "Set up authenticator" shows a QR code rendered in the
  page, the `otpauth://` URI and the base32 key. A code from the app finishes
  enrolment; until then the secret sits in `users.totp_pending`.

TOTP secrets are stored as base32 in `users.totp_secret`, unencrypted: the
database file is the trust boundary (it already holds the session table and
argon2 hashes). Back it up accordingly.

Routes, on the session cookie. Asking for a challenge and dropping a factor
re-check the password; finishing a registration rides on that challenge.

| Route | Body | Effect |
|---|---|---|
| `GET /api/security` | — | `{passkeys: [{id, name, created_at, last_used_at}], totp: {enabled, pending}, webauthn_available}` |
| `POST /api/security/passkeys/challenge` | `{password}` | creation options; `503` where passkeys can't run, `409` at the cap |
| `POST /api/security/passkeys` | `{name, credential}` | the row; `422` bad name or challenge, `409` cap or duplicate credential |
| `PATCH /api/security/passkeys/{id}` | `{name}` | the renamed row; `422`, `404` |
| `DELETE /api/security/passkeys/{id}` | `{password}` | `204`; `404` |
| `POST /api/security/totp/start` | `{password}` | `{secret_base32, otpauth_uri, issuer, account}`, the only response carrying the secret |
| `POST /api/security/totp/confirm` | `{code}` | `204`; `401` on mismatch |
| `DELETE /api/security/totp` | `{password}` | `204`, clearing secret and pending |

A wrong password is `401`; ten in 15 minutes are `429`. Changes log
`passkey_added`, `passkey_removed`, `totp_enrolled`, `totp_removed`.

The relying party is the host of `public_base_url`, named "Note". When browsers
see a different origin:

```toml
[admin]
rp_id = "note.example.net"
rp_origin = "https://note.example.net"
```

## Admin panel

Settings shows admin-role users an "Admin panel" row: accounts (create,
password reset, admin role, disable, sign out everywhere), server status, the
server log. Every action is logged as an `admin_*` row naming the actor.

Every `/api/admin/*` route needs, in order:

1. an admin-role session (`403`);
2. an admin grant: the password again plus a passkey or 6-digit code (password
   alone under `require_second_factor = false`), answered with a cookie
   `admin=…; Path=/api/admin; HttpOnly; SameSite=Strict` (`Secure` on HTTPS),
   valid 15 minutes, bound to the session and deleted with it. All routes but
   `gate`, `elevate` and `drop` are `401` without it;
3. same-origin provenance: a foreign `Sec-Fetch-Site` is `403`.

Elevation is limited like logins (10 per username per 15 minutes); codes and
assertions are single-use; admin responses are `Cache-Control: no-store`.

`[admin] require_second_factor = false` (alias `require_totp`) makes elevation
password-only for everyone; the gate reports it and the panel drops the second
field. Default `true`.

### Legacy shared seed

`secrets_dir` (default `persist/secrets`, gitignored) may hold `admin_totp`: one
base32 TOTP seed (SHA-1, 6 digits, 30 s) shared by every admin. It answers for
an admin who has enrolled nothing and is ignored for one who has. Create it
with `note-server totp-generate`; `note-server totp-uri` prints the URI of the
installed seed.

With no seed and nothing enrolled, a release build keeps that admin out: the
gate reports `totp: "missing"` and elevation is `503` (enrolment needs no
elevation, so Settings is the way in). Startup logs `admin_locked`. A
`dev-inspect` build lets such an account elevate on the password
([development.md](development.md#inspection-build)).

### Admin routes

| Route | Needs | Effect |
|---|---|---|
| `GET /api/admin/gate` | role | `{elevated, expires_at?, second_factor, methods: {passkey, totp}, require_second_factor, totp, inspect}`; `second_factor` is what to ask for (`passkey`, `totp`, `none`), `methods` what the admin holds, `totp` the legacy report (`required`, `password_only`, `missing`) |
| `POST /api/admin/elevate/challenge` | role | WebAuthn request options; `409` with no passkeys, `503` where passkeys can't run |
| `POST /api/admin/elevate {password, code?, assertion?}` | role | sets the grant cookie |
| `POST /api/admin/drop` | role | ends the grant |
| `GET /api/admin/status` | grant | version, build, uptime, DB size, counts, providers |
| `GET /api/admin/users` | grant | `[{id, username, role, disabled, sessions}]` |
| `POST /api/admin/users {username, password, admin}` | grant | `201 {id}`; `409` taken; `422` invalid |
| `PATCH /api/admin/users/{id} {role?, disabled?, password?}` | grant | `409` if it would leave no enabled admin or change the actor's own role |
| `POST /api/admin/users/{id}/revoke_sessions` | grant | `{revoked}` |
| `GET/POST /api/admin/invites`, `DELETE /api/admin/invites/{id}` | grant | invite links (below) |
| `PUT /api/admin/providers/llm/model` | grant | switches the chat model for every later call, kept across restarts, overriding `server.toml` |
| `GET /api/admin/log?limit&kind&before_id` | grant | `{rows, kinds}` |
| `GET /api/admin/traces`, `GET /api/admin/traces/{id}` | grant | agent session traces |

A password reset ends the account's other sessions; disabling ends all of them
and blocks sign-in.

## Invites

An admin can mint a one-time invite link instead of choosing a password for
someone: in the panel, or `note-server invite [--admin] [--name <username>]
[--days <n>]`. The invitee opens `/join/<token>` (`GET/POST /api/join/{token}`)
and chooses a password, and a username unless the invite names one. Lookups of unknown tokens and
refused joins are rate-limited per client address.
