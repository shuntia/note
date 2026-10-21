# Eval results

Each run writes `<label>-<UTC timestamp>.json` (every run's verdict, scores and
fixture) and `.md` (the table), plus a `<label>-<timestamp>/` folder holding
each attempt's scratch server log and a copy of its scratch DB (`aitest`
fixtures only). Everything here except this README is gitignored.

Run by hand from the repo root:

```
CARGO_TARGET_DIR=target/eval cargo run -p note-eval -- --label before [--case visual-arts] [--model minimax] [--repeat 1]
```

The key defaults to `~/.config/note-eval/openrouter-free.key`; `--key-file` or
`NOTE_EVAL_KEY_FILE` overrides it. `--server-bin` skips the `note-server` build.
