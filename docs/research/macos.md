# macOS wallpaper control: the store, the API, and what actually works

Research note for `guruor/whirl`. Documentation only, no product code changed.

**Why this exists.** `prototype/wh-rotate/set_darwin.go` calls
`NSWorkspace.setDesktopImageURL` with an `allSpaces` option, and `prototype/README.md` draws two
conclusions from that spike: that `allSpaces` "rewrote all 10 Space nodes", and that AppleScript
`set desktop picture` is "a silent no-op on Tahoe and must not be used". The card named a third
lead, that `desktoppr` needs a sudo pkg and is therefore unusable for a user-level tool. This note
tests all three on the machine the spike ran on, then decodes the plist the spike only diffed.

**Verification reality.** Unlike `docs/research/windows.md`, most of this was executed. Every claim
below is graded in the **Bottom line** table and every grade says which kind of evidence it rests
on: a command run on this machine with its real output, or a cited primary source (the AppKit
header on this machine, Apple's own technote, Apple's notarization documentation, or the source of
a maintained tool). The **Falsified** section is the part to read first: two of the three leads do
not reproduce.

Environment: MacBook Pro `Mac15,3`, Apple M3, 24 GB, macOS 26.5.2 (build 25F84), one attached
display (Gigabyte M27Q, 2560x1440), 8 Spaces, `Index.plist` 216,932 bytes with 2409 wallpaper
slots. Retrieved and executed 2026-09-25.

**Grounding note.** Two kinds of evidence appear here and they are numbered differently on purpose.

- `[1]` to `[9]`: external sources, listed under **Sources**. Each is a primary artifact: a header
  or a system binary on this machine, Apple's own documentation, or the README and source of a
  maintained tool. No claim rests on a blog post.
- `[V1]` to `[V10]`: observations from this machine, listed under **Verified on this machine** with
  the command that produced them and the observed result.

Provenance was tracked with the `grounded-citations` skill's ledger. Re-running its checker on this
file is expected to report low sentence coverage (about 6 per cent) and to fail on the three
sources that are local artifacts rather than URLs (an SDK header, a system binary, a release asset
listing), because the ledger holds URLs: `python3 .../sources.py verify docs/research/macos.md`.
That is a property of the evidence mix, not a gap in it. Most claims here are `[Vn]` machine
observations, and for those the citation is the command itself, in the V-table.

## Bottom line

| Question | Answer | Evidence | Grade |
|---|---|---|---|
| Where is wallpaper config stored? | `~/Library/Application Support/com.apple.wallpaper/Store/Index.plist`, one nested binary plist. The pre-Sonoma `~/Library/Application Support/Dock/desktoppicture.db` is gone from this machine | Ran [V1][V8], binary strings [3] | Verified on this machine |
| Does `allSpaces` change anything? | **No.** With and without it, exactly the same 2 nodes are written. The spike's "10 Space nodes" is a `LastUse` artifact, not a rewrite | Ran [V5] | Falsified as stated |
| Is AppleScript `set desktop picture` a silent no-op? | **No.** Both `Finder` and `System Events` variants change the wallpaper on 26.5.2. `Finder` writes the same shape as the AppKit API; `System Events` silently drops the placement option | Ran [V4] | Falsified as stated |
| Is there an "Appearance node"? | **No such node exists.** The two slots per node are `Desktop` and `Idle` (screen saver). Zero occurrences of `Appearance` anywhere in the store | Ran [V8] | Premise in the card is wrong |
| Can one image be varied per Space? | Yes, per-Space choices coexist and are respected. 475 Space nodes, 59 distinct images, and a write only ever touches the Space you are on | Ran [V3][V6] | Verified, with a caveat: you cannot choose which Space you write to |
| Per display? | Modelled (`Displays[<uuid>]` sub-nodes everywhere), not exercised. One display attached, so multi-display behaviour is untested | Ran [V3], not testable here | Modelled, unverified |
| Supported way to set it | `-[NSWorkspace setDesktopImageURL:forScreen:options:error:]`, public since 10.6, three documented option keys | Header on this machine [1] | Documented and certain |
| The undocumented part | An option key literally named `"allSpaces"`. It is not in the header, not in Apple's docs, and it changes no observable state | Header [1], ran [V5] | Undocumented and inert |
| Does the process need a GUI? | No GUI code needed, but it needs a GUI **session** (`Aqua` bootstrap). A launchd-submitted job in the Aqua session sets the wallpaper fine | Ran [V7], Apple TN2083 [2] | Verified on this machine |
| A daemon? | No. Daemons run before login, outside the window server context | TN2083 [2] | Documented and certain |
| Sandboxed? | Not testable here: an ad-hoc signed binary carrying `com.apple.security.app-sandbox` traps at launch, AMFI rejects the ad-hoc signature | Ran [V9] | Gap, mechanism proven |
| TCC prompt on first run? | None observed. 5 wallpapers written via AppleScript and 8 via AppKit, no prompt at any point | Ran [V4][V5] | Observed, not explained |
| Minimum macOS | Sonoma (14.0) for this store. The agent binary migrates "from pre-Sonoma legacy DesktopPicture database" and the migration marker is set on this machine | Binary strings [3], ran [V1] | Strongly supported, boundary taken from Apple's own migration strings |
| `desktoppr` needs sudo? | Half true. Homebrew's cask installs a `.pkg` (sudo), but the same release also ships a `.zip` with the same bare binary. The tool itself is an unprivileged CLI | Ran [V10], release API, tool README [5] | Partly falsified |
| Survives display hotplug? | The store keeps one node per display UUID ever seen, 16 nodes here on a machine with 1 display, so a reconnect reuses its node | Ran [V3] | Verified for retention, not for a live replug |
| Focus modes change wallpaper on macOS? | No. Apple's Focus filters cover Calendar, Mail, Messages, Safari. No wallpaper filter exists on macOS | Apple support [9], on-machine absence [V8] | Documented absence |

## Falsified: what the prototype believed and this machine does not show

### Lead 1: `allSpaces` is inert, and the "10 Space nodes" is `LastUse` churn

The claim: `NSWorkspace.setDesktopImageURL` with the `allSpaces` option "works and rewrote all 10
Space nodes of this machine's `Index.plist`".

Write image A with the option, image B without it, snapshot the whole store after each, and diff
every one of the 2409 slots ([V5]). Result, verbatim:

```
############ diff w0 -> w1 (allSpaces) ############
changed : 2
   ~ Spaces[779E9EF1-952C-4623-BA6F-2657CEB6FE7A].Default.Desktop
       type: 'imageFile' -> 'systemDesktopPicture'
       url: ... Wallhaven_l8x1pr.jpg -> 'file:///System/Library/Desktop%20Pictures/Mac%20Yellow.heic'
   ~ Spaces[779E9EF1-...].Displays[26F88FD7-...].Desktop
       (same two fields, same timestamp)

############ diff w1 -> w2 (no allSpaces) ############
changed : 2
   ~ Spaces[779E9EF1-...].Default.Desktop            Yellow -> Pink
   ~ Spaces[779E9EF1-...].Displays[26F88FD7-...].Desktop  Yellow -> Pink
```

Two nodes, identical set, either way. `added` and `removed` are 0 in both directions, so nothing
was created either.

Where the "10 Space nodes" number comes from: **every write bumps `LastUse` on a much larger set
of slots, and only `LastSet` and the URL on the node you actually wrote.** The same diff, counting
`LastUse` ([V5b]):

```
LastUse changed on 21 of 2409 slots, 10 distinct timestamps (all within 0.0012s):
   top-level slots bumped: 3
      AllSpacesAndDisplays.Idle
      SystemDefault.Desktop
      Displays[26F88FD7-C110-4C63-97AB-32974224D014].Desktop
   Space UUIDs bumped: 9 (each with Default.Desktop and Displays[<live display>].Desktop)
      102CB128-...  3A58736D-...  5FC2079E-...  779E9EF1-...  B5876B6B-...
      BE72D3CF-...  C6186AB4-...  DEABD3F7-...  Spaces['']
```

So a `plutil -p` diff of this store shows roughly twenty changed lines per write, spread over
every Space that currently exists, while only one Space's image actually changed. The prototype
counted changed *lines*, not changed URLs. That is the whole discrepancy, and it is my inference:
I did not have the prototype's script to inspect. What would settle it: run the spike again with
its own snapshot and diff, and check whether its 10 nodes were `LastUse`-only.

The 9 Space UUIDs with a bumped `LastUse` are the same 9 that had `LastUse` within 24 hours in a
cold read of the store ([V6]): `<24h: 9`, `<7d: 12`, `<30d: 35`, `<1y: 403`, `older: 16`, out of
475 Space nodes. So the machine has 8 Spaces plus an empty-UUID `Spaces['']` node, and 466 of the
475 Space nodes are historic leftovers. `LastUse` is the field that tells you which slots are in
use; `LastSet` tells you which were written.

### Lead 2: AppleScript is not a no-op here

The claim: `set desktop picture` via `Finder` *and* `System Events` "returns success and is a
silent no-op on macOS 26 Tahoe", so "it must not be used".

On 26.5.2 both work ([V4]). Five alternations, each verified by reading `desktopImageURLForScreen`
back and by the store:

```
$ osascript -e 'tell application "Finder" to set desktop picture to POSIX file "/System/Library/Desktop Pictures/Mac Pink.heic"'
finder -> Mac Pink             (readback confirms)

$ osascript -e 'tell application "System Events" to tell every desktop to set picture to "/System/Library/Desktop Pictures/Mac Yellow.heic"'
system events -> Mac Yellow    (readback confirms)
```

There is a real difference between the two routes, and it is the reason I would still not use
`System Events` (see the writer comparison below): it silently drops the placement option, writing
`type=imageFile` with `EncodedOptionValues` reduced to the string `"$null"`. `Finder` writes the
same `type=systemDesktopPicture` plus `placement=Crop` shape that the AppKit API writes.

I cannot explain the prototype's observation, but the plausible mechanism is a quoting bug rather
than an OS change: `osascript` from a shell needs the path quoted twice (once for the shell, once
for AppleScript), and a malformed path inside a `POSIX file` expression can fail in ways that do
not surface as a non-zero exit. That is inference, not evidence. What would settle it: the exact
`osascript` invocation the spike used, which is not in the repository.

### Lead 3: `desktoppr` and sudo

The claim: `desktoppr` "requires a sudo pkg installer, so it is not an option for a user-level
tool".

Partly true, and the conclusion still holds for a different reason. Homebrew's cask really does
install a pkg, which needs sudo:

```
$ brew info --cask desktoppr
==> desktoppr (desktoppr): 0.5,218
Command-line tool to set the desktop picture
Not installed
==> Artifacts
desktoppr-0.5-218.pkg (Pkg)
```

But the same GitHub release also publishes `desktoppr-0.5-218.zip` ([V10]), and the binary itself
is an ordinary unprivileged CLI: it calls the same public `NSWorkspace` API this note documents
([6]). The tool's own README states the user-level constraint plainly, that it must run as the
user and that a LaunchAgent is the way to do that [5]. So the disqualifier for whirl is not
privilege, it is that whirl would be shelling out to a third-party binary for something it can do
with one AppKit call.

### Lead 4: there is no "Appearance node"

The card asks what a "Display node, an Appearance node and a Space node each mean". Walking every
key and every binary blob in the store for the string `ppearance` returns 0 hits ([V8]). There is
no light/dark appearance split in this file. The four top-level nodes are:

| Top-level node | What it holds on this machine | Inferred meaning |
|---|---|---|
| `AllSpacesAndDisplays` | `{Type, Idle}`, no `Desktop` slot | The "one image on every Space and display" fallback. Its `Idle` holds the aerial asset `8BE8B524-6EAE-43F5-A3E8-01DCFA1BCD4B` with `aerialShuffleFrequency = shuffle_every_12_hours` |
| `SystemDefault` | `{Type, Desktop, Idle}` | The store's default choice, last written 2026-09-23 (UTC) by an older tool |
| `Spaces` | 475 entries keyed by Space UUID, plus one `''` entry | Per-Space wallpaper, one entry per Space UUID ever seen |
| `Displays` | 16 entries keyed by display UUID | Per-display wallpaper, one entry per display UUID ever seen |

The per-slot split is `Desktop` and `Idle`, and `Idle` is the screen saver, not dark mode.
WallpaperAgent's own log strings settle it: `Legacy User Settings - Pre-Content Migration:
[Desktop: %s, Screensaver: %s]` [3].

## 1. The store, decoded

`~/Library/Application Support/com.apple.wallpaper/Store/Index.plist`, next to an `aerials/`
sibling directory in the same parent ([V1]). It is a binary plist containing two further layers of
nested binary plists, which is why `plutil -p` alone does not tell you what image is where.

Shape, from the live Space node on this machine, with the blobs decoded ([V8]):

```
Spaces[<space-uuid>]                       # one node per Space UUID
  Default                                  # "the default display in this Space"
    Type = 'individual'
    Desktop                                # the logged-in desktop image
      LastSet, LastUse                     # UTC datetimes
      Content
        Choices = [ {
            Provider   = 'com.apple.wallpaper.choice.image'
            Files      = []
            Configuration = <binary plist blob, decoded below>
        } ]
        Shuffle = '$null'                  # the literal string "$null" means "unset"
        EncodedOptionValues = <binary plist blob, decoded below>
    Idle                                   # the screen saver image
      ... same shape, Provider 'com.apple.wallpaper.choice.aerials',
          Configuration = {'assetID': '8BE8B524-6EAE-43F5-A3E8-01DCFA1BCD4B'},
          EncodedOptionValues = {'values': {'aerialShuffleFrequency':
              {'_0': {'id': 'shuffle_every_12_hours'}}}}
  Displays[<display-uuid>]                 # per-display override inside this Space
    Type, Desktop, Idle                    # same shape as Default
Displays[<display-uuid>]                   # one node per display UUID, all Spaces
  Type, Desktop, Idle
```

Decoded `Configuration` for a still image (both fields come out of the blob):

```
{'type': 'imageFile', 'url': {'relative': 'file:///Users/.../wh-commons-24733753.jpg'}}
```

Decoded `EncodedOptionValues` for a still image:

```
{'values': {
    'color': {'_0': {'components': [0.254902, 0.411765, 0.666667],
                     'colorSpace': 'kCGColorSpaceGenericRGB'}},
    'placement': {'_0': {'id': 'Crop'}}
}}
```

Three details worth writing down because they cost time otherwise:

- `type` varies with both the image and the writer, and both values render:
  `imageFile` when `NSWorkspace` sets a file from the user's own directory; `systemDesktopPicture`
  when `NSWorkspace` or `Finder` sets a file from `/System/Library/Desktop Pictures`; and
  `imageFile` again when `System Events` sets that same `/System/Library` file ([V4][V5]). I did not
  isolate which variable decides, so do not treat `type` as a validity check or as a hint about who
  wrote the slot.
- Timestamps in this file are **UTC**. `LastSet = 2026-09-25 07:28:57` in the plist was
  `12:58:57` local (IST, UTC+05:30) in `log show` and `defaults`. A rotator that compares `LastSet`
  against `NSDate` local time is off by the timezone offset.
- `LastUse` moves for two reasons, not one. It is bumped on every in-use slot by any write (21
  slots here), and it also moves when the frontmost Space changes: `Spaces[BE72D3CF].Default.Desktop`
  showed `LastSet` 07:27:17 and `LastUse` 07:30:49, a tick with no write in between ([V3]). Use
  `LastSet` to answer "who wrote this image" and `LastUse` to answer "which slots are on screen".
  A rotator that stores its own "last applied" marker in `LastSet` will be confused by the
  screen-saver slot sharing the same field name.

## 2. Which layer is authoritative, and per-display vs per-Space

Four layers can hold a choice for the same screen: `AllSpacesAndDisplays`, `SystemDefault`,
`Spaces[<live>].Default` plus `Spaces[<live>].Displays[<display>]`, and `Displays[<display>]`.
**They disagree on this machine right now**, which is the useful part ([V3]):

```
AllSpacesAndDisplays.Desktop        <absent>
SystemDefault.Desktop               imageFile  .../spice/.../wallhaven-vpzyg3.jpg   LastSet 2026-09-23
Spaces[LIVE].Default.Desktop        imageFile  .../wh-commons-24733753.jpg          LastSet 2026-09-25 07:27
Spaces[LIVE].Displays[LIVE].Desktop imageFile  .../wh-commons-24733753.jpg          LastSet 2026-09-25 07:27
Displays[LIVE].Desktop              imageFile  .../spice/.../wallhaven-vpzyg3.jpg   LastSet 2026-09-23
Spaces[''].Default.Desktop          imageFile  .../spice/.../wallhaven-vpzyg3.jpg   LastSet 2026-09-23
```

While those three layers said `vpzyg3.jpg`, `desktopImageURLForScreen` returned
`wh-commons-24733753.jpg` ([V7]). So for the Space you are on, the per-Space node is the one that
decides, and the app that wrote the stale layers (an older tool, judging by the
`T/spice/wallpaper_downloads` paths) did so through a route that does not reach the live Space.

**Per Space: yes, and it is the normal state.** 475 Space nodes, all carrying a choice, 59 distinct
images ([V3]). The live Space held `Wallhaven_l8x1pr.jpg` while another Space held `wh-commons`
([V5]). One write changes one Space's image and leaves the rest alone.

**The limitation is targeting, not variation.** There is no parameter anywhere in the public API
that names a Space (`forScreen:` takes an `NSScreen`, and that is all, [1]). macOS applies the
write to whichever Space is frontmost at that moment, and the frontmost Space changed under this
session mid-experiment: a write at 07:27 UTC landed on `Spaces[BE72D3CF]`, and a write twelve
minutes later landed on `Spaces[779E9EF1]`, both of them live at the time ([V5] versus [V3]). So a
rotator can set the wallpaper for the Space the user is looking at, and cannot touch any other,
including the ones the user will switch to later. If whirl wants "same image everywhere", it has
to write on each Space change, or write the layers that all-Spaces fallback reads, and neither is
documented.

**Per display: modelled, unexercised.** Every Space node carries a `Displays[<uuid>]` sub-node, and
`Displays[<uuid>]` nodes exist at top level. `NSScreen.screens` had one entry here, so all I can
say is the model is per-display; whether a two-display write produces two changed sub-nodes was not
tested.

## 3. Setting it: what is public, what is private

Public and supported ([1], the AppKit header on this machine):

```
- (BOOL)setDesktopImageURL:(NSURL *)url forScreen:(NSScreen *)screen
                   options:(NSDictionary<NSWorkspaceDesktopImageOptionKey, id> *)options
                     error:(NSError **)error          API_AVAILABLE(macos(10.6));
- (nullable NSURL *)desktopImageURLForScreen:(NSScreen *)screen     API_AVAILABLE(macos(10.6));
- (nullable NSDictionary<...> *)desktopImageOptionsForScreen:(NSScreen *)screen  API_AVAILABLE(macos(10.6));
```

Three documented option keys, all `macos(10.6)`:

| Key | Header text ([1]) |
|---|---|
| `NSWorkspaceDesktopImageScalingKey` | "an `NSNumber` containing an `NSImageScaling`. If this is not specified, `NSImageScaleProportionallyUpOrDown` is used" |
| `NSWorkspaceDesktopImageAllowClippingKey` | "the image may be clipped... If this is not specified, `NO` is assumed" |
| `NSWorkspaceDesktopImageFillColorKey` | "used to fill any empty space around the image. If not specified, a default value is used" |

The header also documents the failure contract, and it holds: "This returns `YES` if the image was
successfully set; otherwise, `NO` is returned and an error is returned by reference". Handing it a
path that does not exist returns `NO` with `error=The file doesn't exist.` and writes nothing to
the store ([V5c]):

```
image: /Users/.../Library/Caches/spice/.../Wallhaven_l8x1pr.jpg
  screen M27Q -> FAILED error=The file doesn't exist.
  readback M27Q -> .../wh-commons-24733753.jpg
```

Private and undocumented: one option key, `"allSpaces"`, which `prototype/wh-rotate/set_darwin.go`
passes as `@{@"allSpaces": @YES}`. It appears in no header on this machine ([1], grep for
`allSpaces` in `NSWorkspace.h` returns nothing) and in Apple's published documentation for the
method. It is inert: see Falsified 1. `WallpaperAgent` contains the string
`com.apple.wallpaper.choice.allSpacesAndDisplays` ([3]), which is a different thing, the name of
the all-Spaces fallback in the store, and is not something a caller can pass.

The recommendation that follows is direct: **drop `allSpaces` from `set_darwin.go`.** It buys
nothing, it is undocumented surface that Apple can change without notice, and a reviewer reading
`@{@"allSpaces": @YES}` will assume it does something.

## 4. The three writers, compared

Same target image, same machine, store diffed after each ([V4][V5]):

| Writer | Works here | Nodes written | Slot shape written | Placement preserved |
|---|---|---|---|---|
| `NSWorkspace.setDesktopImageURL` | yes | `Spaces[<live>].Default.Desktop` and `Spaces[<live>].Displays[<live>].Desktop` | `type=imageFile` for user files, `systemDesktopPicture` for `/System/Library` files | yes, `placement=Crop` round-trips |
| AppleScript via `Finder` | yes | same 2 nodes | `type=systemDesktopPicture` | yes |
| AppleScript via `System Events` | yes | same 2 nodes | `type=imageFile`, `EncodedOptionValues` collapsed to the string `"$null"` | **no, silently dropped** |
| Direct plist edit | not tested | n/a | n/a | n/a |

Direct plist editing is the one route I deliberately did not try on a machine someone is using, and
it is also the route with a live owner: `WallpaperAgent` holds the store, and the file is replaced
rather than modified in place. The inode changed on every write (`203393620` to `203394346` across
one call, [V5d]), which means macOS writes a new file and renames it over the old one, so a
long-lived reader must re-open the path rather than keep a file descriptor, and a writer that
edits in place is racing a rename.

## 5. `desktoppr`, for the record

`desktoppr` is a maintained Apache-2.0 Swift CLI that wraps the same public API ([6]), plus a
`manage` verb that reads `com.scriptingosx.desktoppr` defaults, so a configuration profile can
drive it [5]. Two operational facts from its README are worth more than the tool itself, because
they describe the platform rather than the tool [5]:

- "When the path is a path to a folder, the system will rotate the wallpaper through all the image
  files in that folder. The default rotation time is 30 minutes and can be changed in the Wallpaper
  settings pane. `desktoppr` cannot change the frequency."
- "When you run multiple `desktoppr` commands in sequence in a script, the subsequent commands
  might not 'take.' Insert a `sleep 1` command in between to allow the process managing the
  wallpaper to 'catch up.'"

The second one is the concurrency answer independent of the tool: **serialise wallpaper writes and
give the agent a beat between them.** I did exactly that in [V5] (`sleep 3` between writes) and
every write landed. The README also notes that from Ventura on, a LaunchAgent-managed binary needs
a `com.apple.servicemanagement` profile to appear as a managed login item [5], which is the modern
delivery story for anything that runs at login.

Whirl should not shell out to it. One AppKit call is the same amount of code, with no third-party
binary, no pkg, and no version drift.

## 6. Dynamic and aerial wallpapers, and Focus

Aerial/dynamic wallpapers live in the `Idle` slot, and setting a still image does not touch them.
On this machine the `Desktop` slot holds a still image while `Idle` still holds the aerial asset
`8BE8B524-6EAE-43F5-A3E8-01DCFA1BCD4B` with `aerialShuffleFrequency = shuffle_every_12_hours`, and
`AllSpacesAndDisplays.Idle` holds the same asset ([V8]). Apple's `SystemWallpaperURL` preference is
likewise untouched, still naming `Sequoia Sunrise.mov` ([V1]).

So the answer to "does setting a still image disable them, and can it be undone" is: the screen
saver choice is preserved rather than disabled, the aerial selection is still in the file, and
reverting means writing the aerial choice back into the same `Desktop` slot. Whether macOS
preserves a user's *desktop* aerial selection somewhere when a still image overwrites the
`Desktop` slot I did not test, and the only layer that could hold it here (`SystemDefault`) holds a
stale image, not an asset ID. Confidence: medium that it is fully recoverable, low that it is
recoverable through any documented call. What would settle it: set an aerial desktop in System
Settings, run the still-image write, and diff.

Focus modes: **no wallpaper integration on macOS.** Apple's own Focus documentation lists filters
for Calendar, Mail, Messages and Safari only [7], and the store contains no Focus-keyed node at all
([V8]: 0 hits for `Focus` in the keys, and `WallpaperAgent` contains no `focus` string [3]).
Ignore the third-party pages that claim macOS changes wallpaper per Focus; the ones I read are
describing iOS. This removal of a feature is the reason a wallpaper daemon on macOS does not need a
Focus observer.

## 7. Minimum macOS version

**Sonoma, 14.0.** Three pieces of evidence, all from this machine:

- `WallpaperAgent` contains the migration logic and its log strings ([3]):
  `Attempting migration from pre-Sonoma legacy DesktopPicture database`,
  `LegacyDesktopPictureDatabaseMigrator`, `Desktop Picture Database Migration Complete`,
  `Running Sonoma options migrator`, and
  `Migration attempt required due to SonomaFirstRunMigrationPerformed NOT being set`.
- The preference domain that migration writes is set here ([V1]):
  ```
  $ defaults read com.apple.wallpaper
  {
      SonomaFirstRunMigrationPerformed = 1;
      StoreIndexMigrationVersion = 10;
      SystemWallpaperURL = "file:///System/Library/Desktop%20Pictures/.wallpapers/Sequoia%20Sunrise/Sequoia%20Sunrise.mov";
  }
  ```
- The store it migrated to contains a version field the agent compares against its own
  (`Running content migration as the StoreIndexMigrationVersion is set to %s which is less than the
  current version %s`) [3], and the pre-Sonoma database's directory
  `~/Library/Application Support/Dock/` does not exist on this machine ([V1]).

Apple's own `NSWorkspace` method is older (10.6, [1]), but the store it now writes through, the
versioning and the migration are Sonoma artefacts. Confidence: high. Gap: no pre-Sonoma Mac was
available, so I could not watch the migration run or check that the pre-Sonoma path still works on
Big Sur through Ventura.

## 8. Sandbox, entitlements, notarization, TCC

**The sandbox question is untestable on this machine without a real signing identity, and the
failure is informative.** Building the write probe with `com.apple.security.app-sandbox` and
signing it ad-hoc kills it at launch, before a line of my code runs ([V9]):

```
$ codesign --force --sign - --identifier com.whirl.sandboxprobe \
           --entitlements sandbox.entitlements wp-set-sandbox
$ ./wp-set-sandbox /path/to/image.jpg
Trace/BPT trap: 5        (exit 133)

$ log show --last 3m --predicate 'process == "wp-set-sandbox"'
(AppleMobileFileIntegrity) AMFI: '/Users/.../wp-set-sandbox' is adhoc signed.
amfid: /Users/.../wp-set-sandbox not valid: Error Domain=AppleMobileFileIntegrityError Code=-423
  "The file is adhoc signed or signed by an unknown certificate chain"
wp-set-sandbox[62582] (libsystem_secinit.dylib) AppSandbox
```

Control, same ad-hoc signature without the entitlement: builds, signs, runs, sets the wallpaper,
exit 0 ([V9]). So the trap is triggered by the sandbox entitlement, not by ad-hoc signing as such.
Practical consequences for whirl: you cannot smoke-test a sandboxed wallpaper writer on a dev
machine without a Developer ID or development certificate; and App Sandbox is a real, enforced
boundary at process start, not a soft one. Whether a *correctly signed* sandboxed CLI can call
`setDesktopImageURL` (it is a Mach message to the wallpaper agent, which a sandbox may or may not
permit) is **unverified**. My confidence that it needs an entitlement beyond `app-sandbox` is low,
and the cheapest route around the whole question is not to sandbox: whirl is a wallpaper rotator,
it needs no App Store distribution, and a plain Developer ID signed binary has no sandbox.

**Notarization.** Required for Developer ID distribution: "Beginning in macOS 10.14.5, software
signed with a new Developer ID certificate and all new or updated kernel extensions must be
notarized to run. Beginning in macOS 10.15, all software built after June 1, 2019, and distributed
with Developer ID must be notarized" [4]. The notary service takes flat installer packages and
disk images [4], which matters because a rotator that runs at login in a user's session ships as
a pkg or a signed binary inside a LaunchAgent, not as a Mac App Store app. A Gatekeeper prompt on
first run is the cost of skipping this, and there is no entitlement that avoids it.

**Entitlements.** The only entitlement that matters here is the one you should not add:
`com.apple.security.app-sandbox` [9]. Nothing in the public API needs a custom entitlement;
setting the wallpaper is a call to a system service, not a privileged operation.

**TCC.** No prompt appeared at any point: 5 AppleScript writes across `Finder` and `System Events`
and 8 AppKit writes, from a CLI in a terminal-hosted agent process, with zero dialogs and zero
errors ([V4][V5]). AppleScript normally needs Automation consent for the target app, so the
absence of a prompt means the grant already existed for this host, which makes the observation
weak evidence for anything else. What I can state is the negative result from the other direction:
the AppKit route itself raised nothing, and it does not read or write any TCC-protected location,
because the wallpaper agent, not the caller, opens the image file.

## 9. Daemon versus LaunchAgent

Apple states the rule, and the observed behaviour matches it ([2]):

- "To run your agent in a particular session type, use the session type strings from Table 1 as the
  value of the `LimitLoadToSessionType` property in your agent's property list file... If you don't
  specify the `LimitLoadToSessionType` property, `launchd` assumes a value of `Aqua`."
- On the window server: "You see this message if you try to connect to the global window server
  service from outside of the pre-login context before the user has logged in; typically this means
  that you're trying to use the window server from a daemon. You should not attempt to fix this by
  convincing the window server to trust your program."
- And the blunt version, which is the one whirl should design to: "It is not possible for a daemon
  to act on behalf of a user with 100% fidelity."

Ran, not just cited: a job submitted from this session runs in the Aqua session and can both read
and write ([V7]). `launchctl submit` reports the session type it chose:

```
$ launchctl submit -l com.whirl.probe4 -- /bin/sh .../probe-wrapper.sh
$ launchctl list com.whirl.probe4
{ "LimitLoadToSessionType" = "Aqua"; "Label" = "com.whirl.probe4"; "LastExitStatus" = 0; ... }

# wp-probe read, run by that job, not by the terminal:
NSScreen.screens count: 1
screen: M27Q
  desktopImageURL: /Users/.../wh-commons-24733753.jpg

# wp-set, run by that same launchd context, wrote the wallpaper:
  screen M27Q -> OK
  readback M27Q -> /Users/.../wh-commons-24733753.jpg
wp-set exit=0
# the store confirms it: LastSet 07:21:11 -> 07:22:29 on Spaces[<live>].Default.Desktop
```

So the shape that works: a **LaunchAgent in the user's Aqua session** (`LimitLoadToSessionType`
absent or `Aqua`), never a LaunchDaemon, and the daemon itself does not need a GUI toolkit or a
window, it needs the session. A daemon in `/Library/LaunchDaemons` runs at boot as root, before
login, with no window server connection, and cannot do this at all.

## Verified on this machine

Everything below was run on macOS 26.5.2 (25F84), one display, and the store was restored to the
image it held before each experiment unless noted. The probe sources are in
`docs/research/probes/`, with build commands and re-run recipes in `docs/research/probes/README.md`.

| # | What was run | Observed |
|---|---|---|
| [V1] | `sw_vers`; `defaults read com.apple.wallpaper`; `ls ~/Library/Application\ Support/com.apple.wallpaper/`; `ls ~/Library/Application\ Support/Dock` | macOS 26.5.2 / 25F84; `SonomaFirstRunMigrationPerformed = 1`, `StoreIndexMigrationVersion = 10`, `SystemWallpaperURL = .../Sequoia Sunrise.mov`; store dir contains `Store` and `aerials`; `Dock` does not exist |
| [V2] | `strings -a /System/Library/CoreServices/WallpaperAgent.app/Contents/MacOS/WallpaperAgent` | migration strings quoted in section 7; slot names `Desktop`, `Idle`; `com.apple.wallpaper.choice.allSpacesAndDisplays`; providers `image`, `aerials`, `helios`, `image-folder` |
| [V3] | `python3 probes/wallpaper_store.py layers` | the per-layer table in section 2: `Spaces` 475 nodes, `Displays` 16, 59 distinct images, live Space holding the newest `LastSet` |
| [V4] | `osascript` to `Finder` and to `System Events`, alternating Mac Pink, Mac Yellow, then the original image, each verified with `wp_probe` | every write took effect; `System Events` dropped the placement option |
| [V5] | `allspaces_test.sh` (write with `allSpaces`, write without, restore), recipe in `probes/README.md` | `changed (choice or LastSet): 2` both ways, the same 2 nodes; `LastUse` churn on 21 slots / 9 Space UUIDs |
| [V5b] | `python3 churn2.py w0.plist w1.plist` and `lastuse_groups.py w0.plist w1.plist` | `LastSet: 2 of 2409`, `LastUse: 21 of 2409`, 10 distinct timestamps inside 1.2 ms |
| [V5c] | `./wp-set <deleted-path> allspaces` | `screen M27Q -> FAILED error=The file doesn't exist.`, store unchanged |
| [V5d] | `bash inode_test.sh` | inode `203393620` to `203394346` across one write, directory listing never shows a temp file: the store is replaced, not edited |
| [V6] | `python3 live_spaces.py` | `LastUse` age buckets: `<1h 0`, `<24h 9`, `<7d 12`, `<30d 35`, `<1y 403`, `older 16`, total 475 |
| [V7] | `launchctl submit -l com.whirl.probe4 -- /bin/sh set-wrapper.sh` / `probe-wrapper.sh`; then `launchctl list`; then `raw_node.py` on `l0/l1.plist` | `LimitLoadToSessionType = "Aqua"`; launchd-run read saw `NSScreen.screens count: 1`; launchd-run write returned `OK` and bumped `LastSet` 07:21:11 to 07:22:29; `launchctl list \| grep whirl` afterwards: none |
| [V8] | `python3 probes/wallpaper_store.py dump` (was `layers.py` / `structure.py`) | the decoded tree in section 1; 0 hits for `Appearance`; `AllSpacesAndDisplays` has no `Desktop` slot |
| [V9] | `bash sandbox_test.sh` (build with `com.apple.security.app-sandbox`, ad-hoc sign, run) then the control without the entitlement | sandboxed: `Trace/BPT trap: 5`, AMFI `-423 "The file is adhoc signed or signed by an unknown certificate chain"`; control: runs, `OK`, exit 0 |
| [V10] | `brew info --cask desktoppr`; `curl https://api.github.com/repos/scriptingosx/desktoppr/releases/latest` | cask artifact `desktoppr-0.5-218.pkg (Pkg)`; release `v0.5` ships both `.pkg` and `.zip` |

Side effects on the machine, stated plainly: the wallpaper on the live Space was moved through
three images and left on `~/.hermes/cache/scratch/wh-rotate/wh-cache/wh-commons-24733753.jpg`, the
image the session started with. The Space that was frontmost partway through had held
`~/Library/Caches/spice/.../Wallhaven_l8x1pr.jpg`, which no longer exists on disk (Spice pruned
it), so that exact image could not be written back; `wp-set` reported
`FAILED error=The file doesn't exist.` The store was not otherwise edited by hand, no plist was
written directly, and the two crash reports generated by [V9] were deleted.

## Unverified, and what would settle each

1. **Multi-display behaviour.** One display attached. Untested: whether a second display gets its
   own `Spaces[<space>].Displays[<uuid>]` entry written in the same call, whether `allSpaces`
   becomes observable when displays have different images, and how `Displays[<uuid>]` at top level
   interacts with a live Space. Settle it with two displays, one write, and [V3].
2. **Display hotplug and resolution change.** The store retains 16 display nodes for one attached
   display, so retention is proven. Untested: whether the *image* survives replug, and whether a
   resolution change re-renders or re-selects. Settle it by unplugging a display, changing its
   resolution, and diffing.
3. **Display sleep and wake.** Not tested: waking a headless machine's display was out of scope for
   this session. Settle it with `pmset displaysleepnow`, a wake, and a diff.
4. **A correctly signed sandboxed writer.** See section 8. Settle it with a development
   certificate and the same probe.
5. **What wrote the stale `SystemDefault`, `Displays[<live>]` and `Spaces['']` layers** on
   2026-09-23, and whether writing them is a supported alternative route to an all-Spaces set. The
   paths suggest the Spice prototype. Settle it by writing those layers only, then switching Spaces.
6. **The pre-Sonoma path.** No pre-Sonoma machine available. Settle it on a Ventura VM.
7. **`type` semantics.** `imageFile` versus `systemDesktopPicture` for the same file depending on
   the writer is observed and unexplained; whether the value matters to any consumer is untested.

## What this means for the current code

Not a review, but the three places this research touches `prototype/`:

- **`prototype/wh-rotate/set_darwin.go`: drop `allSpaces`.** It is undocumented and provably inert
  here ([V5]), and its presence is exactly the kind of folklore the card wanted removed.
- **`prototype/README.md` carries two claims that do not reproduce.** "10 Space nodes verified
  rewritten" is `LastUse` churn ([V5b]); "AppleScript `set desktop picture` is a silent no-op on
  Tahoe" is false on 26.5.2 ([V4]). The README says not to modify it, so this note is the
  correction of record.
- **The read path is a trap.** `desktopImageURLForScreen` reflects the live Space's node, and the
  same file disagrees with three other layers ([V3]). Anything that reads the store to decide "what
  is the wallpaper" must read `Spaces[<frontmost>]`, and the frontmost Space UUID is not something
  the public API exposes.
- **Serialise writes, verify with readback, and treat `LastSet`/`LastUse` as UTC.** Desktoppr's
  own README documents the first [5]; the error contract in the header documents the second [1];
  and section 1 documents the third.

## Sources

[1] Local SDK header, `$(xcrun --show-sdk-path)/System/Library/Frameworks/AppKit.framework/Headers/NSWorkspace.h`,
    lines 204 to 237 (`NSWorkspaceDesktopImageScalingKey`, `NSWorkspaceDesktopImageAllowClippingKey`,
    `NSWorkspaceDesktopImageFillColorKey`, `setDesktopImageURL:forScreen:options:error:`, all
    `API_AVAILABLE(macos(10.6))`). Primary source, on this machine.
[2] Apple Technical Note TN2083, "Daemons and Agents".
    https://developer.apple.com/library/archive/technotes/tn2083/_index.html
[3] `/System/Library/CoreServices/WallpaperAgent.app/Contents/MacOS/WallpaperAgent`, `strings -a`.
    Binary on this machine, macOS 26.5.2.
[4] Apple, "Notarizing macOS software before distribution".
    https://developer.apple.com/documentation/security/notarizing_macos_software_before_distribution
[5] `scriptingosx/desktoppr`, README. https://github.com/scriptingosx/desktoppr
[6] `scriptingosx/desktoppr`, `desktoppr/main.swift` and `desktoppr/Defaults.swift`.
    https://github.com/scriptingosx/desktoppr/blob/main/desktoppr/main.swift
[7] Apple, "Change Focus settings on Mac" (Focus filters: Calendar, Mail, Messages, Safari).
    https://support.apple.com/en-gb/guide/mac-help/mchlff5da36d/mac
[8] `scriptingosx/desktoppr` release `v0.5` assets, `desktoppr-0.5-218.pkg` and
    `desktoppr-0.5-218.zip`. https://github.com/scriptingosx/desktoppr/releases/tag/v0.5
[9] Apple, "App Sandbox" (entitlement `com.apple.security.app-sandbox`).
    https://developer.apple.com/documentation/security/app_sandbox
