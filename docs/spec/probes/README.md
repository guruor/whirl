# docs/spec/probes/: how the measurements in state-and-cache.md were taken

Four scripts, all stdlib-only Python 3, each printing its own result. They are
reproduction recipes for the `[L n]` rows in `docs/spec/state-and-cache.md`. They
are not part of the product and nothing in `prototype/` imports them.

Run everything from this directory. Nothing here writes outside a temp directory,
and nothing here touches the cache, the state directory or the wallpaper.

## `atomic_write_probe.py` (evidence `[L 5]`)

```sh
python3 atomic_write_probe.py 400
```

Two writers, same 512 KiB payload, same 400 iterations, one reader process in a
tight loop. `inplace` opens the target with `wb` (truncate) and writes; `replace`
writes a temp file, fsyncs, and `os.replace`s it onto the target. A read that
raises or parses without the `seq` key counts as bad.

Observed on macOS 26.5.2, arm64: `inplace` 2387 to 3832 bad reads of 2830 to
4270 (84% to 90%); `replace` 0 bad reads of 949 to 983. It works in a temp
directory under `$TMPDIR` and removes it afterwards.

## `hash_cost.py` (evidence `[L 5]`)

```sh
python3 hash_cost.py 20
```

Creates a 20 MB blob, then times a plain read against a read plus SHA-256, three
runs each. Observed: hashing 21.0 MB took 14.4 / 14.6 / 13.9 ms against 2.3 / 1.6
/ 1.5 ms for the read alone, so the hash is about 13 ms per 20 MB on this
machine. Deletes the blob.

## `index_size.py` (evidence `[L 5]`)

```sh
python3 index_size.py 500
```

Serialises a `cache/index.json` in the shape of section 2.1 with 500 synthetic
entries. Observed: 170 607 bytes compact, 220 135 bytes indented, 341 bytes per
entry.

## `size_stats.py` (evidence `[L 6]`, `[L 8]`, `[L 9]`)

```sh
curl -sS -A "whirl-spec-probe" \
  "https://wallhaven.cc/api/v1/search?sorting=random&purity=100&ratios=16x9&atleast=2560x1440&page=1" \
  -o p1.json
curl -sS -A "whirl-spec-probe" \
  "https://wallhaven.cc/api/v1/search?sorting=random&purity=100&ratios=16x9&atleast=2560x1440&page=2" \
  -o p2.json
python3 size_stats.py p1.json p2.json
```

Reads `file_size` out of the saved responses and prints the distribution. The
endpoint is the one `docs/spec/features.md` 2.3 specifies, and no API key is
needed for `purity=100`. `sorting=random` returns a different 48 images on every
request, so this probe is not reproducible by construction and every run is a
sample rather than a re-measurement. Three draws are recorded, all 48 images:

| Draw | Source | Min | Median | Max | Mean | Spread |
|---|---|---|---|---|---|---|
| 1 (`[L 6]`) | the original run for this document | 0.05 MB | 2.25 MB | 20.34 MB | 3.83 MB | 378x |
| 2 (`[L 8]`) | re-run while applying review round 1 | 0.19 MB | 2.11 MB | 16.03 MB | 3.23 MB | 82x |
| 3 (`[L 9]`) | re-drawn by the round-1 reviewer | 0.23 MB | 2.59 MB | 14.02 MB | 2.90 MB | 60x |

The conclusion the document draws from them rests on the tightest draw, not the
widest: even 60x leaves a count-only cap of 40 spanning roughly 9 MB to 560 MB.

A User-Agent is sent because the API is a public service; do not hammer it. Two
pages is the whole sample this document needs, and the card's own default
(`pages: 1`, hard cap 5) is the reason not to add more.

## What these probes deliberately do not cover

- Behaviour on real Linux and real Windows. No such hardware was available for
  this card, and `docs/research/linux.md` and `docs/research/windows.md` carry
  the checklists that must be run there instead.
- The eviction invariants against a running implementation. There is no
  implementation yet; that is the point of writing the invariants first.
