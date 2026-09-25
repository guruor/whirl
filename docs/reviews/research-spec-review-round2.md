# Review round 2: the four fixes to the research and spec pack

Card t_3f247196, reviewer p-amy, 2026-09-25. Change under review: commit `0382364`
("docs: fix the four defects from the research and spec review", card t_9fe9e5f4),
merged to `main` as `cedf839`. Diff inspected as `git diff 9e8726d cedf839`; pre-fix
text read from `git show 9e8726d:<path>`.

**Verdict: DONE. All four defects are closed, nothing was weakened, and the pack at
`cedf839` is fit to publish.**

## Defect 1: features.md 1.3 / F7 reconciled with macos.md - FIXED

The pre-fix text claimed the spike showed per-Space setting via `NSWorkspace.setDesktopImageURL`
with the `allSpaces` option, which `docs/research/macos.md` falsifies. The replacement
text (features.md 1.3, lines 111-137 at `cedf839`) was checked sentence by sentence
against macos.md sections 2 and 3:

| New claim in features.md | Where macos.md supports it |
|---|---|
| One `setDesktopImageURL` call covers the displays of the **frontmost Space only** | Section 2: "macOS applies the write to whichever Space is frontmost at that moment"; [V5] shows the write landing on `Spaces[<live>].Default` and its `Displays[<uuid>]` sub-node |
| With and without `allSpaces`, the **same two `Index.plist` nodes** are written | [V5]: `changed (choice or LastSet): 2` both directions, same two nodes; reproduced in this review (below) |
| No parameter in the public API names a Space (`forScreen:` takes an `NSScreen`, and that is all) | Section 2, near-verbatim |
| Per-Space coverage **not reachable through any documented API** | Section 2: "The limitation is targeting, not variation" |
| `allSpaces` undocumented (absent from `NSWorkspace.h` and Apple's docs), inert, **must not be used** | Section 3: "It appears in no header on this machine... It is inert... drop `allSpaces`" |
| The prototype's "rewrote all 10 Space nodes" is `LastUse` churn, not a rewrite | Falsified Lead 1 title, and [V5]/[V5b] |

No new claim beyond what macos.md supports, and no sentence anywhere in features.md
still says per-Space works: every `allSpaces`/Space mention (lines 38, 123-133,
152-153, 546, 569) states the same frontmost-Space-only answer.

The two "wait for the research" rows are closed:

- The `assumption:` block is replaced by a `decision:` block naming the four research
  notes with per-platform answers (Windows per-monitor reachable, macOS per-Space not
  reachable, Linux per-DE). Both cited section headings exist: windows.md "The central
  question, answered explicitly" and linux.md "The decisive column, at a glance".
- The Part 3 non-goal rows are rewritten as answered: "Per-display images as a
  blanket v0.1 promise" states the per-platform result; "Windows and Linux
  *per-virtual-desktop* differentiation" now says `IDesktopWallpaper` does not model
  virtual desktops at all and names `IVirtualDesktopManagerInternal::SetDesktopWallpaper`
  as undocumented per-build shell COM, which windows.md (lines 27, 43, 59) supports.

The board-query evidence is kept but explicitly marked "history, not current state"
(line 160), with the note that all four cards completed and were merged. The stale
"`card t_b9e349e6`, `running`" reference in 1.5 is gone. F7's cost cell now carries
the per-platform call cost. Defect 4's typo fix sits in the same file and is covered
below.

## Defect 2: macos.md Verified table names only committed files - FIXED, and re-run

Files named by the post-fix table and confirmed on the filesystem in
`docs/research/probes/`: `wallpaper_store.py`, `allspaces_test.sh`, `churn2.py`,
`lastuse_groups.py`, `inode_test.sh`, `live_spaces.py`, `raw_node.py`,
`sandbox_test.sh`, `probe-wrapper.sh`, `set-wrapper.sh`, plus `wp_probe.m` / `wp_set.m`
(the binaries are build outputs, built from committed sources per the README). All
exist. The pre-fix table named `layers.py`, `structure.py`, `allspaces_test.sh`,
`raw_node.py`, `probe-wrapper.sh` and the `l0/l1.plist` snapshots, none of which were
in the tree - that is what the defect was, and it is gone. The run-time `.plist`
snapshots (`w0/w1/w2`, `l0/l1`, `before`) are now declared uncommitted run-time
outputs with the reason (they carry this machine's wallpaper history), and the README
recipes show how each is produced. That is honest, not a dodge: the recipes are
followable and were followed below. All row labels [V1]-[V10] survive; no row was
dropped.

Re-runs performed in this review (both recipes from the committed README, binaries
built from the committed `.m` sources into `/tmp`):

- **[V5]** `sh allspaces_test.sh`: `changed (choice or LastSet): 2` with `allSpaces`
  and `2` without, the same two nodes both ways
  (`Spaces[BE72D3CF...].Default.Desktop` and its `Displays[26F88FD7...].Desktop`),
  `added=0 removed=0`, `LastSet: 2 of 2409`, `LastUse: 21 of 2409`. Matches the row.
- **[V5b]** `churn2.py $OUT/w0.plist $OUT/w1.plist` and
  `lastuse_groups.py $OUT/w0.plist $OUT/w1.plist`: `LastSet: 2 of 2409`,
  `LastUse: 21 of 2409`, 10 distinct timestamps, spread 2.23 ms (row records 1.67 ms
  and calls the spread scheduler jitter rather than a constant, which this run
  confirms). The row's "9 Space UUIDs" is the 8 named Space UUIDs plus the empty
  `Spaces[]` group, as in the original measurement.

Wallpaper side effects, stated per the card's rules: the live image before the run
was `/Users/govind.rajpurohit/.hermes/cache/scratch/wh-rotate/wh-cache/wh-commons-24733753.jpg`
(captured independently with `wp_probe` before starting). The script wrote Mac Yellow
then Mac Pink, then restored; the restore was verified by `wp_probe` readback twice
(once by the script, once independently after the [V5b] runs) and the live image is
`wh-commons-24733753.jpg` again.

## Defect 3: windows.md [41] version - FIXED

The new text quotes `windows = { version = "0.48.0", features = ["Win32_Foundation",
"Win32_System_Com", "Win32_System_Memory", "Win32_UI_Shell"] }` read from the live
file 2026-09-25. Fetched `https://raw.githubusercontent.com/sindresorhus/windows-wallpaper/main/Cargo.toml`
during this review: it declares exactly `windows = { version = "0.48.0", features =
["Win32_Foundation", "Win32_System_Com", "Win32_System_Memory", "Win32_UI_Shell"] }`
(package `wallpaper` 3.0.2). Version and all four feature flags match byte for byte.
The "four Win32 feature flags (COM plus three more)" reword is true about the live
file and preserves the reviewer's load-bearing claim (one dependency, not a runtime).

## Defect 4: features.md line 524 typo - FIXED

"could not got a row" is now "could not get a row" (acceptance-check section, line 559
at `cedf839`; the line number moved because 1.3 grew). The edited regions (1.3, F7
row, 1.5 startup bullet, Part 3 non-goal rows, acceptance bullets) were read in full;
no new typo arrived.

## Nothing weakened

`git diff 9e8726d cedf839 --stat` touches exactly: features.md, macos.md, windows.md,
probes/README.md and nine new probe files. No deletions of documents.

- **Evidence rows**: all [V1]-[V10] labels present before and after; each macos.md row
  change is an addition of a re-run observation or a re-pointing at a committed file.
  The only row content replaced was [V8]'s reference to throwaway scripts, and the
  replacement says so explicitly.
- **Measurements**: no number was deleted; [V5d] and [V6] add re-run values alongside
  the originals with the drift explained (inode numbers differ per run; age buckets
  are relative to now). [V6]'s 475 total and the 9 in-use Spaces are stable.
- **Citations and confidence labels**: the diff touches no confidence-label line
  (checked by grep over the deletion side of the diff).
- **Other platform documents**: linux.md, scheduling.md and state-and-cache.md are
  untouched by the fix commit. `prototype/` untouched.

## Publish batch

All verified at commit `cedf839` (merge of `0382364`, which is an ancestor - checked
with `git merge-base --is-ancestor`). Each hash below is the blob at `cedf839`; every
one resolves, and the fixed text was confirmed present in each of the four changed
documents via `git show cedf839:<path>`.

| Document | Blob at cedf839 | Notes |
|---|---|---|
| `docs/spec/features.md` | `8fee26f` | carries the 1.3 reconciliation, decision block, closed non-goal rows, typo fix |
| `docs/spec/state-and-cache.md` | `6e779a6` | unchanged by the fix; reviewed round 2 approved |
| `docs/research/macos.md` | `9431698` | Verified table re-pointed at committed files, re-run observations recorded |
| `docs/research/windows.md` | `5fd15f4` | [41] reads 0.48.0 / four flags |
| `docs/research/linux.md` | `1063208` | unchanged by the fix |
| `docs/research/scheduling.md` | `cecec57` | unchanged by the fix |
| `docs/reviews/research-spec-review.md` | `00e090d` | round-1 review, kept for the record |
| `docs/research/probes/` (tree) | 9 new scripts + `README.md` (`fe15871`): `allspaces_test.sh`, `churn2.py`, `lastuse_groups.py`, `inode_test.sh`, `live_spaces.py`, `raw_node.py`, `sandbox_test.sh`, `probe-wrapper.sh`, `set-wrapper.sh` | every script named by the Verified table |
| `docs/spec/probes/` (tree) | `README.md` at `e3f16a0` | state-and-cache probe, unchanged by the fix |

Pushing `cedf839` to the public repository publishes exactly the fixed versions above.
Nothing beyond this machine was verified: the GitHub fetch of the upstream Cargo.toml
succeeded today, but no other remote state was checked and none is assumed.

## What was not done

- No merge, no push, no fixes applied by the reviewer.
- Only [V5] and [V5b] were re-run end to end (the card's stated minimum). [V6]/[V7]/
  [V9] re-run notes in the table are the fix card's observations, judged for honesty
  (they read as measurements with drift explained, not dodges) but not independently
  reproduced in this review.