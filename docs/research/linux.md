# Linux wallpaper matrix: what actually sets a background, and what has to stay resident

Scope: how a background image can be applied on the Linux desktops whirl intends to support,
whether a one-shot process can do it, and what the architecture therefore has to own.

Status: research, pre-design. Nothing here has been executed on Linux.

## Method and its limits

There is no Linux machine or VM available to this project, and nothing in this document was run on
Linux. Every command and API below was read out of the upstream project's own source or
documentation, or out of a distribution's documentation, and is labelled with how far that evidence
goes. A command being plausible is not evidence that it works, and this document does not claim
otherwise. The reader should treat the matrix as a set of hypotheses with citations, each carrying a
release-blocking test checklist, not as a tested integration.

Two confidence labels are used throughout:

- **documented-and-certain** - the upstream project's own source or documentation states the
  behaviour, at the cited file and line.
- **implemented-elsewhere-and-likely** - a related tool, a distribution, or a downstream
  implementation demonstrates it, but the upstream project does not state it directly.
- **unverified-hypothesis** - reasoned from the above, not stated anywhere. Marked `[unverified]`
  inline.

## The decisive column, at a glance

"One-shot" means whirl can run a command, have that command exit, and the wallpaper stays set.
"Resident" means some process must keep a surface open, and no one-shot command can replace it.

| environment | setter | one-shot or resident | package |
|---|---|---|---|
| GNOME (Wayland) | `gsettings set org.gnome.desktop.background picture-uri ...` | **one-shot** | none beyond the session |
| GNOME (X11) | same command, same key | **one-shot** | none; session is being removed upstream |
| KDE Plasma 6 (Wayland) | `plasma-apply-wallpaperimage`, or a DBus `evaluateScript` call | **one-shot** | `plasma-workspace` |
| KDE Plasma 6 (X11) | same, same DBus path | **one-shot** | `plasma-workspace` |
| sway | `swaymsg output <name> bg <file> <mode>` | **one-shot for whirl** (sway owns the helper) | `swaybg` |
| Hyprland | `hyprctl hyprpaper wallpaper '[mon], [path]'` | **resident required** (`hyprpaper`) | `hyprpaper` + `hyprctl` |
| generic X11 | `feh --bg-fill`, `xwallpaper --zoom`, `hsetroot -fill` | **one-shot** | `feh` / `xwallpaper` / `hsetroot` |

The single architectural finding: on every environment except Hyprland, the compositor or desktop
shell already owns the background, and whirl's job reduces to one command and one exit. Hyprland is
the only target where a third-party process has to be running for the operation to succeed at all.

## GNOME (Wayland and X11)

### 1. The command, and whether a one-shot process suffices

The interface is the GSettings key `picture-uri` in schema `org.gnome.desktop.background`. The key
is a string holding a URI, and the schema states the backend accepts only local `file://` URIs.[1]
The schema itself ships in `gsettings-desktop-schemas`, not in gnome-shell.[1]

```
gsettings set org.gnome.desktop.background picture-uri 'file:///abs/path/image.jpg'
gsettings set org.gnome.desktop.background picture-uri-dark 'file:///abs/path/image.jpg'
```

**One-shot.** `gsettings` writes to the dconf database and exits. gnome-shell reads the key and owns
the drawing surface: `background.js` defines `BACKGROUND_SCHEMA = 'org.gnome.desktop.background'`,
reads `PICTURE_URI_KEY`/`PICTURE_URI_DARK_KEY`, and creates a `MetaBackgroundActor` per monitor.[2]
There is no surface for whirl to own and therefore no helper process.[2]

The same code path runs in a Wayland session and in an X11 session, because gnome-shell is the
reader in both: the background is a mutter actor, not an X11 root-window pixmap.[2] This is an
inference from the source, not a statement in GNOME's documentation.[unverified]

One trap that the prototype's design shares: the shell picks the dark or light key from
`org.gnome.desktop.interface color-scheme`, not from `picture-uri` alone.[2] Writing only
`picture-uri` leaves a dark-mode user looking at the previous wallpaper, which is a reported
symptom.[2] A correct implementation writes both keys.

### 2. Per-monitor support

None. This is a hard limitation, not a missing API call. `background.js` creates one
`BackgroundManager` per monitor but comments that "for other backgrounds we can use the same
background object for all monitors", and it forces `monitorIndex = 0` for anything whose filename
does not end in `.xml`.[2] An animated `.xml` background can hold variants chosen by monitor
resolution and aspect ratio, which is not the same thing as a different image per monitor.[2] Per
monitor wallpapers exist only as out-of-tree patches; one such patch adds an
`org.gnome.shell per-monitor-background` key specifically because the upstream schema has no such
thing.[8]

### 3. Restart and persistence

The setting survives logout and login because dconf persists it. The user database is a binary GVDB
file at `~/.config/dconf/user` (`$XDG_CONFIG_HOME/dconf/user`).[5] A session profile selects it with
a `user-db:user` line.[6] System administrators are shown writing the same
`picture-uri` and `picture-options` keys into a dconf profile to set a machine default, which is the
same key path whirl writes per user.[3] whirl should treat that file as belonging to GNOME and never
write it directly; `gsettings` is the supported writer.[5]

### 4. Package and failure mode

No package beyond the session itself: `gsettings` comes from GLib and the schema from
`gsettings-desktop-schemas`, both present in any GNOME session.[1] The realistic failure is not a
missing binary but a missing schema: on a GLib-equipped non-GNOME desktop, `gsettings` exists and
fails with "No such schema" because `org.gnome.desktop.background` is only installed by the GNOME
schema package.[1] Presence of `gsettings` is therefore not evidence of a GNOME session. This
matters directly to the prototype, which selects its setter with `have("gsettings") && ...`.[unverified]

### 5. Native rotation

GNOME does have one: if `picture-uri` points at a file whose name ends in `.xml`, gnome-shell treats
it as an animated background and drives it through `GnomeDesktop.BGSlideShow`.[2] The XML grammar is
a list of `<static>` and `<transition>` blocks with `<duration>`, `<file>`, `<from>` and `<to>`,
optionally per-`<size>` variants; gnome-desktop parses exactly those element names.[7] The
administrator documentation confirms XML backgrounds use the same
`org.gnome.desktop.background` keys.[4]

Do not defer rotation to it. The format has no published specification: the question "where can I
find a specification of all this" is answered with "I've not been able to find specified or
documented anywhere",[12] and the grammar above is known only from reading the parser.[7] A
generated file that GNOME silently rejects leaves the user with no wallpaper and no error. whirl
should set images itself and treat the XML slideshow as a documented limitation.

### Confidence and verification level

| claim | level |
|---|---|
| `picture-uri`/`picture-uri-dark` are the interface, `file://` only | documented-and-certain [1] |
| one-shot: dconf write, gnome-shell draws, no helper process | documented-and-certain [2] |
| same path on Wayland and X11 | implemented-elsewhere-and-likely (source shows one reader; docs silent) |
| no native per-monitor wallpaper | documented-and-certain [2][8] |
| persistence in `~/.config/dconf/user` | documented-and-certain [5][6] |
| `.xml` slideshow exists but is unspecified | documented-and-certain for existence [2][7]; the incompleteness of the docs is itself documented [12] |
| GNOME X11 session disappearing | documented-and-certain [9][10][11] |

### What must be tested on real Linux before release

1. Set both `picture-uri` and `picture-uri-dark` on a Wayland GNOME session; confirm the change
   appears immediately with no shell restart, and that it survives logout/login.
2. With `color-scheme` set to `prefer-dark`, confirm the dark key is the one that shows.
3. Set a wallpaper, then change GNOME Settings > Appearance to a plain colour, and confirm what
   whirl's next read of the key reports (the DE can overwrite the key without telling us).
4. Two monitors: confirm the image applies to both and document that per-monitor is refused, not
   silently half-applied.
5. On a session without `gsettings-desktop-schemas`, confirm the "No such schema" error is detected
   and reported rather than swallowed.
6. Confirm behaviour when the file is deleted after being set (what does gnome-shell show, and does
   the key still point at the missing path).

## KDE Plasma 6 (Wayland and X11)

### 1. The command, and whether a one-shot process suffices

Two routes, both one-shot, and they are the same route underneath. The shipped tool
`plasma-apply-wallpaperimage` builds a JavaScript snippet that iterates `desktops()`, sets
`d.wallpaperPlugin = 'org.kde.image'`, then `d.currentConfigGroup = ['Wallpaper',
'org.kde.image', 'General']` and `d.writeConfig('Image', 'file://...')`, optionally adding
`FillMode`.[13] It sends that string as a DBus method call to `org.kde.plasmashell`, object
`/PlasmaShell`, interface `org.kde.PlasmaShell`, method `evaluateScript`, and exits.[13] KDE's own
scripting documentation documents the same call shape for running a script from the terminal.[14]

```
plasma-apply-wallpaperimage --fill-mode preserveAspectCrop /abs/path/image.jpg
```

**One-shot.** plasmashell owns the desktop containment and draws the wallpaper; the caller only
writes configuration and exits.[13]

A security caveat that belongs in the error path: DBus script execution can be administratively
disabled. When the scripting console is disabled, an `evaluateScript` call fails with
"Administrative policies prevent script execution", and the switch is
`[KDE Action Restrictions] plasma-desktop/scripting_console=false` in `kdeglobals`.[15]
`plasma-apply-wallpaperimage` surfaces a DBus error message when the call fails,[13] so a whirl
adapter must treat a non-empty error reply as a hard failure and not as success.

Wayland and X11 are the same code path here: the target is the session bus, not an X display.[13]
That the interface also exists under Plasma 6 Wayland is asserted by community reports of `qdbus6`
against `plasmashell` on 6.0.2 and by the tool still being built from `plasma-workspace` master.[13]
[unverified]

### 2. Per-monitor support

The stock tool does not do per-monitor: it loops over every `desktops()` entry with the same
image, and its own success message says the wallpaper was set "for all desktops".[13] Plasma does
create one containment per screen, so a scripting caller could address a single one by index, and
users do keep per-screen wallpaper settings in separate containment groups of the applets config
file.[18] Doing that from whirl means depending on containment ordering, which nothing documents
stably. whirl should set all screens and declare per-monitor out of scope for KDE.[unverified]

### 3. Restart and persistence

`writeConfig` writes through to Plasma's application config, which lands in
`~/.config/plasma-org.kde.plasma.desktop-appletsrc`; the Arch wiki points at that file for the
desktop containment state (it names `lastScreen` values there).[18] Plasma rewrites the file when it
shuts down cleanly, which is the classic reason changes "do not stick" after a crash.[18] whirl must
not edit that file; it must go through `writeConfig`/DBus and then accept that the DE owns the
value.[13]

### 4. Package and failure mode

`plasma-apply-wallpaperimage` ships in `plasma-workspace`, the package that also provides
plasmashell.[13] If it is absent but plasmashell is running, the same operation is still reachable by
calling `evaluateScript` directly.[14] If plasmashell is not running, the DBus call fails with a
"service not found" style error, which the tool reports as an error message rather than an
exception;[13] a whirl adapter must handle the no-session case explicitly rather than treating it as
a bad image.

### 5. Native rotation

Yes, and it is a first-class plugin: `org.kde.slideshow`. Its configuration schema defines
`SlidePaths` (a string list of directories), `SlideInterval` with a default of 900 seconds,
`SlideshowMode` for ordering, `UncheckedSlides` and `SlideshowFoldersFirst`.[16] The wallpaper QML
switches the rendering backend between `SingleImage` and `SlideShow` on the plugin name, and feeds
`SlidePaths`/`SlideInterval` into the slideshow timer.[17]

This is the one environment where delegating is genuinely attractive: for a local-folder source,
whirl could set the plugin and the folder and then hold no state at all.[16] The cost is that the
user's rotation is then invisible to whirl (no history, no source mixing, no "next"). Recommendation:
support it as an explicit delegate mode, default off, and do not make it the normal path.

### Confidence and verification level

| claim | level |
|---|---|
| DBus `evaluateScript` + `writeConfig(Image)` is the interface | documented-and-certain [13][14] |
| one-shot; plasmashell owns the surface | documented-and-certain [13] |
| scripting can be blocked by action restrictions | documented-and-certain [15] |
| all screens, no per-monitor from the stock tool | documented-and-certain [13] |
| persistence in `plasma-org.kde.plasma.desktop-appletsrc` | implemented-elsewhere-and-likely [18] |
| works identically on Wayland | likely (same session-bus call; no upstream statement) [13][14] |
| `org.kde.slideshow` is a supported native rotation | documented-and-certain [16][17] |

### What must be tested on real Linux before release

1. `plasma-apply-wallpaperimage` on a Wayland Plasma 6 session, then on X11: confirm immediate
   effect and no plasmashell restart.
2. Confirm which `FillMode` integer values a KDE 6 build accepts, and that an unknown one is
   rejected (`stretch`, `preserveAspectFit`, `preserveAspectCrop`, `pad` map to 0, 1, 2, 6 in the
   source; `tile` appears in the man page but not in that map).[13]
3. With `plasma-desktop/scripting_console=false` in `kdeglobals`, confirm the failure is detected
   and reported clearly.
4. Two monitors: confirm the same image lands on both, and record what containment indices exist,
   to decide whether per-monitor is worth revisiting.
5. Log out and back in after a set, and confirm the value persisted; then kill plasmashell (no clean
   shutdown) and confirm whether the last change was lost.
6. Set a slideshow via the plugin path and confirm `SlideInterval`/`SlidePaths` are honoured, deciding
   from that whether delegate mode ships.

## sway

### 1. The command, and whether a one-shot process suffices

```
swaymsg output '*' bg /abs/path/image.jpg fill
```

`sway-output(5)`: `output <name> background|bg <file> <mode> [<fallback_color>]`, with mode one of
`stretch`, `fill`, `fit`, `center`, `tile`, plus `solid_color` for a colour.[19] `sway(5)` defines
`swaybg_command`, whose default is `swaybg`.[20]

**One-shot for whirl.** sway itself spawns and manages the background process; the Arch wiki states
that wallpaper in sway is handled by a dedicated program, that swaybg "must be installed ... in order
to run the `output ... bg` command", and that sway can manage it directly.[22] This is the reason
sway is one-shot from whirl's perspective while Hyprland is not: whirl sends one IPC message and
exits, and the process that owns the layer surface is sway's child, not whirl's.[20][22]

swaybg is the surface owner when invoked directly: it is "a wallpaper utility for Wayland
compositors ... compatible with any Wayland compositor which implements the wlr-layer-shell
protocol and `wl_output` version 4".[25]

### 2. Per-monitor support

First-class per output, addressed by name, by `*` for all, or by the make/model/serial string
`swaymsg -t get_outputs` reports.[19] `swaybg` accepts the same idea directly with
`-o, --output <name>`, where the special value `*` selects all outputs, and appearance options given
after `-o` apply only to that output.[21]

### 3. Restart and persistence

An `output ... bg` line in `~/.config/sway/config` is reapplied whenever sway starts, so a
config-file approach survives login.[22] A `swaymsg output ... bg` sent at runtime is an IPC command
and is not written back to the config; the setting is lost when sway restarts.[20][unverified]
whirl therefore has to choose: fight sway's config (write a line, reload) or accept that the
wallpaper is runtime-only and re-apply on the next rotation. Re-applying on rotation is the honest
answer, and the daemon's own persistent state already records which image is current.

### 4. Package and failure mode

`swaybg` is a separate package and a separate project with its own release cadence.[22][25] Missing
it means `output ... bg` has nothing to run; the Arch wiki's phrasing makes the dependency
explicit.[22] Building it needs meson, wayland, wayland-protocols and cairo, with gdk-pixbuf
optional for formats other than PNG.[25]

### 5. Native rotation

None. sway has no timer and no slideshow; the ecosystem compensates with external daemons, listed on
sway's own add-ons wiki page under "Wallpaper": `wpaperd` is described there as a "Wallpaper daemon
for Wayland that can change the wallpaper after a fixed time", alongside `swww`, `mpvpaper` (video)
and `multibg-wayland` (per-output).[24] If whirl wants to defer, `wpaperd` is the thing to defer to;
sway itself offers nothing to defer to.[24]

### Confidence and verification level

| claim | level |
|---|---|
| `output ... bg` is the command, with those modes | documented-and-certain [19] |
| sway spawns and manages swaybg | documented-and-certain [20][22] |
| per-output support and its syntax | documented-and-certain [19][21] |
| config-file persistence, runtime non-persistence | documented-and-certain for the config [22]; non-persistence is inference [unverified] |
| swaybg required and separate | documented-and-certain [22][25] |
| no native rotation; `wpaperd` is the add-on | documented-and-certain [24] |

### What must be tested on real Linux before release

1. `swaymsg output '*' bg <file> fill` on a running sway: confirm the image appears, and confirm the
   swimlane where the process that appears is sway's child (`pgrep -a swaybg`, `ps -o ppid= -p`).
2. Change the image twice in quick succession and confirm the old swaybg is reaped rather than
   accumulating one process per set.
3. Two outputs: set different images per output, then hotplug an output and record what the new one
   shows (the fallback colour path).
4. Restart sway and confirm whether a runtime-set wallpaper survives (expected: no).
5. Delete the image file after setting it and confirm what sway shows.
6. Confirm the `pkill -x swaybg` approach the prototype uses does not fight sway's own management
   (it should be removed in favour of the IPC command).

## Hyprland

### 1. The command, and whether a one-shot process suffices

```
hyprctl hyprpaper wallpaper 'DP-1, /abs/path/image.jpg, cover'
```

hyprpaper is "a fast, IPC-controlled wallpaper utility for Hyprland", and the current wiki lists
exactly two IPC requests: `hyprctl hyprpaper wallpaper '[mon], [path], [fit_mode]'` and
`hyprctl hyprpaper listactive`.[26] The wiki explicitly warns that "older examples may mention
preload, reload, unload, or listloaded; check `hyprctl hyprpaper --help` for the requests supported
by your installed version", which makes those four unsupported-or-legacy rather than documented.[26]
The older 0.52 documentation described `preload`, `wallpaper`, `unload` and `reload` as the request
set.[27]

**Resident required.** The `hyprctl` implementation refuses to work unless both the compositor and
the daemon are present: it returns "can't send: no HYPRLAND_INSTANCE_SIGNATURE (not running under
hyprland)" when the environment variable is unset and "can't send: failed to connect to hyprpaper
(is it running?)" when the daemon is not running.[29] Only `wallpaper` and `listactive` are routed;
anything else is "invalid hyprpaper request".[29] So there is no one-shot way to set a Hyprland
wallpaper: hyprpaper must be running first, and something has to be responsible for it being
running.[26][29]

Hyprland itself does not provide a background surface; hyprpaper is a separate project in the Hypr
ecosystem.[30][unverified]

### 2. Per-monitor support

First-class. The wallpaper is configured per monitor; the monitor may be left empty to act as a
fallback, and the wiki notes the fallback "only applies to monitors that have never had a specific
monitor target assigned".[26] `listactive` prints the active wallpaper per monitor.[26] Fit modes are
`contain`, `cover`, `tile`, `fill`.[26]

### 3. Restart and persistence

Two files matter. The daemon's own config is `~/.config/hypr/hyprpaper.conf`, which is not required
to exist, and it is read when hyprpaper starts.[26] Starting hyprpaper is the session's job:
the wiki says to add it to the compositor's autostart, or, when Hyprland is started under `uwsm`, to
`systemctl --user enable --now hyprpaper.service`.[26] Hyprland's own startup configuration moved to
Lua in recent releases, so the autostart line lives in `hyprland.lua`; on-the-fly `hyprctl` changes
are not saved.[31]

An IPC `wallpaper` call changes the daemon's live state; the wiki documents no write-back to
`hyprpaper.conf`.[26][unverified]

### 4. Package and failure mode

`hyprpaper` and `hyprctl` (shipped with Hyprland) are required.[26][29] The failure messages are
explicit and machine-usable: no `HYPRLAND_INSTANCE_SIGNATURE`, hyprpaper not running, hyprpaper too
old for the protocol, invalid path, invalid monitor.[29] There is no silent no-op path, which is
good for a whirl adapter: every failure mode is a distinguishable string.[29] Building hyprpaper
needs hyprtoolkit, hyprlang, hyprutils and hyprwire development files.[30]

### 5. Native rotation

Yes, inside the daemon. `path` in a `wallpaper {}` block may be "an image file or a directory
containing image files", with `timeout` ("timeout between each wallpaper change, in seconds, if path
is a directory", default 30) and `order` (only supported value `random`), plus `recursive`.[26]
So hyprpaper can rotate a folder by itself with no other process involved.[26]

If whirl ever wanted to hold zero runtime state on Hyprland, writing a directory into
`hyprpaper.conf` would do it.[26] As with KDE's slideshow, that path forfeits whirl's own scheduling,
history and source mixing, so it belongs behind an explicit delegate switch.[unverified]

### Confidence and verification level

| claim | level |
|---|---|
| `hyprctl hyprpaper wallpaper '[mon], [path], [fit]'` is the interface | documented-and-certain [26] |
| resident daemon required; no one-shot path | documented-and-certain [26][29] |
| `preload`/`reload`/`unload` are legacy, not current | documented-and-certain as a warning [26]; their exact status per version is version-dependent [27][29] |
| per-monitor with an empty-monitor fallback | documented-and-certain [26] |
| persistence via `hyprpaper.conf`, autostart is the session's job | documented-and-certain [26][31] |
| IPC changes are not written back to the config | implemented-elsewhere-and-likely [unverified] |
| directory rotation with `timeout`/`order=random` | documented-and-certain [26] |
| Hyprland has no built-in background drawing | unverified-hypothesis [unverified] |

### What must be tested on real Linux before release

1. `hyprctl hyprpaper --help` on a current release: record which requests exist. The prototype uses
   `unload all` and `preload`, which this documentation does not list.[26]
2. Set a wallpaper with hyprpaper not running, and confirm the exact error string, so the adapter can
   distinguish "no Hyprland" from "no hyprpaper".
3. `hyprctl hyprpaper wallpaper ', /path'` with an empty monitor: confirm the fallback behaviour and
   which monitors it does and does not affect after a specific assignment exists.[26]
4. Two monitors, one specific assignment plus a fallback: confirm the documented precedence.
5. Restart the compositor and confirm whether hyprpaper comes back and with which wallpaper.
6. Watch hyprpaper's RSS across ~20 rotations: this is the measurement that decides whether whirl may
   ever supervise it.
7. Confirm whether a directory `path` with `timeout` actually rotates, and what `order = random`
   does with a folder of one file.

## Generic X11 fallback (feh / xwallpaper / hsetroot)

### 1. The commands, and whether a one-shot process suffices

```
feh --bg-fill /abs/path/image.jpg
xwallpaper --zoom /abs/path/image.jpg
hsetroot -fill /abs/path/image.jpg
```

All three paint the X root window (and the pseudo-transparency atoms) and exit. feh's documentation
calls background setting a mode of a viewer: "In many desktop environments, feh can also be used as a
background setter."[32] xwallpaper's manual says it "allows you to set image files as your X
wallpaper" and that "the wallpaper is also advertised to programs which support semi-transparent
backgrounds".[33] hsetroot "allows you to compose wallpapers ('root pixmaps') for X".[34]

**One-shot.** feh and xwallpaper each set the root pixmap and exit; xwallpaper's `--daemon` option
exists specifically to keep the process running to redraw on RandR events, which implies the default
is to exit.[33] hsetroot has no daemon mode at all.[34]

The important caveat is that this is the X root window, and a desktop environment that draws its own
desktop will cover it. feh's own manual states it "does not support setting the wallpaper of GNOME
shell desktops", and points the reader at the GNOME gsettings key instead.[32] Nothing here should be
used when a desktop shell owns the background.[32]

Nothing about the X root window is composited under Wayland: these tools talk to an X server, and in
a Wayland session with XWayland that server's root window is not what the compositor draws.[unverified]

### 2. Per-monitor support

- feh: Xinerama-based. Documented as: "You may even specify more than one file, in that case, the
  first file is set on monitor 0, the second on monitor 1, and so on", with `--xinerama-index` to
  target one screen and `--no-xinerama` to treat the display as one screen.[32] The mapping from
  Xinerama IDs to real outputs is left to the user (`xrandr --listmonitor`).[32]
- xwallpaper: `--output <output>` selects the output; `--no-randr` treats the whole display as one.[33]
- hsetroot: `-root` ignores xrandr outputs and treats multiple displays as one screen; `-screens` sets
  a screenmask.[34]

This is the oldest and most fragile per-monitor model of the five, and it is index-based rather than
name-based.[32]

### 3. Restart and persistence

Nothing persists. feh writes the command line it used into `~/.fehbg` unless `--no-fehbg` is passed,
and the documented way to restore the background is to add `~/.fehbg &` to an X startup script such
as `~/.xinitrc`.[32] That is a user-managed persistence mechanism; whirl would be writing a file the
user owns.[32] xwallpaper and hsetroot write no such file.[33][34]

This means on plain X11 whirl is the only thing that remembers the wallpaper, and it must re-apply on
every session start (the daemon's scheduled rotation does that naturally) or the user must wire it up
themselves.

### 4. Package and failure mode

`feh`, `xwallpaper` or `hsetroot`, any one of which is sufficient. Missing means the executable is
not found and the root window is simply untouched.[32][33][34] There is no daemon whose absence
produces a distinctive message, so the adapter cannot rely on an error string to tell "no tool
installed" from "tool failed".

### 5. Native rotation

None. There is no timer anywhere in any of the three, and no session-level slideshow for plain
X11.[32][33][34]

### Confidence and verification level

| claim | level |
|---|---|
| these three tools set the X root wallpaper and exit | documented-and-certain [32][33][34] |
| feh writes `~/.fehbg`, restored from the user's X startup | documented-and-certain [32] |
| feh does not support GNOME shell desktops | documented-and-certain [32] |
| per-monitor is index/Xinerama-based for feh, `--output` for xwallpaper | documented-and-certain [32][33][34] |
| no native rotation, no persistence | documented-and-certain [32][33][34] |
| useless under a Wayland session | unverified-hypothesis [unverified] |

### What must be tested on real Linux before release

1. On bare X11 with no desktop shell (i3 or a bare `startx`), confirm each of the three sets the
   background and exits, and that the wallpaper survives an X restart only when re-applied.
2. Under GNOME and KDE X11 sessions, confirm the wallpaper is covered or overwritten, documenting the
   refusal rather than shipping a half-working path.
3. Two X monitors: confirm feh's file-order-to-monitor mapping and xwallpaper's `--output` naming.
4. Change the image and confirm no accumulation of processes.
5. Under XWayland in a Wayland session, confirm nothing happens, and make the adapter refuse this case.

## The resident helper: minimum footprint, and who supervises it

Only two environments need resident helpers, and only one of them needs whirl to care.

| helper | package size | installed | notes |
|---|---|---|---|
| swaybg | 15.3 KB | 33.6 KB | Arch package metadata [37]; deps cairo, wayland, optional gdk-pixbuf for non-PNG [25] |
| hyprpaper | 160.1 KB | 478.8 KB | Arch package metadata [38]; deps hyprtoolkit, hyprlang, hyprutils, hyprwire [30] |

Those are package figures, not runtime figures: neither helper is a walled garden with a toolkit,
an index or a scheduler inside, which is the property the project cares about. Their decoded image
plus per-output buffers is the real cost, and that is not measured here; the table above is the only
number this document can honestly give.[unverified]

Who supervises whom:

- **sway: hand off.** whirl sends `swaymsg output '*' bg <file> fill` and exits; sway spawns and
  manages its own background process, which is what `swaybg_command` is for.[20][22] whirl should
  never run swaybg itself and should never `pkill` it, because the process belongs to sway.
- **Hyprland: hand off to the session.** hyprpaper is the user's autostart unit (or
  `hyprpaper.service` under uwsm); whirl talks to it over IPC and must not start, own or restart it
  unless it also accepts owning a resident process.[26] If whirl were to supervise hyprpaper it would
  inherit a resident footprint plus a restart policy and a conflict with the user's own autostart,
  which is exactly the architectural mistake the project exists to avoid. The recommendation is to
  depend on it: detect it, use it, fail clearly if it is not running.[29]

So the answer to "does whirl supervise it or hand off to it" is: hand off, in both cases, and treat
"the helper is not running" as a diagnosable error rather than a condition to repair.[20][26][29]

## Detecting the environment (what the sniff has to key on)

The decisive signals are the ones a compositor sets for its own children, not the generic ones:

| signal | source | use |
|---|---|---|
| `XDG_CURRENT_DESKTOP` | set by the login manager from the session file's `DesktopNames`; colon-separated list [35][36] | GNOME vs KDE; `Hyprland` is informally recognised [36] |
| `XDG_SESSION_TYPE` | `wayland` or `x11` | Wayland vs X11, to refuse the X11 setters under Wayland [unverified] |
| `SWAYSOCK` (and `I3SOCK`) | sway's IPC socket path, documented in `sway-ipc(7)` [23] | unambiguous "this is a sway session" |
| `HYPRLAND_INSTANCE_SIGNATURE` | used by hyprctl, which errors when it is unset [28][29] | unambiguous "this is a Hyprland session" |

`gsettings` being on `PATH` is not a GNOME signal, and `XDG_CURRENT_DESKTOP` must be matched as a
colon-separated list rather than by equality.[35][36] The compositor-specific variables are the ones
that cannot be set by accident.[23][29]

## What the prototype's Linux backend gets wrong

Documentation only; `prototype/` is untouched by this card. The findings below are what the matrix
implies for `prototype/wh-rotate/set_linux.go`:

1. `have("gsettings") && XDG_CURRENT_DESKTOP != "KDE"` treats "gsettings exists" as "GNOME". On any
   GLib-equipped non-GNOME desktop that branch is taken and the schema may not exist at all.[1]
2. It sets only `picture-uri`, never `picture-uri-dark`, so a dark-mode GNOME user sees no change.[2]
3. It `pkill -x swaybg`s and then starts swaybg itself, which fights the process sway owns, and it
   gets no fallback colour, no `solid_color` and no per-output addressing that the IPC command
   provides.[19][22]
4. Its hyprpaper branch uses `unload all` and `preload`, which the current documentation does not
   list among supported requests, and it never checks that hyprpaper is running.[26][29]
5. Its `feh` branch is reachable on Wayland, where the X root window is not what is drawn.[unverified]
6. It has no per-monitor concept anywhere, which is correct for GNOME and wrong for everything
   else.[2][19][26][32]

## Recommendation for v0.1

**Support, all one-shot from whirl's side:**

1. **GNOME (Wayland).** Largest single user base (Fedora Workstation, Ubuntu), one command, no helper,
   no package, and the deepest documentation of any target here.[1][2] Set both the light and dark
   keys.[2]
2. **KDE Plasma 6 (Wayland).** The other large base, same one-shot shape through DBus, and the only
   target with a documented native rotation to defer to.[13][16]
3. **sway.** Small user base, but the cheapest correct implementation in the whole matrix: one IPC
   message, the compositor owns the helper, and provenance is unambiguous via `SWAYSOCK`.[19][20][23]
4. **generic X11 (feh or xwallpaper).** Not for desktops, but for the bare-Window-Manager case, it is
   one command each and it makes i3/openbox/bspwm users reachable for free.[32][33]

**Deliberately deferred:**

- **Hyprland.** The only target where a one-shot process cannot do the job, and the only one that
  would force whirl to own a resident helper or to depend on a third-party daemon whose IPC surface
  changed within the past year (`preload`/`reload`/`unload` are now legacy). It is a good v0.2 target
  once the architecture has decided whether it will ever supervise a process.[26][29]
- **GNOME on X11.** Being removed: the X11 session is disabled by default at compile time from GNOME
  49, Fedora dropped the GNOME X11 packages, and upstream targeted removal for GNOME 50. There is no
  point testing a path that is going away, and no user base left in a year.[9][10][11]
- **Per-monitor wallpaper on GNOME.** Not possible; a single image is applied to all monitors and
  only `.xml` animations can differ per monitor. Say so rather than half-supporting it.[2][8]
- **Deferring rotation to the DE's own slideshow.** Tempting on KDE (`org.kde.slideshow`) and Hyprland
  (`timeout`/`order`), but it hands scheduling away and forfeits whirl's history and source mixing.
  Offer it as an explicit delegate mode later, not as the default.[16][26]
- **GNOME's `.xml` slideshow as a rotation format.** Undocumented; a silently rejected file leaves
  the user with no wallpaper and no error.[7][12]

The shape that falls out of this is that whirl's Linux backend has two setters (a GSettings adapter
and a DBus adapter), one compositor-IPC adapter for sway, one plain-X11 adapter, and no process
supervision on any supported target. That is the same "one lightweight approach" the project assumes,
with the single documented exception of Hyprland.

## Sources

[1] https://raw.githubusercontent.com/GNOME/gsettings-desktop-schemas/master/schemas/org.gnome.desktop.background.gschema.xml.in
[2] https://gitlab.gnome.org/GNOME/gnome-shell/-/raw/main/js/ui/background.js
[3] https://help.gnome.org/system-admin-guide/desktop-background.html
[4] https://help.gnome.org/system-admin-guide/backgrounds-extra.html
[5] https://wiki.gnome.org/Projects/dconf/SystemAdministrators
[6] https://help.gnome.org/system-admin-guide/dconf-profiles.html
[7] https://gitlab.gnome.org/GNOME/gnome-desktop/-/raw/master/libgnome-desktop/gnome-bg-slide-show.c
[8] https://raw.githubusercontent.com/laverdone/gnome-shell/per-monitor-backgroud/per-monitor-background.md
[9] https://blogs.gnome.org/alatiera/2025/06/08/the-x11-session-removal
[10] https://github.com/GNOME/gnome-session/blob/main/NEWS
[11] https://fedoraproject.org/wiki/Changes/WaylandOnlyGNOME
[12] https://askubuntu.com/questions/945953/gsettings-set-org-gnome-desktop-background-parameter-specifications
[13] https://invent.kde.org/plasma/plasma-workspace/-/raw/master/wallpapers/image/plasma-apply-wallpaperimage.cpp
[14] https://develop.kde.org/docs/plasma/scripting
[15] https://phabricator.kde.org/D14185
[16] https://raw.githubusercontent.com/KDE/plasma-workspace/master/wallpapers/image/slideshowpackage/contents/config/main.xml
[17] https://raw.githubusercontent.com/KDE/plasma-workspace/master/wallpapers/image/imagepackage/contents/ui/main.qml
[18] https://wiki.archlinux.org/title/KDE
[19] https://raw.githubusercontent.com/swaywm/sway/master/sway/sway-output.5.scd
[20] https://raw.githubusercontent.com/swaywm/sway/master/sway/sway.5.scd
[21] https://man.archlinux.org/man/swaybg.1.en
[22] https://wiki.archlinux.org/title/Sway
[23] https://man.archlinux.org/man/sway-ipc.7.en
[24] https://raw.githubusercontent.com/wiki/swaywm/sway/Useful-add-ons-for-sway.md
[25] https://raw.githubusercontent.com/swaywm/swaybg/master/README.md
[26] https://wiki.hypr.land/Hypr-Ecosystem/hyprpaper
[27] https://wiki.hypr.land/0.52.0/Hypr-Ecosystem/hyprpaper
[28] https://wiki.hypr.land/IPC
[29] https://raw.githubusercontent.com/hyprwm/Hyprland/main/hyprctl/src/hyprpaper/Hyprpaper.cpp
[30] https://github.com/hyprwm/hyprpaper
[31] https://wiki.archlinux.org/title/Hyprland
[32] https://manpages.ubuntu.com/manpages/noble/man1/feh.1.html
[33] https://manpages.debian.org/testing/xwallpaper/xwallpaper.1.en.html
[34] https://manpages.debian.org/testing/hsetroot/hsetroot.1.en.html
[35] https://specifications.freedesktop.org/desktop-entry/latest/recognized-keys.html
[36] https://wiki.archlinux.org/title/XDG_CURRENT_DESKTOP
[37] https://archlinux.org/packages/extra/x86_64/swaybg
[38] https://archlinux.org/packages/extra/x86_64/hyprpaper
