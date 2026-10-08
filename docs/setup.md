# Setup

The short version. Everything else is in the other docs.

## 1. Run it

On NixOS, import the module and turn it on:

```nix
services.note = {
  enable = true;
  settings.public_base_url = "https://note.example.com";
};
```

Elsewhere: `nix develop`, then `cargo run --release -p note-server`. It listens on `127.0.0.1:3271`.

## 2. Make yourself an account

```sh
sudo note-ctl create-user <name> <password> --admin   # NixOS
cargo run -p note-server -- create-user <name> <password> --admin   # anywhere else
```

## 3. Give it a model

With no provider, Note runs on a mock that only pretends. Point `providers.llm` at anything OpenAI-compatible, or at Anthropic, and hand it the key as a credential. See [configuration](configuration.md#servertoml).

## 4. Open it

Go to the address in a browser and sign in. On a phone, add it to the home screen. On a computer, there is a [desktop app](web.md) that asks for the address the first time it opens.

## What it does

- **Talk to it.** Tell Note a task, a plan, a deadline, a mood. It writes things down itself.
- **Call it.** With an empty message box, the send button is a mic. Tap it and talk.
- **Let it plan.** Each night Note sets the order you'll do things in tomorrow. Now starts the first one. Long-press a task to drag it somewhere else in the order.
- **Let it watch the day.** Note wakes itself a few times a day to tidy tasks and reshuffle the order. It speaks up only when it needs you, and calls only when you look free.
- **Leave it alone.** It keeps its own short notes and remembers what matters. You don't manage either.

## Optional

- **Push notifications.** Give it a VAPID key ([delivery](delivery.md)).
- **Chat from Matrix,** and have Note ring your phone ([delivery](delivery.md), [voice](voice.md)).
- **Better voices,** Japanese included ([voice](voice.md)).
- **Calendars and school feeds** ([importing](importing.md)).
- **Passkeys and TOTP** for the admin panel ([security](security.md)).
- **Quiet hours, day start and check-in times** live in your settings ([configuration](configuration.md)).

That's it. Poke around.
