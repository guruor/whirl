# whirl quickstart

whirl rotates your desktop wallpaper through a folder of pictures you already have: a small daemon,
a config file, no window. Everything below was run in this order on macOS on 2026-09-27, with one
scratch home standing in for yours.

## 1. Install it

```sh
git clone --branch development https://github.com/guruor/whirl
cd whirl
cargo install --path crates/whirl-cli    --locked
cargo install --path crates/whirld       --locked
cargo install --path crates/whirl-worker --locked
```

`development` and not `main`, which cannot set a wallpaper yet. The three binaries land in
`$CARGO_HOME/bin` (`~/.cargo/bin`), already on your `PATH`; keep them in that one directory, because
`whirld` looks for `whirl-worker` beside itself and not on `PATH`.

## 2. Point it at your pictures

```sh
mkdir -p "$HOME/.whirl"
cat > "$HOME/.whirl/config.json" <<'JSON'
{
  "backend": "native",
  "sources": [
    { "id": "pictures", "kind": "local", "paths": ["~/Pictures/Wallpapers"] }
  ]
}
JSON
```

`~/Pictures/Wallpapers` is the line to change. The `local` source walks that folder and takes any
`jpg`, `jpeg`, `png`, `heic` or `webp` at least 1600x900.

## 3. Start it, and rotate

```sh
export WHIRL_CONFIG="$HOME/.whirl/config.json" WHIRL_SOCKET="$HOME/.whirl/whirl.sock" WHIRL_STATE_DIR="$HOME/.whirl/state" WHIRL_CACHE_DIR="$HOME/.whirl/cache"

whirld
```

`whirld` runs in the foreground and logs to stderr, so leave that terminal alone. In a second
terminal, paste that same `export` line, then:

```sh
whirl next
```

It prints `queued` and then `set: <digest> <origin_key> <path>`, naming the picture it set.
`whirl config check` prints what a source did, `candidates=` and `admitted=` and the rejection
counters; it asks the daemon, so it only works while `whirld` is up.

## 4. Stop it

Ctrl-C in the daemon's terminal. Nothing else is left running.

## 5. Three things worth knowing

- The screen changes a moment *after* `whirl next` returns. macOS applies the picture out of
  process, so the `set:` line means the write was accepted, not that the desktop has repainted:
  two runs here put the write the store records 2.6 s and 3.4 s later.
- `WHIRL_BACKEND` in your shell disarms the whole run without saying so. With `WHIRL_BACKEND=noop`
  exported (a leftover from a test) every stage still runs except the platform setter: `whirl next`
  prints its `set:` line, the history records it, and your wallpaper never changes. `whirl config
  check` prints `backend=` in its plan line; unset it and restart the daemon.
- Not implemented yet: the launchd agent that would start `whirld` at login and restart it. Today
  whirl rotates only while the daemon runs (`whirl next`, or every 30 minutes on its own,
  `schedule.interval_seconds`), and a reboot stops it until you start it again.
