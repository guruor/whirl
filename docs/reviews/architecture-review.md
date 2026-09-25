# Review: architecture and development guide (cards t_ef74be0b, t_598fa55e)

Reviewer: p-amy. Date: 2026-09-25. Worktree: `wt/t_cb0dcdee` at `9d2d735` (fast-forwarded
from `cedf839` per the orchestrator note). The decisive test was done first and blind: a
client was written against `docs/architecture.md` section 2 alone, without opening
`prototype/whd/whd.rs` or `prototype/whctl.rs`, and only compared against the prototype
afterwards. Nothing in this review fixed any reviewed file; the wallpaper was not touched;
the prototype was built in a scratch directory only.

## Verdict

Changes required. Ten defects, most serious first, then the per-criterion checklists and
what was verified and what could not be.

## The decisive test: a client from the spec alone

`spec_client.py` (Python 3.14, stdlib only, ~70 lines) was written from section 2 of
`docs/architecture.md` only. It connects, reads the greeting, sends `hello 2`, `ping`,
`status`, `history 2`, then opens a second connection, sends `subscribe`, reads one
`event:` line, and `close`s both connections. It was run against a mock daemon built from
the same section (greeting, negotiation, verbs, `subscribed:`, an event, a heartbeat):
every step behaved as the spec says (run log: greeting `OK whirl 0.1.0 protocol 2`; hello
-> `protocol: 2`, `OK`; subscribe -> `subscribed: 183`; first event -> `event: 184
rotate_start 4211`; close -> `OK` on both). Client exit 0.

Guesses recorded verbatim while writing, and what each means for the spec:

1. **Greeting version token.** 2.4's template is `OK whirl <daemon-version> protocol <n>`;
   2.10's `daemon_version` example is `whirl 0.1.0` (two tokens). I had to guess whether
   `<daemon-version>` is the bare semver or the two-token string, i.e. whether the product
   name appears once or twice in the greeting. Settled only by the transcript example, not
   by grammar. Worse: `docs/development.md` section 7 guesses the other way
   (`OK whirl whirl 0.1.0 protocol 2`). Defect 5 below.
2. **`subscribe` has no terminator**, but 2.6 says every response ends in exactly one
   terminator and 2.9 says subscribe "is a mode, not a verb with an answer". Reconcilable,
   but only by reading two sections against each other; the grammar block in 2.6 does not
   carry a subscribe-specific note.
3. **`prev` semantics.** Nothing in 2.5 says which entry `prev` selects, or whether history
   is mutated. Defect 3 below; the prototype's answer (destructive `pop_back`,
   `whd.rs:336`) is neither specified nor rejected.
4. Minor: the order of `already: 1` relative to `favorited:` is fixed only by transcript A;
   the bare `queued` token is defined only in 2.6's grammar block, not in 2.5's table, so a
   client written from the table alone would prefix-match it wrongly; whether a `gap:` line
   can appear without a `since` argument is settled only by an annotation inside a code
   block.

Comparison against the prototype (made only after the client was finished): all 16 rows of
2.12 check out against the code lines they cite (`whd.rs:114, 120, 161, 165, 218, 283,
407-414, 456, 466`; `whctl.rs:2, 32, 42, 68-72`; `wh-rotate/main.go` 2-minute client
timeout). The built prototype answers exactly as [L 3] records. One prototype behaviour the
architecture neither specifies nor justifies: **`prev` removes the last entry from history**
(`whd.rs:336`) before setting the previous one. The architecture's `prev` is silent on both
the selection and the mutation, so the supersession is implicit rather than stated.

## Defects, most serious first

1. **Section 10 quotes sentences that no longer exist in the spec documents.** The worktree
   ships with the round-1 spec fixes already merged (`0382364`, "fix the four review
   defects"). After those fixes, four of the fourteen rows quote vanished sentences:
   - 10.1 quotes features.md as saying *"The spike reports per-Space setting on macOS via
     `NSWorkspace.setDesktopImageURL` with the `allSpaces` option."* Current features.md
     (lines 122-133) says the opposite of that sentence and names `macos.md` as its source.
   - 10.2 quotes *"`all` is the default because it needs one call on macOS and Windows."*
     Current line 114 reads "`all` is the default because it is reachable from a
     short-lived worker on every platform the spike touched", and already states the
     one-call-per-frontmost-Space reach and the `SystemParametersInfoW` naming that 10.2
     corrects.
   - 10.3 quotes *"are in flight and have produced no output yet"*. Current features.md
     (lines 160-167) keeps the old query as labelled history, not as a current claim.
   - 10.8 quotes state-and-cache.md's *"one notification, and only once"*. Current text
     (lines 894-895) says "one state transition, so there is one notification, and the
     spike's wart does not survive R2".
   The card's acceptance criterion is that each quoted sentence is quoted accurately.
   As it stands, 10.1 even prescribes an action ("should be struck from features.md") that
   has already been taken. Fix: re-base section 10 against the current spec text; rows that
   the round-1 fixes resolved become "resolved by ..." rows, the rest quote the sentences
   that exist today. (Checked verbatim by substring search; 10.4, 10.5, 10.6, 10.7, 10.9,
   10.12, 10.14 all still quote existing text. 10.6b's "parses only the top-level scalars"
   exists split across a line break, so only the four above are stale.)

2. **The worked transcript's cache paths violate the fan-out rule the document claims they
   follow.** The 2.11 preamble says "paths under `cache/sha256/` follow the two-level
   fan-out of `[D 6 §3]`". `docs/spec/state-and-cache.md` line 200-202 fixes the fan-out,
   and its own example (line 665-666) makes it concrete: digest `ab12cd34...` ->
   `sha256/ab/12/ab12cd34...jpg`, i.e. the first two characters of the digest. Transcript A
   writes `anchor_path: sha256/3f/9c/d435840ce84f...jpg` for a digest starting `d435`
   (should be `d4/35`), and transcript B writes `sha256/ab/12/26b885...jpg` for a digest
   starting `26b8`. Both are wrong under the rule the document cites, and the transcript is
   the part an implementer copies. (Verified by recomputing: `sha256("space:ab12cd")` =
   `d435840c...`, so the fan-out directories must be `d4/35`.)

3. **`prev` is unspecified.** 2.5 gives `prev` the same data lines as `next` and the
   `no_prev` error, and 2.6 gives `via` the value `prev`, but nothing says which entry prev
   selects (the second-newest? the newest not equal to current?) or whether the ring is
   mutated. The prototype's answer is destructive (`whd.rs:336` pops the newest entry), and
   2.12 does not adopt, reject or even mention it. A client cannot predict what the second
   `prev` in a row does; an implementer must invent semantics. Fix: one paragraph in 2.5
   stating the selection and the (non-)mutation of history, plus a 2.12 row if the
   prototype's pop is deliberately dropped.

4. **`resume`'s effect on the schedule is unspecified, and so is `next_in_s` while
   paused.** `docs/spec/features.md` F4 says "`resume` re-arms the next slot from now";
   the prototype implements exactly that (`whd.rs:377`), and additionally prints
   `next_in_s: -` while paused. The architecture's `resume` row (2.5), the `resumed` event
   (2.9) and section 5.5 are all silent on the deadline. The document's opening claim is
   that it "leaves nothing open"; a client cannot tell whether `pause` for an hour means a
   rotation on resume or a rotation an hour later. Fix: one sentence in 2.5 or 5.5 adopting
   F4's rule, and a statement of `next_in_s`'s value while `paused: 1`.

5. **The two documents disagree on the greeting, and 2.4's grammar permits both.**
   Architecture transcript A: `OK whirl 0.1.0 protocol 2` (product name once). Development
   guide section 7: `OK whirl whirl 0.1.0 protocol 2` (twice), justified in the guide by
   `docs/architecture.md` 2.4 plus 2.10's `daemon_version: whirl 0.1.0` example. The
   prototype's greeting is name-once (`OK whd 0.1.0 protocol 1`, `whd.rs:23,305`), which
   supports the transcript. One of the two documents is wrong; a client author must guess
   (my client guessed name-once and asserted it). Fix: 2.4 states `<daemon-version>` is the
   bare semver and the product name appears exactly once, and the guide's example is
   corrected to match (or vice versa, but then the transcript and the prototype disagree
   too).

6. **The development guide's example transcript contradicts the architecture on
   `origin_key` and on the source count.** Guide section 7's `whirl next` output is
   `set: 05fcfcea... local:05fcfcea source <path>`: the origin_key prefix is the source
   *kind* (`local`), which is exactly the divergence architecture 10.14 says it does NOT
   make (2.5 uses the source `id`, e.g. `pictures:...`). And the guide's `status` example
   prints `sources: 2` while the config it just wrote defines exactly one source. Both are
   the lines a newcomer copies from the guide. Fix: regenerate the example output with the
   source id as the prefix and a consistent source count.

7. **The Windows `windows` crate feature count diverges from the research.** 3.1's
   distribution row says "none beyond the `windows` crate's two feature flags for COM";
   3.3 says "reached from the `windows` crate with two feature flags beyond COM".
   `docs/research/windows.md` section 1 (citing the live `sindresorhus/windows-wallpaper`
   Cargo.toml, its source [41], read 2026-09-25) lists four Win32 features:
   `Win32_Foundation`, `Win32_System_Com`, `Win32_System_Memory`, `Win32_UI_Shell` - COM
   plus three more. (I inspected the same manifest in the scratch workspace; it matches the
   research.) Fix both rows to the research's count.

8. **3.2 misstates the sandbox measurement.** "The measured failure is an ad-hoc signed
   binary with `com.apple.security.app-sandbox` unable to write the store `[D 1 §7]`." The
   research's [V9] measured a trap at launch - `Trace/BPT trap: 5`, AMFI rejecting the
   ad-hoc signature with error -423 - before any of the probe's code ran; the binary never
   reached a store write, so "unable to write the store" is not what was measured.
   (`docs/development.md` section 5 renders the same finding correctly as "broke the
   wallpaper setter in the research probe with `Trace/BPT trap: 5`".) Fix: state the launch
   trap, and note the research's own caveat that whether a correctly signed sandboxed build
   can write the store is unverified ([D 1 §8]'s own list).

9. **`sweep_deferred` has two meanings inside the architecture.** The status table (2.10)
   defines it: "`1` when the cache root showed another user" (state spec 5.5 step 1: the
   rotation lock was held). Failure-mode row 4 (disk full) says "the sweep is deferred
   rather than run (`sweep_deferred: 1`)". A client keying on the status key cannot tell
   which condition it saw, and the second trigger is invented - the state spec grants
   `sweep_deferred` exactly one meaning. Fix: give the disk-full case its own signal (or
   `cache_over_reason`-style wording) and keep `sweep_deferred` single-meaning.

10. **The Windows virtual-desktop interference is missing from the failure-mode table.**
    `windows.md` records, from three independent sources [19][30][35], that when
    per-virtual-desktop wallpaper mode is active, an `IDesktopWallpaper` set lands on the
    currently active desktop only or silently reverts. Section 7 has no row for it; 3.7
    covers only the slideshow question. This is a documented, user-visible silent failure,
    which is the exact class section 7 exists to enumerate. Fix: a row (what the daemon
    does: nothing it can detect; what the user sees: the change reverted or per-desktop
    only), or an explicit statement that v0.1 cannot detect it and where it is logged.

Minor, listed for completeness, not required to fix: 1.6 attributes nine environment
variables to "[D 3 §Detecting the environment] names as the decisive ones", but the
research names four as decisive (`XDG_CURRENT_DESKTOP`, `XDG_SESSION_TYPE`, `SWAYSOCK`
with `I3SOCK`, `HYPRLAND_INSTANCE_SIGNATURE`); the five extras are sensible but the
citation overstates. Likewise 2.5.1 and section 8 cite "[D 5 §F10]" for the
block-until-state-changes/no-polling rule, which lives in features.md's verb table
(`whirl idle` row, line 58), not in F10 (status introspection).

## What was run, and what was observed

- **Client test.** Client written blind; mock daemon written from the spec; full session
  passed (greeting, `hello 2` -> `protocol: 2`/`OK`, `ping` -> `OK`, `status` keys,
  `history 2` -> `count:`/`entry:`, `subscribe` -> `subscribed: 183`, first event received,
  `close` -> `OK` twice).
- **Prototype built and run.** `rustc -O` in scratch: `whd` 727,072 bytes, `whctl` 469,496
  bytes (matches [L 3]). Started on a scratch socket (`WHD_STATE_DIR=/tmp/whd-state`,
  `WHD_INTERVAL=3600`): startup line `whd whd 0.1.0 listening on ...`, RSS 1.7 MB.
  **Socket mode observed: `srw-------` (0600)**, matching the architecture's claim. The
  counterfactual also reproduced: a Python unix-socket bind at umask 022 with no chmod is
  `0755`, matching Pr4's stated basis. The spec client met the prototype and diverged
  exactly where 2.12 says: greeting `OK whd 0.1.0 protocol 1`, `hello`/`subscribe` answered
  `ACK unknown command '...'`, unknown verb `ACK unknown command 'frobnicate'` with the
  connection surviving, bare `last: ` with trailing space, bare `OK` for empty
  `history`/`favorites`.
- **Connection cost reproduced.** 32 held connections against the prototype: daemon-reported
  `rss_mb: 2.7`, `open_fds: 71` - exactly [L 4]'s row for 32.
- **Transcript digests recomputed.** All four (`space:ab12cd`, `pictures:401df...`,
  `/System/Library/Desktop Pictures/Mac Yellow.heic`, `pictures:1cc4...`) are genuine
  SHA-256 values of the strings the document says they hash.
- **Status table vs transcript.** All 47 keys in 2.10's table appear in transcript A's
  status block; the transcript's extra lines are the other verbs' record forms. No
  defined-but-unexercised verb; the only undefined tokens in A/B/C are the two deliberate
  error cases (mechanical check, matching [L 9]).
- **Config example.** Parses with `json.loads`; all six ordering rules hold at the example's
  defaults; `_comment_*` keys are the only extra keys. `docs/spec/probes/index_size.py 500`
  re-run: 170,607 bytes compact, 341 per entry - the R3 number reproduces exactly.
- **No placeholders, no typographic dashes/quotes.** Grep for TBD/TODO/FIXME/XXX/to-be-
  decided: only the escape-set mention `\uXXXX` (not a placeholder) and the reviewer-
  checklist row itself. Em dash 0, en dash 0, typographic quotes 0.
- **Citations spot-checked.** `prototype/README.md` lines 19-22, 26-35, 62, 86-87, 97 match
  the [M 1]-[M 12] values quoted; `3068 of 3508`, the 60x spread, `daemon.lock`,
  `next_at` wall-clock rule, `clock_jump` at `2 * interval`, ring of 50, `api_key_ref` all
  match the current spec documents.
- **Cross-platform verdict read against the research.** macOS: X1 (LaunchAgent/Aqua,
  TN2083), allSpaces inert, readback trap, AppleScript nuance, Sonoma floor, Focus absence
  all match. Windows: X2 (session 0, error 1459), per-monitor documented, per-VD
  undocumented and per-build, `REGDB_E_CLASSNOTREG` retry, `Enable(FALSE)`, `SetPosition`
  system-wide, hotplug-by-device-path all match. Linux: Hyprland deferral, GNOME per-monitor
  impossibility, KDE out of scope, sway per output, detection signals, package figures
  (swaybg 15.3/33.6 KB, hyprpaper 160.1/478.8 KB) all match. Scheduling: ThrottleInterval
  10.1 s, `StartInterval` loses firings, `KeepAlive` implies `RunAtLoad`, 72-hour
  `ExecutionTimeLimit`, `DisallowStartIfOnBatteries` default true, no-linger rule, and the
  whole-interval advance (which is the fix the scheduling document itself proposes) all
  match. Divergences found are defects 7 and 8 above, plus the two minor attributions.
- **Development guide, done not read.** Fresh clone from
  `https://github.com/guruor/whirl` into an isolated directory: `cargo build --workspace`
  fails immediately ("could not find `Cargo.toml`") - exactly the failure the guide's own
  status table predicts ("the Cargo workspace does not exist yet"). The sequence past that
  step therefore remains unverified against the real tree, which the guide states rather
  than hides. `actionlint` 1.7.11 on `.github/workflows/ci.yml`: exit 0, no findings. YAML
  parse: 6 jobs (`fmt`, `clippy`, `test`, `msrv`, `guards`, `artifacts`), triggers
  push/pull_request/workflow_dispatch, `permissions: contents: read`, caches keyed by
  runner OS + `Cargo.lock` hash, `guards` job contains the two `python3` heredocs the guide
  describes, artifact name `whirl-<os>-<sha>`.
- **CI claim verified.** `gh run list -R guruor/whirl --limit 10` returns empty: no CI run
  has ever executed. The guide claims exactly this and does not claim a green pipeline
  anywhere.
- **Frontend contract.** The five actionable codes all exist in 2.7's closed set; exit codes
  0/1/2/3 are consistent across 2.12, section 8 and the failure table; nothing in "may rely
  on" is contradicted by "must never". The one promise worth watching: "seq increases by
  exactly 1 per event" including heartbeats is specified and consistent.
- **Process model vs measurements.** [M 1] (1.8 -> 2.3 MB), [M 2] (12 -> 140 MB), [M 4]
  (21-25 MB, 1.2-3.4 s), [M 8] (883 MB), [M 9] (the ratchet trace) all verified against
  `prototype/README.md` lines 19-35, 62; [M 5] verified at `wh-rotate/main.go` (2-minute
  client timeout); [L 4]'s numbers reproduced at the 32-connection point. The 140 MB ratchet
  is used only to forbid decode in the daemon, which the numbers support; the delegated rows
  (1.7-1.8 MB idle) I reproduced myself. The in-process half remains non-reproducible from
  the tree, and the document's own 1.4 caveat says so - that caveat is accurate.

## Unverified (marked, not assumed either way)

- Any CI job on any runner (no runs exist; the guide says so).
- The development guide's section 7 sequence beyond `cargo build` (no workspace exists to
  build; the guide's status table says the same).
- Windows: pipe DACL, Job Object containment, per-monitor behaviour, slideshow/Spotlight
  interaction, signing cost figures (cited with retrieval dates, not re-fetched).
- [M 2]/[M 9] (the in-process ratchet): artifact not committed; the document's caveat
  states this and I did not re-run it.
- Vendor prices (99 USD/yr Apple, 9.99 USD/mo Microsoft): present with retrieval dates; not
  independently re-fetched in this review.

## Per-acceptance-criterion checklists

### Card t_ef74be0b (architecture)

| Acceptance criterion | Verdict |
|---|---|
| Cross-platform verdict direct, per platform, with cost | PASS, with two evidence divergences (defects 7, 8). Every "yes" names a resident cost; 3.6 states the three impossibilities as limitations; 3.7 lists every unverified claim. |
| Every decision tied to evidence | PASS. All 24 decision rows carry a basis; the bases I re-derived (socket mode, no-chmod bind, binary sizes, connection sweep, README citations, index size, probe citations) all held. |
| Worked transcript present and consistent with the grammar | PARTIAL. Verbs and status keys fully consistent (mechanically checked); the cache paths in A and B contradict the fan-out rule the same section cites (defect 2). |
| Process model consistent with the measurements | PASS. The 140 MB ratchet supports exactly the rule drawn from it; the delegated numbers were reproduced in this review. |
| Security model checkable | PASS. The socket claim was tested for real: `srw-------` observed on the running prototype; the 0755 no-chmod counterfactual reproduced. |
| Config example parses | PASS. Real parser, six ordering rules hold. |
| Failure modes match the research | PARTIAL. Rows match the research except the missing Windows per-desktop interference (defect 10) and the invented second meaning of `sweep_deferred` (defect 9). |
| Frontend contract deliverable by the protocol | PASS. |
| No TBD / unresolved placeholders | PASS. |
| Spec disagreements stated with accurate quotes | FAIL. Four rows quote sentences that no longer exist after the round-1 spec fixes (defect 1). |

### Card t_598fa55e (development guide)

| Acceptance criterion | Verdict |
|---|---|
| Repo layout, every crate and module | PASS. Matches architecture 1.2; prototype-kept rationale and rules present. |
| MSRV and dependency policy as rules | PASS. rust-toolchain contents, MSRV 1.85 rationale, four-step raise, guards enforcement - all stated and consistent with ci.yml. |
| Test matrix and honest gap | PASS. Six-job table matches the workflow file exactly; the cannot-prove table points at the research checklists. |
| Local development, noop backend | PARTIAL. The sequence's stopping point matches the guide's own status table (no workspace exists), but the example transcript contradicts the architecture twice (defect 6) and the greeting example is the wrong reading of 2.4 (defect 5). |
| CI workflow valid and jobs genuinely passable | PARTIAL. actionlint clean, structure verified, and the guide honestly reports no run has ever executed (`gh run list` empty, verified). Runner behaviour remains unverified, which the guide states. |
| Release process, signing and packaging | PASS as written; vendor figures dated but not re-fetched (unverified). |
| Contribution flow and review rule | PASS. |
| Good first issues | PASS. All seven point at real documents; #5's description of the heredoc matches the actual ci.yml. |