# Review: the research and spec pack (review card t_09fad5a8)

Reviewer: p-amy. Reviewed set: `docs/research/macos.md` (t_57b8de1e, 80587a0+55e9b93),
`docs/research/windows.md` (t_a7a134e2, d958b0f), `docs/research/linux.md` (t_be6f5e61,
2ef5dfc), `docs/research/scheduling.md` (t_b9e349e6, e210a6b), `docs/spec/features.md`
(t_58ccf6c9, 8043231), `docs/spec/state-and-cache.md` (t_4b15d270, 4987ba9), all read at
`main` 1261786, which carries all six merges.

Verdict: **changes required** (four defects, listed most serious first). No defect is in the
fabricated-evidence class: every pasted output I re-ran reproduced, and every citation I opened
said what the document claims it says. The pack is close to publishable; the fixes are one
reconciliation pass and three small precision items.

## Defects

1. **features.md 1.3 / F7: the macOS rationale for the `all` mode rests on a spike claim that
   macos.md falsifies, and the pack never reconciles what `all` delivers across Spaces.**
   features.md 1.3: "`all` is the default because it needs one call on macOS and Windows" and
   "The spike reports per-Space setting on macOS via `NSWorkspace.setDesktopImageURL` with the
   `allSpaces` option". macos.md now proves the opposite of the spike story on this machine
   ([V5], re-run by me): `allSpaces` is inert, with and without it exactly the same 2 nodes are
   written, and writes land on the frontmost Space only, so one call covers the *current*
   Space's displays and every other Space keeps its own image. macos.md states the consequence
   directly ("a rotator can set the wallpaper for the Space the user is looking at, and cannot
   touch any other"). A reader implementing from features.md alone believes one `NSWorkspace`
   call covers the machine; on a multi-Space Mac it does not. Evidence: my re-run of the
   committed A/B recipe (`wp_set` Yellow with `allspaces`, `wp_set` Pink without, store diffed
   both ways) returned `changed (choice or LastSet): 2` in both directions on the same two
   nodes, added=0 removed=0, and `LastUse` churn on 21 of 2409 slots across 9 Space UUIDs,
   exactly as [V5]/[V5b] record. Fix: a reconciliation paragraph in features.md 1.3 (macOS
   `all` covers the frontmost Space's displays; per-Space coverage is not reachable through any
   documented API; `allSpaces` is undocumented and inert and must not be used, per macos.md
   sections 2 and 3), and close the two now-answered "wait for the research" rows (the
   `assumption:` block naming the research cards as in flight, and the Part 3 non-goal row
   "Windows and Linux per-virtual-desktop differentiation ... wait for"). The board-query
   evidence describing the research cards as `running` is honest history but reads as current
   state in the published pack; mark it as historical or update it.

2. **macos.md: the Verified table names seven probe scripts that are not in the repository, so
   those rows cannot be re-run as committed.** [V5] names `allspaces_test.sh`, [V5b] names
   `churn2.py` and `lastuse_groups.py`, [V5d] `inode_test.sh`, [V6] `live_spaces.py`, [V7]
   `raw_node.py`, [V9] `sandbox_test.sh`. None is committed under `docs/research/probes/`
   (which holds `wallpaper_store.py`, `wp_probe.m`, `wp_set.m`, the probe plists, the
   transcript and the man-page copy). The macOS card's own acceptance criterion is "a
   'verified on this machine' section listing the exact commands and observed results, so a
   reviewer can re-run them". The measurements themselves are sound, and the README recipes
   make the headline one re-runnable (I reproduced [V5] and [V5b] numbers end to end with
   `wp_set` + `wallpaper_store.py diff`, including the 21-slot/9-UUID LastUse churn), but the
   V-table references files a reviewer cannot find. Fix: commit the seven helper scripts, or
   inline their few lines into the README recipes so each V-table row names only files that
   exist.

3. **windows.md [41]: stale version pin.** The doc: "Its `Cargo.toml` pins `windows = { version
   = \"0.44.0\", features = [\"Win32_Foundation\", \"Win32_System_Com\", \"Win32_System_Memory\",
   \"Win32_UI_Shell\"] }`". The live `sindresorhus/windows-wallpaper` `Cargo.toml` declares
   `windows = { version = "0.48.0", features = [same four] }`. The load-bearing claim (one
   dependency, four feature flags beyond COM) holds; the version string is wrong against the
   file cited. Fix: update the version or reword to "declares the four Win32 features" without
   pinning a number.

4. **features.md line 524: typo in the acceptance check.** "Anything that could not got a row"
   should read "could not get a row". Cosmetic, but this pack is the publish batch.

## What I re-ran on this machine, and what I observed

All on macOS 26.5.2 (25F84), the same machine the documents measured.

- **Wallpaper-set claim (macos.md [V5]/[V5b]/[V7])**: built `wp_probe.m`/`wp_set.m` with the
  committed commands; `wp_probe` read back the same display UUID
  (`26F88FD7-C110-4C63-97AB-32974224D014`), the same undocumented options
  (`NSWorkspaceDesktopImageScalingKey=3`, `AllowClippingKey=1`) and the same current image as
  the doc records. The `allSpaces` A/B recipe from `probes/README.md` reproduced `changed:
  2` in both directions on the same two nodes and `LastUse` churn on 21 slots / 9 Space UUIDs.
  **I restored the original wallpaper afterwards** (`wp_set` of the image that was live before
  the experiment) and verified the restore by `wp_probe` readback.
- **Store reads (macos.md [V1]/[V3]/[V8])**: `sw_vers`, `defaults read com.apple.wallpaper`
  (`SonomaFirstRunMigrationPerformed = 1`, `StoreIndexMigrationVersion = 10`, Sequoia Sunrise
  `SystemWallpaperURL`), store dir contains only `Store` and `aerials`, `~/Library/Application
  Support/Dock` absent; `wallpaper_store.py layers` gave 475 Space nodes / 16 Displays / the
  four top-level nodes / the stale `SystemDefault` spice layers; `dump` produced the decoded
  nested-plist shape of section 1.
- **launchd (scheduling.md)**: bootstrapped the committed
  `com.whirl.research.probeR.plist`; 5 fresh firings at deltas 2.06-2.20 s (doc claims
  2.05-2.12 s; one sample at 2.20 is within scheduler jitter); `launchctl print` reported
  `minimum runtime = 1` and `run interval = 2 seconds` as claimed. Booted out, and confirmed
  `launchctl list` shows no whirl job and `~/Library/LaunchAgents` has no whirl plist. The
  man-page quotes ([launchd.plist(5)] on kqueue-missed-interval, calendar coalescing, 10 s
  throttle, KeepAlive implying RunAtLoad) are verbatim in the committed
  `man-launchd.plist-macos-26.5.2.txt` at the cited lines. The probe transcript (342 lines,
  probes A-J plus R) is committed and carries the runs=0/runs=1 G/H evidence and the 5.39 s
  G2 latency sample.
- **Spec probes (state-and-cache.md)**: `index_size.py 500` returned 170607 bytes compact /
  220135 indented / 341 per entry, exact to the digit. `size_stats.py` on the surviving
  `/tmp/whirl-l6b` draw files reproduced [L 8] to the digit (48 images, 0.19-16.03 MB, p50
  2.11, p90 10.19, mean 3.23, spread 82x, cap-40 7.8-641.3 MB). `atomic_write_probe.py 150`
  reproduced the mechanism (in-place torn by 62.5% of reads at the lower iteration count,
  temp+rename 0 of 511). `hash_cost.py 20` under current load measured 24 ms hash / 2.5-3.2 ms
  read against the recorded 14 ms / 1.5-2.3 ms: the row is load-sensitive as the round-2
  handoff already records, and the conclusion it supports (hash cost negligible next to a
  download) is unaffected.
- **Prototype line citations**: `whd.rs:165` (`next_at = now_secs() + interval` after a
  rotation), `:104-108` (in-place `fs::write`), `:456` (unconditional socket unlink), the
  `:189/:195` sleep-then-compare shape; `main.go:55/67` (Pictures cache, single config path),
  `:246-247` (local id from filename), `:278-282` (`wh-<id>.<ext>`, extension from URL),
  `:324-353` (count-only prune), `:385-393` (the `-set` path returns before any prune),
  `:417/422-425` (dry-run prunes, failure path returns before prune) - all present as cited.

## Citation verification (opened sources, not trusted)

`sources.py verify --strict` results: windows.md `citations OK` against its 43-source ledger;
state-and-cache.md `citations OK` against the 11-source task ledger; features.md `citations OK`
once `wallhaven.cc/help/api` is registered (the shared profile ledger had lost the entry, as the
earlier round already found; I rebuilt it in my own scratch). macos.md fails the checker exactly
as its grounding note declares (local SDK header, system binary, release-asset listing are not
URL-ledger sources); I verified its URL sources by opening them instead: TN2083's "not possible
for a daemon to act on behalf of a user with 100% fidelity" and `LimitLoadToSessionType` text
are verbatim; the desktoppr README quotes (folder rotation, 30-minute default, `sleep 1`,
`servicemanagement`) are verbatim; the Apple Focus page names Calendar, Mail, Messages and
Safari and contains no wallpaper filter.

linux.md: [1] gschema (`file://`-only backend, both `picture-uri` keys), [2] `background.js`
(`PICTURE_URI_KEY`/`PICTURE_URI_DARK_KEY`, "same background object for all monitors",
`monitorIndex = 0` unless `.xml`, color-scheme pick), [13] `plasma-apply-wallpaperimage.cpp`
(`evaluateScript` DBus call, "for all desktops" success message, FillMode map), [26] the live
hyprpaper wiki (`wallpaper`/`listactive` as the two requests, the verbatim "older examples may
mention preload, reload, unload, or listloaded" warning, the empty-monitor fallback sentence,
`timeout`/`order=random`), [29] `Hyprpaper.cpp` error strings verbatim, [32] the feh manual
("GNOME shell desktops" refusal, `~/.fehbg`, `--xinerama-index`) - all verbatim against the
cited files.

windows.md: [1] interface page (method table, Windows 8 / Server 2012, desktop apps only),
[2] "Set this value to NULL to set the wallpaper image on all monitors.", [3] `S_FALSE` +
empty string when monitors differ, [9] detached-monitor remark, [12] interactive-services
sentences, [18] the Insider build 21337 blog announcing per-virtual-desktop backgrounds as a
Settings feature, [19] the AutoDarkMode issue comments ("Not both, this is a windows
limitation", "multi-screen multi-vdesktop wallpapers itself"), [20] `VirtualDesktop11.cs:181-182`
(`SetDesktopWallpaper`/`UpdateWallpaperPathForAllDesktops`), [24] `lib.rs` (`CLSCTX_LOCAL_SERVER`,
`GetMonitorDevicePathAt`, `SetWallpaper`) - all verbatim, except the [41] version pin (defect 3).
Two independent sources for the central per-monitor question are present: Microsoft Learn plus
four maintained implementations that call `SetWallpaper` per monitor.

scheduling.md: [1] Apple's "skipped when the computer is turned off or asleep" sentence, [4]
the 10-minute delay and its "applies only to time-based tasks..." scope sentence, [12] "The
default setting for this element is True.", [13] "A value of PT0S will enable the task to run
indefinitely.", [16] "stopped 72 hours after it starts to run", [19] the systemd.timer.xml
quotes (calendar catch-up and single-service-activation, `Persistent`, `AccuracySec` 1min,
monotonic pause, `WakeSystem` privilege, "already in the past"), [28] the History tab /
TaskScheduler Operational event log page, [29] the ExecType child set - all verbatim against
the live pages (the [28] quote also sits verbatim in `probes/evidence/`).

state-and-cache.md: [2] Apple's "cached data that can be regenerated as needed. Apps should
never rely on the existence of cache files", [3] XDG ("must be absolute", "non-essential
(cached) data", "actions history (logs, history, ...)", "user-specific runtime files and other
file objects"), [4] `%APPDATA% (%USERPROFILE%\AppData\Roaming)`, [5] "For new code, always use
SHGetKnownFolderPath...", [6] CSIDL "data repository for local (nonroaming) applications",
[10] POSIX rename "shall remain visible to other threads throughout the renaming operation" -
all verbatim. features.md: [1] Wallhaven "45 per minute" + 429, "24 results per page",
`X-API-Key`, "401 - Unauthorized error", "only the 'purity' filter will be available" - verbatim.

## Cross-document check

One contradiction found (defect 1). Checked and consistent: state-and-cache's `[D 1]`/`[D 2]`/
`[D 3]` claims match what macos.md/linux.md/windows.md actually say; the features.md vs
state-and-cache differences (index writer, recent-window size) are explicitly reconciled in
state-and-cache section 9; the scheduling recommendation (OS scheduler owns the process, the
daemon owns the clock) is consistent with features.md F1/F9 and the hosting models in
windows.md (per-user, interactive session) and macos.md (LaunchAgent, Aqua session).

## Acceptance criteria, per card

**t_57b8de1e (macos.md)** - every claim is a machine observation ([V1]-[V10], each with the
command) or a primary source (header on this machine, Apple docs, tool source); the
"Verified on this machine" table exists and a large subset reproduces (I re-ran [V1], [V3],
[V4]-equivalent, [V5], [V5b], [V7] read, [V8]); unverified items are seven numbered gaps each
with a settlement plan; the decoded plist structure is pasted, not paraphrased. Met, except
defect 2 (V-table names probe scripts that are not committed), which sits against this card's
own re-run criterion.

**t_a7a134e2 (windows.md)**: central question answered explicitly with Microsoft Learn as the
primary source and maintained OSS implementations as the second source; confidence levels per
claim; a 13-item real-Windows checklist; nothing claimed as run. Met (no Windows host exists
here, and the doc says so), except defect 3.

**t_be6f5e61 (linux.md)**: the matrix covers GNOME (Wayland/X11), KDE Plasma 6 (Wayland/X11),
sway, Hyprland and generic X11; the one-shot/resident column is marked plainly; per-monitor,
persistence, package and native-rotation columns present; confidence level per row; per-
environment test checklists; resident-helper footprint table; v0.1 recommendation with
deferrals. Met. Nothing was run on Linux and the doc claims nothing was.

**t_b9e349e6 (scheduling.md)**: launchd claims observed on this machine with the plist and
behaviour pasted (committed transcript; probe R re-run by me); the other platforms cite primary
sources, verified verbatim; the comparison table carries a source or an explicit inference
label on every row; the recommendation is explicit per platform with install/uninstall
commands. Met. Nothing was left scheduled.

**t_58ccf6c9 (features.md)**: every feature F0-F10 carries a cost and a v0.1 reason; the source
schema is concrete (2.2/2.3 with key tables, 2.6 with the three-method interface and the
one-line factory rule); Wallhaven endpoints named with key requirements and cited [1],
verified live; the multi-display policy names its dependency and fallback; no resident GUI
toolkit anywhere. Met, except defects 1 and 4.

**t_4b15d270 (state-and-cache.md)**: paths and formats are per-platform and unambiguous; the
eviction rule is INV-CACHE-1/2/3, stated on configs the document itself declares legal, and
the display-protection rule is grounded in macos.md's measured deleted-path failure; corruption
and partial-write behaviour is specified per file including the favorites degraded mode; the
concurrency rule names one writer per file; platform-convention citations verified. Met.

## Marked unverified (checked, not assumed)

Multi-display write shape, hotplug/replug survival, sleep/wake survival of `StartInterval` and
`StartCalendarInterval` (would require suspending the owner's machine), every Task Scheduler
and systemd behaviour, every Linux behaviour, signed-sandbox behaviour. All are labeled
unverified in their documents with a test plan, which the cards allow. Nothing I could check
was stated as fact beyond its evidence.