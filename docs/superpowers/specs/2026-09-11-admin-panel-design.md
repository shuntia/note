# Admin panel

A dedicated admin surface for Note, reached from Settings, that manages users
and sessions, shows server status and the server log, and — only in a
dev-inspect build — lets an admin inspect and change any user's data.

## Lockdown model

Three layers stack on every `/api/admin/*` route:

1. **Role.** The session's user must have `role = 'admin'`; anything else is 403.
2. **Elevation.** A short-lived *admin grant* obtained by re-entering the
   password plus a second factor — a passkey assertion, a code from the
   account's own authenticator app, or a code from the legacy shared seed. The
   grant is a second cookie
   (`admin=<token>; Path=/api/admin; HttpOnly; SameSite=Strict; Secure` when
   the site is HTTPS), 15 minutes absolute (no sliding), bound to the session
   (deleted with it), stored in `admin_grants`. Every admin route except
   `gate`, `elevate` and `drop` requires a live grant; reads included. A
   missing or expired grant answers 401 so the client can re-gate.
3. **Request provenance.** A request carrying `Sec-Fetch-Site` other than
   `same-origin`/`none` is refused (403). Mutations require a JSON body.
   Admin responses carry `Cache-Control: no-store`.

Elevation attempts share the login limiter's shape (10 per 15 minutes per
username) on a separate limiter, and a failed attempt costs the argon2 verify
regardless of which factor was wrong. TOTP codes are single-use: the highest
accepted 30-second step is stored per user and any code at or below it is
rejected — enrolment spends a step too, so the code that confirmed an app
cannot then elevate. A passkey challenge is held server side against the
session, single use, for 2 minutes; an accepted assertion writes back the
credential's sign counter and stamps `last_used_at`.

### Second factors (migration v13)

Factors belong to accounts, not to the server. `passkeys` holds up to 10
credentials per user (serialized webauthn-rs `Passkey`, unique `cred_id`);
`users.totp_secret` and `users.totp_pending` hold an account's authenticator
secret and an unconfirmed enrolment, base32 and in the clear — the database is
the trust boundary. Enrolment is `/api/security/*` on the ordinary session
cookie (so it needs no elevation, which is what keeps a factorless admin able
to get in), and every mutating call re-checks the account password.

Passkeys need a secure origin: the relying party is the host of
`public_base_url`, overridable with `[admin] rp_id` / `rp_origin`, and where
the origin is neither https nor localhost the gate reports
`webauthn_available: false` and the UI offers only codes.

Every mutation is written to `event_log` as `admin_<action>` with the acting
admin's `user_id` and a detail naming the target.

### Secrets

`secrets_dir` in `server.toml` (default `persist/secrets`, gitignored) holds:

- `admin_totp` — the shared admin TOTP seed, base32 (RFC 4648, unpadded,
  whitespace ignored), SHA-1, 6 digits, 30-second step, ±1 step tolerated.

`[admin] require_second_factor` in `server.toml` (default `true`, and reading
`require_totp` as its former name) selects the factors. Set to `false`,
elevation verifies the account password alone against the stored argon2 hash and
no factor is consulted; everything else about the grant — the cookie and its 15
minutes, the limiter, `admin_elevate` / `admin_elevate_denied` — is unchanged,
and the gate reports `require_second_factor: false`.

Absent or unreadable seed *and* nothing enrolled on the account, with
`require_second_factor = true`:

- **Release build:** elevation is refused with 503 and the panel says the
  server has no admin secret installed. The panel fails closed until the user
  installs the file. Startup logs one `admin_locked` row.
- **dev-inspect build:** elevation accepts the password alone and the panel
  shows the dev banner. The gate reports `totp: "password_only"`.

CLI helpers, run on the server host:

- `note-server totp-generate` prints a fresh seed and its `otpauth://` URI
  (writes nothing; the user installs the seed themselves).
- `note-server totp-uri` prints the enrol URI for the installed seed.

## Build flag

Cargo feature `dev-inspect` on `note-server`, off by default and off in the
Nix package. It compiles in the inspection routes, relaxes elevation to
password-only when no seed is installed, and reports `inspect: true` from the
gate. The web client is one build; it renders the inspection UI only when the
gate reports it. Dev run: `cargo run --features dev-inspect`.

## Schema (migration v10)

```sql
ALTER TABLE users ADD COLUMN disabled INTEGER NOT NULL DEFAULT 0 CHECK (disabled IN (0,1));
CREATE TABLE admin_grants (
    token TEXT PRIMARY KEY,
    session_token TEXT NOT NULL REFERENCES sessions(token) ON DELETE CASCADE,
    user_id INTEGER NOT NULL REFERENCES users(id),
    expires_at INTEGER NOT NULL
);
CREATE TABLE totp_replay (
    user_id INTEGER PRIMARY KEY REFERENCES users(id),
    last_step INTEGER NOT NULL
);
```

A disabled user cannot log in (the verify still runs, so timing does not reveal
the state) and any existing session or grant of theirs stops resolving.

## API

All under `/api/admin`, JSON in and out.

| Route | Needs | Effect |
|---|---|---|
| `GET gate` | role | `{elevated, expires_at?, second_factor: "passkey"\|"totp"\|"none", methods: {passkey, totp}, require_second_factor, totp (the former report, one more release), inspect}` |
| `POST elevate/challenge` | role | WebAuthn request options for this admin's passkeys; 409 none, 503 where passkeys can't run |
| `POST elevate` `{password, code?, assertion?}` | role | sets the admin cookie; 401 wrong, 429 limited, 503 no factor and no seed (release) |
| `POST drop` | role | deletes the grant, clears the cookie |
| `GET status` | grant | version, build, started_at, uptime_s, db_bytes, counts (users, sessions, push_subscriptions), providers (kind/model only), webpush, secrets.admin_totp |
| `GET users` | grant | `[{id, username, role, disabled, sessions}]` |
| `POST users` `{username, password, admin}` | grant | 201 `{id}`; 409 taken; 422 invalid |
| `PATCH users/{id}` `{role?, disabled?, password?}` | grant | 200; 409 when it would leave no enabled admin or touch the actor's own role/disabled |
| `POST users/{id}/revoke_sessions` | grant | `{revoked}` |
| `GET log?limit&kind&before_id` | grant | rows with `id`; `kinds` list alongside |

dev-inspect only:

| Route | Effect |
|---|---|
| `GET inspect/users/{id}` | `{config_path, config_toml, tasks, conversations, memory, events_today}` |
| `PUT inspect/users/{id}/config` `{toml}` | validates as `UserConfig`, writes `user.toml` |
| `GET inspect/users/{id}/conversations/{cid}` | messages |
| `GET/PUT inspect/users/{id}/memory/{mid}` | raw memory file |
| `POST inspect/sql` `{sql}` | `{columns, rows, truncated}` for a query, `{changes}` otherwise; 500 rows cap |

The existing `GET /api/admin/log` moves behind the grant; the Settings
"Server log" fold goes away in favour of the panel.

## Web

`Settings` gains an "Admin" group (admins only) whose row opens the `admin`
view. The view uses the Settings column vocabulary (groups, rows, folds,
switches) at a wider measure:

- **Gate**: a card like the login screen asking for the password and, when
  required, the second factor — "Use passkey" where the account has one, with
  "Use a code instead" when it also has an app, and the code field alone when
  that is all it has. An account with neither on a release server shows one line
  pointing at Settings instead of the form; password-only drops the second
  field, and keeps the dev-build note for dev-inspect builds only.
- **Status**: label/value rows.
- **Users**: one fold per user (role badge, session count) with password
  reset, role switch, disabled switch, revoke sessions; an "Add user" fold.
- **Server log**: kind filter, table, "older" paging.
- **Inspect** (dev only, under a rose-toned banner): user picker; folds for
  config (editable TOML), tasks, conversations (open to read), memory
  (open, edit); SQL console with a results table.

Elevation expiry drops the view back to the gate on the next 401.

## Testing

Unit: TOTP vectors (RFC 6238), replay rejection, seed parsing, last-admin
guard, `require_second_factor = false` overriding every factor, relying-party
construction (domain, bare IP, localhost, overrides), per-user enrolment
(start/confirm/remove, wrong code, replay), passkey cap and name rules. Integration: member
403 on every route; admin without grant 401; grant lifecycle (elevate, use,
expire, logout cascade); wrong code 401 and limiter 429; disabled user login and session behaviour; cross-site header
403; inspect routes absent (404) without the feature and present with it;
release build refuses elevation without a seed; password-only elevation
(no code accepted, wrong password 401 and logged, limiter 429); the
`/api/security` routes end to end against a software authenticator
(`webauthn-authenticator-rs`'s SoftPasskey), including the password re-check,
the gate's `methods`, a member enrolling, and a per-user secret taking over
from the shared seed.
