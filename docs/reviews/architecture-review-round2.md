# Architecture and development guide, review round 2

Reviewer verdict: **all ten defects fixed, nothing weakened.** Every check below was
re-run by me against the committed tree at 0c39930 (the merge of the fix branch,
4619acf); none of it is taken from the fix card's handoff. The worktree was cut from
a `main` that already contained the fix, so no fast-forward was needed and no
pre-fix text was reviewed by mistake.

## The ten defects

1. **Section 10 quotes. FIXED.** I extracted every quoted span from section 10 and
   substring-searched it, whitespace-normalized, against `docs/spec/features.md`
   and `docs/spec/state-and-cache.md`: 14 of 14 attributed quotes are present
   verbatim (10.1's two quotes, 10.2's replacement sentence, 10.3's quote, 10.6's
   three, 10.8's two from state-and-cache and the prototype's "Cosmetic, unfixed
   here.", 10.9's whirl idle row, 10.10's "Stable name for status, history and
   error messages."). The four sentences section 10 says were removed or
   mis-transcribed are absent from the current specs, and a pickaxe over all
   branches confirms "one notification, and only once" never existed in
   state-and-cache.md, so 10.8's correction as a mis-transcription is honest. The
   three "Resolved by 0382364" rows are honest: commit 0382364 really did rewrite
   features.md 1.3, striking exactly the sentences those rows quoted (per-Space
   via `allSpaces`, "one call on macOS and Windows", "in flight and have produced
   no output yet") and replacing them with the sentences the resolved rows now
   quote, with the board query kept explicitly as history. 10.2's claim that the
   section now names `IDesktopWallpaper` for per-monitor checks out (features.md
   line 148).

2. **Cache paths in the transcript. FIXED.** I recomputed rather than trusted: all
   8 full `sha256/xx/yy/<digest>.<ext>` paths (6 at d4/35, 2 at 26/b8) fan out
   correctly from their own digests, and no `3f/9c` or `ab/12` remains anywhere.
   The digests themselves recompute as the document claims: sha256("space:ab12cd")
   = d435840ce84f..., sha256("pictures:1cc43835...") = 26b885cbb809..., and the
   local-entry chain is sha256(valley.jpg path) = 401df717... with sha256 of that
   origin_key = 3b29b617..., both verified. Development.md's
   `pictures:<sha256 of that path>` is a placeholder and the text says so. The
   fan-out rule is now stated in 2.11 and matches state-and-cache.md lines 197-200
   (`sha256/aa/bb/<64 hex>.<ext>`).

3. **`prev` semantics. FIXED.** 2.5 states the four-step selection rule (newest
   entry matching the anchor, walk older to the first differing digest, external
   fallback, `ERR no_prev`), states that `prev` mutates nothing and why recording
   a `prev` set as history would make the second `prev` unpredictable, and 2.12
   gained a row rejecting the prototype's `pop_back`. I confirmed the prototype
   really pops: whd.rs line 336 area does `st.history.pop_back()` under the lock.

4. **`resume` and `next_in_s` while paused. FIXED.** 2.5 states pause freezes
   `next_at` and resume sets `next_at = now + interval`, persisting before
   answering; 5.5 rule 8 carries both halves; 2.10's `next_in_s` row now reads
   `-` while `paused: 1`; the 2.8 `paused`/`resumed` event rows agree. This matches
   features.md F4 ("`resume` re-arms the next slot from now", line 35) and
   whd.rs:377, which does exactly `st.next_at = now_secs() + shared.cfg.interval`.

5. **The greeting. FIXED.** 2.4 now reads `OK whirl <semver> protocol <n>` with
   the bare-semver rule spelled out ("the product name appears exactly once"), the
   guide's section 7 example prints `OK whirl 0.1.0 protocol 2`, and 2.10's
   `daemon_version` row says the two-token form is that key's value and never a
   greeting token, so it cannot be read the other way. The 2.12 comparison row
   still tells the truth: I built the prototype (`rustc -O`) and drove it on a
   scratch socket, and it greets `OK whd 0.1.0 protocol 1` (VERSION is "whd
   0.1.0", whd.rs:305), which is name-once in shape but differs from the
   transcript's version and protocol number, exactly as the row says.

6. **The guide's section 7 example. FIXED.** The `set:` line's second field is
   `pictures:<sha256 of that path>`, the source `id` from the config the same
   section writes, not the `local` kind, with the `<source id>:<source-scoped id>`
   shape explained and tied to architecture 2.5 and 10.14. The status example
   prints `sources: 1` for the one source the same section configures. Internally
   consistent.

7. **Windows feature count. FIXED.** I opened `docs/research/windows.md`: its
   Cargo.toml excerpt (lines 122-124) declares four Win32 feature flags, COM plus
   `Win32_Foundation`, `Win32_System_Memory`, `Win32_UI_Shell`. Architecture 3.1
   (distribution row) and 3.3 now say exactly that, naming the same three.

8. **The sandbox wording. FIXED.** 3.2 now says the trap happened at launch,
   before any probe code ran: `Trace/BPT trap: 5` (exit 133), AMFI rejecting the
   ad-hoc signature (-423), `AppSandbox` in `libsystem_secinit`, and that the
   same binary without the entitlement runs, so the trap is the entitlement. It
   states the binary never reached a store write and carries the research's
   caveat, listed in 3.7, that a correctly signed sandboxed build is unverified.
   All of it matches macos.md [V9] (line 535, re-run 2026-09-25) and section 8.

9. **`sweep_deferred`. FIXED.** 2.10 gives it exactly one meaning: the rotation
   lock was already held, which is the only trigger state-and-cache.md 5.5 step 1
   gives (line 614). The disk-full row (7 row 4) and 2.10 give the failed sweep
   its own signal, `cache_over_reason: sweep_error`, matching 8.1 (lines 923-925),
   and 2.10's `cache_over_reason` vocabulary is now the four causes 5.4 makes
   exhaustive: `single_file`, `pinned`, `sweep_error`, `favorites_degraded`.

10. **The Windows per-virtual-desktop row. FIXED.** Section 7 gained row 15: v0.1
    cannot detect the interference, the rotation is logged as an ordinary success,
    and `whirl config check` cannot warn because the mode lives in the shell. I
    checked the claim against windows.md itself, not the architecture's summary:
    section 3 has IDesktopWallpaper not modelling virtual desktops at all, the
    mutual exclusivity of per-desktop and per-monitor modes sourced to [19], the
    lands-on-active-desktop / silent-revert behaviour to [19][30], and the
    sourcing caveat that [35] (WinDynamicDesktop) is an open proposal cited only
    as corroboration, which row 15 reproduces verbatim in spirit.

## The two optional minor citations

Both were touched and both are now right. 1.6 splits the nine Linux variables
into the four decisive signals (five variables, sway and i3 sharing a row),
which matches linux.md lines 527-536 exactly, including the "compositor sets for
its own children" phrasing and the gsettings-not-a-signal remark. 2.5.1, section
8 rule 9 and 10.9 now cite features.md 1.1's `whirl idle` row (line 58), which
really does say "Block until state changes. For frontends, so none of them
polls.", and F10 is correctly described as `status`/`sources` introspection.

## Nothing was weakened

I diffed b2b9697 (pre-fix) against 4619acf (post-fix) hunk by hunk. Every hunk
belongs to one of the ten fixes or the two optional citation items. No evidence
row, measurement or confidence label was deleted: "unverified" appears 16 times
pre-fix and 17 post-fix, the [V] citations [V5c] and [V9] are intact and [V9]
was added, and the one [V3] pointer that left the old 10.1 text is still carried
where the architecture follows that measurement (3.x, line 1100). Citation-set
diffs show only additions ([D 1 §7, §8], [D 5 §F4], windows sources [19], [30],
[35]) and the deliberate F10 reattribution. No transcript line changed beyond
the recomputed cache paths, and no grammar rule was altered. The only new
factual claim I could not re-derive from this machine is row 15's "the readback
agrees for the desktop the daemon can see", which the research supports by
source reports rather than by a measurement here; the row itself flags that and
names the check that would settle it, so I mark it unverified rather than wrong.

## What I re-ran

I rebuilt the prototype from this tree with `rustc -O` and drove it on a scratch
socket and scratch state directory: greeting `OK whd 0.1.0 protocol 1`,
`next_in_s: 1741` before `pause`, `next_in_s: -` while paused, and `next_in_s:
1800` immediately after `resume`, which is the adopted rule (2.10, 5.5 rule 8)
and whd.rs:377 observed live. No wallpaper was touched. I also recomputed all 8
cache paths and 6 digests with python3, and substring-checked all 14 quoted
sentences plus the 4 removed ones against the spec documents and the commit
0382364 diff.

## Noted, not a defect of this round

Architecture 2.10's `cache_over_cap` still reads "1 after a sweep driven by
contention rather than by a timer 5.2", while state-and-cache.md 5.4's
INV-CACHE-1 keys the flag on a non-zero overshoot. This predates the fix round,
was flagged by the fixer to the orchestrator, and is not one of the ten defects.