# Frontends: what shipping wallpaper and tray apps put in the menu, and the v1 option set for whirl

Research note for `guruor/whirl`. Documentation only, no product code changed.

**Why this exists.** `README.md` promises that "the protocol is the API; anything that speaks it is
a valid frontend" and that a menu bar item is an optional, thin client (`docs/architecture.md`
section 8, Frontend contract; section 3.5, The optional lightweight UI). A reference frontend is
being planned: a tray or menu bar item plus a small settings window. This note supplies the option
set: which controls earn a place, and what the shipping applications in this space already do with
them. It is the input to the frontend spec and design cards, not a decision about them.

**Verification reality.** Nothing here was installed or run. This was written on macOS 26.5.2 from
public sources only, and the constraints of the task forbid creating an account, obtaining an API
key, or typing a credential. So there is no "verified on this machine" grade in this file: every
claim about an application's behaviour is **cited** from the project's own page or documentation,
its source code, the platform's app listing, or a dated review, and each app note names the source
it rests on. Where a field could not be found in a source, it is written **unverified**, never
guessed. Two exceptions are graded as **inference** and labelled: the menu bar versus settings
split of an app whose menu layout no source enumerates, and the per-display rows where a source
describes the capability without saying which display it applies to.

The Stack question (runtime, toolkit, packaging) is out of scope here and lives in its own card;
no recommendation below names a toolkit, and none should be read as one.

## The set, and why this set

Ten shipping applications, which is the card's upper bound. The candidates the card named were
kept where a primary source exists and replaced only where it does not.

| # | Application | Platform | Source that it ships | Why in |
|---|---|---|---|---|
| 1 | Plash | macOS | `sindresorhus.com/plash`, Mac App Store app 1494023538 [1][2] | Menu bar app, open source, a real preference surface |
| 2 | Unsplash Wallpapers (official) | macOS | Mac App Store app 1284863847 [6] | The reference "curated source" frontend; window + toolbar, not a menu bar item |
| 3 | Irvue | macOS | Mac App Store app 1039633667 [8] | The most menu-bar-centric macOS example; every control lives in the status item |
| 4 | Bing Wallpaper (Microsoft) | Windows, macOS | Microsoft Download Center; Homebrew cask `bing-wallpaper` [11][14] | First-party daily-image app; the platform vendor's own shape |
| 5 | Wallpaper Engine | Windows | Steam app 431960, vendor help site [16]-[20] | The heavy end: playlists, per-display profiles, tray, CLI |
| 6 | John's Background Switcher | Windows, macOS | `johnsad.ventures/backgroundswitcher` [21] | The "many sources, credential per source" example |
| 7 | Dynamic Theme | Windows | `apps.pinnula.ca`, Microsoft Store 9nblggh1zbkw [28][29] | The auto-updating UWP shape: no tray, history, pause-for-a-while |
| 8 | Variety | Linux | `github.com/varietywalls/variety`, Ubuntu manpage [32][33] | The tray-plus-preferences reference on Linux |
| 9 | HydraPaper | Linux | `hydrapaper.gabmus.org`, Debian package, manpage [36][37][38] | Per-monitor control as the whole product |
| 10 | Komorebi | Linux | `github.com/Komorebi-Fork/komorebi` [39] | The desktop-menu (no tray) shape, and animated wallpapers |

### Candidates named by the card that were not covered, and why

- **Wallpaper Wizard 2 (MacPaw)** — no longer shipping. MacPaw announced its sunset on 23 February
  2026 and removed it on 23 March 2026: "Wallpaper Wizard 2 is no longer available for purchase,
  and will be sunset on March 23, 2026. You won't be able to access the app or any legacy
  versions." [40] A dead product cannot inform a v1 option set; recorded here so the next reader
  does not re-open the question.
- **Wallch (Linux)** — shipping in name only. Its own repository documents the current state as a
  rewrite whose headline features are non-functional: "Live Earth — ❌ Non-functional", "Wikipedia
  Picture of the Day — ❌ Non-functional", "Live Website — ❌ Non-functional", "Wallpaper Clocks —
  ❌ Non-functional", and local-image change is "Partially works (not on all platforms yet)" [41].
  Komorebi and HydraPaper cover the Linux tray/desktop ground with sources that describe working
  behaviour.

## Per-application notes

Each note answers the five fields the card asks for: the tray/menu bar entries and how "act now"
is separated from settings; where settings live; how launch at login is presented (and whether it
covers the background piece, the UI, or both); how a credential is stored if a remote source needs
one; and footprint, only where a source states it.

### 1. Plash (macOS)

| Field | Finding | Source |
|---|---|---|
| Menu bar item | The app is a menu bar item; clicking it is where you add a website and switch between the websites you added ("add multiple websites and easily switch between them"). The website itself is shown with Browsing Mode on or off. | [1], [2], [5] |
| Act now vs settings | Not enumerated as a menu by any source. The features list separates the live controls (switch website, browsing mode, reload) from preferences (opacity, reload interval, per-display). The switch-to-a-specific-website action is a Shortcuts action, not necessarily a menu row. | [1], [2] |
| Settings location | A preferences window, implemented as a SwiftUI `PreferencesView`: launch at login toggle, deactivate on battery, show on all Spaces, invert colours, opacity, reload interval, display, keyboard shortcuts, custom CSS, clear website data. | [3] |
| Launch at login | A `LaunchAtLogin.Toggle()` row in that preferences window. It covers the app itself; there is no separate background service. | [3], [4] |
| Credential | None. Plash shows a website; any site login is handled by the website in its own web view, not by a Plash credential store. No source describes a Plash-held token. | [1], [2] |
| Footprint | **Unverified** — no size stated on the project page or in the App Store text retrieved. | — |

Note: the repository's file layout has moved since the preferences source quoted above was written;
the citation is pinned to the commit that contains it. The reload interval in that source defaults
to 60 s and has a floor of 0.1 min (6 s); it is not a fixed menu of choices [3].

### 2. Unsplash Wallpapers, official (macOS)

| Field | Finding | Source |
|---|---|---|
| Menu bar item | None documented. The action is a toolbar button: "Click the Unsplash button on the toolbar — Pick a photo — Ba-boom! Fresh wallpaper." This is a windowed app, unlike the menu-bar apps beside it. | [6] |
| Act now vs settings | The app deliberately minimises settings ("No fidgeting around with the settings. We've made it dead simple"). The act is picking a photo or a theme; the settings are the automatic-change cadence (daily or weekly) and the download folder. | [6] |
| Settings location | An in-app window (the app's own UI), plus a setting for the download folder added in version 2025.02.1. | [6] |
| Launch at login | **Unverified** — no source states a launch-at-login control. | — |
| Credential | None visible to the user: the app ships with Unsplash's own access. A third-party reimplementation describes the alternative (paste your own Unsplash access key, stored locally in the app's user data directory), which is explicitly *not* how the official app works. | [6] |
| Footprint | 1.4 MB, as listed on the Mac App Store listing (version 2025.02.1). | [6] |

Themes are the source/collection control: the app added "a selection of curated themes, and you can
even create your own" [7]. One third-party app claims the official app "doesn't allow you to create
custom collections tailored to your preferences" and offers that as its differentiator [42]. That is
a competitor's description of a rival and is recorded here as such, not as a finding about the
official app.

### 3. Irvue (macOS)

| Field | Finding | Source |
|---|---|---|
| Menu bar item | Every control lives in the status item. A dated review of the app: "The Irvue user interface is represented by a simple status bar menu that provides access to all available controls and customization options", including "rapidly load the next wallpaper in queue" and "quick information about the current wallpaper". | [9] |
| Act now vs settings | Both in the one status menu; no separate window is documented. The menu acts (next, download, open author page, disable the scheduler) and configures (interval, channels, multi-display, hide-list). **Inference**, from the review's description of a single menu holding both. | [9] |
| Settings location | The status menu itself, described as holding "all available controls and customization options". AppleScript support is exposed for automation. | [9], [10] |
| Launch at login | **Unverified** — no source found states a start-at-login control or a login item. | — |
| Credential | No credential is required to rotate. If the user has an Unsplash account, the app can like photos and add them to collections; how that token is stored is **unverified** — no source describes the store. | [8] |
| Footprint | 8.7 MB (Mac App Store-adjacent listing, version 2026.3); the App Store listing text has also been reported at 5.3 MB for an earlier build. Both are store-stated, neither measured here. | [9], [8] |

### 4. Bing Wallpaper, Microsoft (Windows 10+, plus a macOS build)

| Field | Finding | Source |
|---|---|---|
| Tray item | Microsoft's install steps end at "Explore the Bing Wallpaper app in the system tray"; the app lives in the tray and the daily image is the wallpaper. | [11][12] |
| Act now vs settings | On macOS a Microsoft answer states there are "two arrows, allowing you to choose the wallpaper" in the status bar; on Windows the app browses images and themes from the tray/UI. The distinction between a live action and a settings surface is not enumerated by a source. | [15], [11] |
| Settings location | The app is the settings surface (themes, region); no separate file or preference pane is documented. | [11][12] |
| Launch at login | **Unverified** — no source names a launch-at-login control, though a daily-update app of this kind normally has one. Not claimed. | — |
| Credential | None required: images come from the Bing homepage feed with no user account. Microsoft Rewards is offered separately and is not a source credential. | [11] |
| Footprint | 218.5 MB, the Windows installer `BingWallpaperInstaller.exe` (version 1.1.460.0, published 24/08/2026) as stated on the Microsoft Download Center. The installer bundles the Bing search components, so this is not the wallpaper engine's own footprint alone. | [11] |

Platform note: the current marketing page states "The Bing Wallpaper app is compatible with Windows
10 and above only at this time" [12], yet a macOS build ships as a `.pkg` from `download.microsoft.com`
and is distributed through the Homebrew cask `bing-wallpaper` (version 1.1.8, `depends_on macos: ">=
:big_sur"`, `requires_rosetta`) [14]. The macOS product page (`bingwallpaper.microsoft.com/mac/...`)
returned HTTP 500 when retrieved for this note, so the cask is the artifact cited for the macOS
build's existence, not the marketing site.

### 5. Wallpaper Engine (Windows)

| Field | Finding | Source |
|---|---|---|
| Tray item | A tray icon beside the clock. The vendor documents that it can be hidden with the registry value `hideTrayIcon = 1`, and warns that with the icon hidden "the only way to turn off Wallpaper Engine will be to kill it through the Windows Task Manager", i.e. Quit normally lives in the tray menu. | [16] |
| Act now vs settings | The tray menu carries the immediate actions; a Steam discussion records "right click the tray icon and choosing pause", and the vendor's own note says to turn the app off by right-clicking the tray icon and selecting "Quit". The full settings are a separate window opened from the app. | [20], [19] |
| Settings location | The application's own settings window ("General" tab and others), not a preference pane or file. | [18], [19] |
| Launch at login | In the settings, "General" tab, an automatic-startup toggle; the vendor documents two modes, normal and high-priority, where high-priority "will register a Windows service" so it starts before other startup programs. This covers the background piece and the UI, which are the same process. | [18] |
| Credential | None: wallpapers come from the Steam Workshop under the Steam account, not a credential held by the app. | [17] |
| Footprint | **Unverified** — no project-stated or independently measured size was found. | — |

### 6. John's Background Switcher (Windows, macOS)

| Field | Finding | Source |
|---|---|---|
| Tray item | "JBS sits in your system tray (down by the clock)". The notification icon "lets you open the settings, pause switching, view all downloaded photos ... email or save backgrounds, check for updates and exit JBS". | [21] |
| Act now vs settings | The tray icon is explicitly the split: act-now entries (pause, cached-picture browser, save/email a background) sit in the menu, and "open the settings" opens a modal settings dialog reached from the same menu. | [21] |
| Settings location | A settings dialog plus a "More Settings" dialog, with an Import/Export section backed by a file. | [21], [25] |
| Launch at login | A dated review instructs "You'll want JBS to Start automatically when starting Windows", with an alternative "On start-up change the background then exit" [26]. An installer analysis of the older 4.x line records an HKCU Run entry, `BackgroundSwitcher.exe /next` [27]. Covers the running app; no separate background service is documented. | [26], [27] |
| Credential | Tokens for remote sources are held in the Windows **Credential Locker**: a release note explains Google Photos was removed from "Windows 7 and below as they don't support Credential Locker" [22]. Unsplash required an account after a change, authenticated "from the Authorise button on Settings or when you open an Unsplash photo set" [23]. The credential is therefore collected by an in-app Authorise action and stored in the OS credential store, not in a JBS file. | [22], [23] |
| Footprint | 2.3 MB installer for version 4.7, per an installer inventory [27]. A current third-party listing states 31.14 MB for version 6.1, which is an installer size read from a download site rather than a project figure; both are reported, neither measured here. | [27] |

Next/previous note: JBS exposes `/next` on the command line and added using it against an already
running instance in 5.8 [24]. A next control in the tray menu itself is not stated by a source.

### 7. Dynamic Theme (Windows)

| Field | Finding | Source |
|---|---|---|
| Tray item | No tray icon is documented. This is a UWP app whose visible surface is its own window plus a live tile; the background work runs as a UWP background task. It is included because the card named it, and the finding is that its shape is the app-window shape, not the tray shape. | [28], [29] |
| Act now vs settings | The app window is where both happen: viewing recent Bing/Spotlight images, saving one, setting it as background or lock screen, and changing settings. | [28], [30] |
| Settings location | In-app pages (Background, Lock screen, Daily Bing picture, Daily Windows Spotlight picture, etc.), plus a UWP background-task permission that lives in Windows Settings > Apps. | [28], [29] |
| Launch at login | The update runs as a **UWP background task**, which Windows schedules; the FAQ documents that Windows must be allowed to run the app in the background and how to fix it when it is not. This is the background piece; there is no separate UI process to launch. | [29] |
| Credential | None for Bing or Spotlight. Synchronising settings across devices "requires a Microsoft account", which is the OS account, not a credential the app stores. | [28] |
| Footprint | **Unverified** — the Microsoft Store listing gives no size in the text retrieved. | [28] |

### 8. Variety (Linux)

| Field | Finding | Source |
|---|---|---|
| Tray item | "Where supported, Variety sits as a tray icon to allow easy pausing and resuming. Otherwise, its desktop entry menu provides a similar set of options." | [32] |
| Act now vs settings | The command-line interface enumerates the actions one per row, which is the closest thing to an enumerated menu: `--next`, `--previous`, `--trash`, `--favorite`, `--pause`/`--resume`/`--toggle-pause`, `--history`, `--downloads`, `--selector`, and separately `--preferences`/`--show-preferences`. So "act now" and "settings" are separate top-level commands, and the tray's Preferences row is the settings entry. | [33] |
| Settings location | A Preferences dialog with tabs (General, Effects, Slideshow, Manual downloading, Color and size, Customize, Tips), backed by `~/.config/variety/variety.conf`. | [33], [35] |
| Launch at login | A Preferences checkbox, "Start Variety when the computer starts", which writes a `~/.config/autostart/variety.desktop` entry whose Exec is `variety --profile ...` (with a 20 s delay in current builds). Covers the app, which is also the background process. | [34], [35] |
| Credential | None held by Variety for its bundled sources; the first-run privacy notice tells the user that images downloaded from Unsplash are tracked and offers to disable Unsplash. No key is obtained or stored. | [35] |
| Footprint | One independent walkthrough observed about 111.6 MB of memory with several online sources enabled and frequent downloads, and notes both memory and disk grow with more sources (measurement dated 2020, version not stated) [35]. No project-stated figure. | [35] |

### 9. HydraPaper (Linux)

| Field | Finding | Source |
|---|---|---|
| Tray item | None. HydraPaper is a GNOME-oriented window app; the site's "fresh wallpapers in seconds" path is the app launcher entry, not a tray. | [36] |
| Act now vs settings | Both in the window: the wallpaper picker, favorites, folders-as-collections, mode (zoom / center-black / center-blur / fit), and a random button. | [36], [37] |
| Settings location | The app window. Everything is also reachable by CLI (`-c/--cli`, `-m/--modes`, `-r/--random`, `-l/--lockscreen`), which the project markets as "Most of what the GUI allows you to do, you can do from the command line." | [37], [36] |
| Launch at login | **Unverified / not present** — no source describes a launch-at-login control; the app has no rotation daemon to start. | — |
| Credential | None; sources are local folders only. | [36] |
| Footprint | **Unverified** — no project or independent size statement found. | — |

### 10. Komorebi (Linux)

| Field | Finding | Source |
|---|---|---|
| Tray item | None. Wallpaper selection and desktop preferences live in a "bubble menu" opened by right-clicking the desktop. | [39] |
| Act now vs settings | Not split into a menu versus a settings surface; both live in that bubble menu, which includes the wallpaper selector and desktop preferences. | [39] |
| Settings location | The desktop bubble menu; wallpapers are created by a separate "Wallpaper Creator" tool. Wallpapers are images, videos, or web pages. | [39] |
| Launch at login | **Unverified** — no source describes an autostart control; the README instead relies on launching `komorebi` manually or from the DE. | [39] |
| Credential | None. | [39] |
| Footprint | **Unverified** — no project or independent size statement found. | — |

## Count table

Counts are over the ten applications above (m = 10) and count only the apps for which a source
**states** the behaviour. "Unverified" apps are excluded from the numerator, so a low count means
"not found in a source", not "the app cannot do it". The applications column names the numerator so
the table and the notes cannot disagree.

| Behaviour | n of 10 | Applications that state it |
|---|---|---|
| Next / previous | 6 | Plash, Irvue, Bing Wallpaper, Wallpaper Engine, John's Background Switcher, Variety |
| Pause or stop | 6 | Irvue, Wallpaper Engine, John's Background Switcher, Dynamic Theme, Variety, Komorebi |
| Pick a source or collection | 10 | all ten |
| Refresh interval | 8 | Plash, Unsplash Wallpapers, Irvue, Bing Wallpaper, Wallpaper Engine, John's Background Switcher, Dynamic Theme, Variety |
| Launch at login | 5 | Plash, Wallpaper Engine, John's Background Switcher, Dynamic Theme, Variety |
| History | 4 | Bing Wallpaper, John's Background Switcher, Dynamic Theme, Variety |
| Favourite or pin | 3 | Irvue, Variety, HydraPaper |
| Show the current image | 10 | all ten |
| Reveal the file | 3 | Wallpaper Engine, John's Background Switcher, Variety |
| Quit versus hide | 3 | Plash (hide menu bar icon), Wallpaper Engine (hide tray icon), Variety (indicator icon configurable) |
| Per-display control | 4 | Irvue, Wallpaper Engine, John's Background Switcher, HydraPaper |

Reading of the table, stated as inference: the two near-universals are the abilities that define
the category at all — choosing a source, and knowing what is currently on screen (10 of 10). The
next tier (6 to 8 of 10) is movement and cadence: next/previous, pause/stop, refresh interval.
The rare tier (3 to 5 of 10) is the state a wallpaper app has to keep: history, favourites,
launch at login, reveal-the-file, quit-versus-hide, per-display. A v1 that ships the near-universals
plus launch at login and history is at parity with most of the field; per-display and reveal-the-file
are differentiators, not table stakes.

## Recommended v1 option set for whirl

Whirl's daemon owns rotation and stores no pixels (`README.md`; `docs/architecture.md` section 9),
and the frontend is an ordinary client of the protocol (section 8). The recommendation below is
shaped by that: every row is a control that reads or writes daemon state through the protocol, and
nothing that would put image work or a second scheduler in the frontend.

### Tray menu

| Control | Behaviour | Rows in the field that support it |
|---|---|---|
| Next wallpaper | Advance the rotation queue now. | next/previous in 6 of 10 |
| Previous wallpaper | Step back one in the rotation, using the daemon's history, not a re-download. | same row |
| Pause rotation / Resume rotation | One row whose label reflects current state; maps to a daemon pause flag. | 6 of 10 offer pause |
| Source submenu | List the configured sources and collections; picking one makes it the active source. | 10 of 10 pick a source |
| Show current wallpaper | Open a small popover: source, collection, author or origin, and the file path of the current image. | 10 of 10 show the current image |
| Reveal current file | Open the OS file manager at the current image. | 3 of 10 reveal the file |
| History submenu | Last N rotations, newest first; picking one sets it as current. | 4 of 10 keep history |
| Favourite / Unfavourite current | Toggle a pin on the current image into a favourites collection. | 3 of 10 favourite |
| Settings… | Open the settings window. This is the act/settings seam, matching JBS and Variety. | JBS, Variety |
| Quit frontend | Quit the menu bar item only; it does not stop the daemon (see omitted table). | Wallpaper Engine's Quit, JBS's Exit |

Note on the seam: JBS and Variety both put "open settings" as one row inside the same menu that
holds the actions [21], [33], and Wallpaper Engine keeps actions in the tray and a separate settings
window [20], [19]. The recommendation follows that shared shape: one tray menu, with Settings… as
the single row that crosses into the window.

### Settings window

| Setting | Shape | Rows in the field that support it |
|---|---|---|
| Sources | Add/remove local folders; enable/disable remote sources; each remote source shows credential state by name only. | 10 of 10 |
| Rotation interval | A numeric value plus a unit, with a sensible floor (Plash's floor is 6 s; Variety's menu spans minutes to hours). | 8 of 10 |
| Rotation order | Sequential vs shuffle. | Irvue (randomise), HydraPaper (random), Wallpaper Engine (playlist order) |
| Launch at login | One toggle for the whole product. | 5 of 10 |
| History size | How many past rotations to keep for the History submenu. | 4 of 10 keep history |
| Favourites collection | The folder or collection favourites are written to. | Irvue, Variety, HydraPaper |
| Cache / download folder | Where fetched images are stored. | Unsplash Wallpapers (download folder), JBS (Favourites/downloads) |
| Credentials | A list of source credentials by name and state; "add" and "remove" only, and the value itself is never displayed or entered in the frontend. | JBS stores tokens in the OS credential store [22] |
| Notify on rotation | On/off, for a "new wallpaper" notice. | Dynamic Theme (alert on new image), Irvue (notifications) |

The credential row is deliberately the only credential surface. `docs/architecture.md` section 6.3
says the config stores credentials by reference to the platform's own store, and the contributing
rules forbid committing one; a frontend that displayed or held the value would contradict both.

### Deliberately omitted in v1, with the reason

| Omitted | Reason |
|---|---|
| Stop or restart the daemon | Daemon lifecycle belongs to the OS service manager, not the UI. A frontend able to leave the daemon stopped is a support burden with no upside; Wallpaper Engine's own docs warn that hiding its tray icon leaves Task Manager as the only way to stop it [16]. |
| In-app OAuth / web sign-in | The credential belongs in the platform store by reference (architecture 6.3). Shipping a web login in the frontend is the shortest path to the frontend becoming a second credential store. JBS's in-app Authorise button is the precedent, and it is also the precedent for the wrong kind of coupling [22][23]. |
| Per-display different images | Every source that has it treats it as a headline feature (HydraPaper is built around it; Irvue, Wallpaper Engine and JBS all support it), so it will be wanted. It is omitted from v1 only because it multiplies the rotation state machine, and the daemon's advertised model is one queue. Revisit once the daemon exposes a display target in the protocol. |
| Image effects (sepia, borders, montages, calendars, clocks, quotes) | JBS and Variety package these; the daemon never owns pixels (architecture 9), so effects are a separate product surface, not a frontend control. |
| Live content wallpapers (live website, live Earth, Wikipedia picture of the day) | Plash, Wallch and Variety each carry a live-content mode. Whirl rotates images from sources; a live feed is a different daemon feature and a different frontend rendering problem. |
| Per-image blacklist / "never show again" | Variety's trash and Irvue's hide-list exist, but favourites plus history cover the common intent, and blacklisting is daemon-side filtering that the daemon does not advertise yet. |
| Auto-update control | A packaging concern. Every Windows example either bundles an updater (Bing Wallpaper) or relies on a store (Dynamic Theme, Wallpaper Engine via Steam); it is not a rotation control and does not belong in the rotation UI. |
| A second "keep running in background" setting separate from launch at login | One toggle is the field norm (5 of 10 have exactly one). A second switch invites the state where the UI is running and the daemon is not. |
| OS accent/theme sync (macOS appearance, Windows accent colour) | Irvue adjusts the macOS theme to the wallpaper; it is a nice-to-have that depends on platform APIs, and it is not a rotation control. |

## Sources

[1] https://sindresorhus.com/plash
[2] https://apps.apple.com/us/app/plash/id1494023538
[3] https://raw.githubusercontent.com/sindresorhus/Plash/b348a62645a873abba8dc11ff0fb8fe423419411/Plash/PreferencesView.swift
[4] https://github.com/swiftbar/LaunchAtLogin
[5] https://9to5mac.com/2020/01/09/plash/
[6] https://apps.apple.com/us/app/unsplash-wallpapers/id1284863847
[7] https://unsplash.com/blog/our-wallpaper-app-gets-its-most-requested-feature/
[8] https://apps.apple.com/us/app/irvue-desktop-wallpapers/id1039633667
[9] https://mac.softpedia.com/get/Wallpapers/Unsplash-Wallpaper.shtml
[10] https://irvue.tumblr.com/apple-script-support
[11] https://www.microsoft.com/en-us/download/details.aspx?id=101202
[12] https://bingwallpaper.microsoft.com/
[13] https://bingwallpaper.microsoft.com/mac/en/bing/bing-wallpaper
[14] https://github.com/Homebrew/homebrew-cask/blob/47817d96ba845ccedf309ed3f1e820076cecd0ba/Casks/b/bing-wallpaper.rb
[15] https://learn.microsoft.com/en-us/answers/questions/2349135/how-does-the-bing-wallpaper-app-for-mac-work-i-onl
[16] https://help.wallpaperengine.io/en/functionality/tray.html
[17] https://help.wallpaperengine.io/en/functionality/cli.html
[18] https://help.wallpaperengine.io/en/functionality/automaticstartup.html
[19] https://help.wallpaperengine.io/en/general/bits.html
[20] https://steamcommunity.com/app/431960/discussions/2/1697169163417854473/
[21] https://johnsad.ventures/software/backgroundswitcher/windows/
[22] https://johnsad.ventures/software/backgroundswitcher/johns-background-switcher-5-7-release-notes/
[23] https://johnsad.ventures/software/backgroundswitcher/johns-background-switcher-5-9-release-notes/
[24] https://johnsad.ventures/software/backgroundswitcher/johns-background-switcher-5-8-release-notes/
[25] https://johnsad.ventures/software/backgroundswitcher/windows/johns-background-switcher-frequently-asked-questions/
[26] https://www.makeuseof.com/tag/dynamic-desktop-wallpaper/
[27] https://www.shouldiremoveit.com/Johns-Background-Switcher-78296-program.aspx
[28] https://apps.pinnula.ca/en/dynamic-theme/9bghzk
[29] https://apps.pinnula.ca/en/dynamic-theme/9bghzk/faq
[30] https://www.makeuseof.com/how-to-get-best-bing-windows-spotlight-wallpapers-with-dynamic-theme/
[31] https://www.thewindowsclub.com/dynamic-theme-app-windowws-10
[32] https://github.com/varietywalls/variety
[33] https://manpages.ubuntu.com/manpages/focal/man1/variety.1.html
[34] https://github.com/NixOS/nixpkgs/issues/402093
[35] https://learnubuntumate.weebly.com/variety-wallpaper.html
[36] https://hydrapaper.gabmus.org/
[37] https://manpages.ubuntu.com/manpages/jammy/man1/hydrapaper.1.html
[38] https://packages.debian.org/testing/graphics/hydrapaper
[39] https://github.com/Komorebi-Fork/komorebi
[40] https://macpaw.com/news/wallpaper-wizard-sunset
[41] https://github.com/LeonVitanos/Wallch
[42] https://github.com/cweihung/Pixel-Desktop-Pictures-MacOS-App
