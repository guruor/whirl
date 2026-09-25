# Windows wallpaper control: per-monitor and per-virtual-desktop reality

Research note for `guruor/whirl`. Documentation only.

**Why this exists.** `prototype/wh-rotate/set_windows.go` calls
`SystemParametersInfoW(SPI_SETDESKWALLPAPER)` and writes `WallpaperStyle`/`TileWallpaper` under
`HKCU\Control Panel\Desktop`. That was written from memory and has never been executed. This note
replaces the guesswork with sourced fact and answers one question that changes the daemon's data
model: can Windows do **per-monitor** wallpaper, and does any of it work **per virtual desktop**?

**Verification reality.** No Windows machine was available for this project and nothing below was
run. Every capability claim carries a numbered source, and every claim is graded in
`Confidence and verification levels`. There is a `What must be tested on real Windows` checklist at
the end.

Retrieved 2026-09-25.

## Bottom line

| Capability | Reality | Minimum OS | Grade |
|---|---|---|---|
| Per-monitor wallpaper, programmatic | Yes. `IDesktopWallpaper::SetWallpaper(monitorID, path)` [1][2] | Windows 8 / Server 2012, desktop apps only [1] | Documented and certain |
| One image on all monitors | Yes, same call with `monitorID = NULL` [2] | same [2] | Documented and certain |
| Enumerate monitors for that call | Yes, `GetMonitorDevicePathCount` + `GetMonitorDevicePathAt` [1][9] | same [9] | Documented and certain |
| Per-monitor styling (fill/fit/span/...) | No. `SetPosition` is system-wide, not per monitor [4] | Windows 8 [4] | Documented and certain |
| Per-virtual-desktop wallpaper via `IDesktopWallpaper` | No. Not modelled by the API at all [1] | n/a | Documented absence, plus three independent non-Microsoft sources [19][30][35] |
| Per-virtual-desktop wallpaper, any supported way | No public API. Only undocumented, per-build shell COM (`IVirtualDesktopManagerInternal::SetDesktopWallpaper`) [20][21] | Windows 11 only, per build [21] | Implemented elsewhere, likely |
| `SystemParametersInfoW(SPI_SETDESKWALLPAPER)` | One image for everything; no monitor handle, no monitor enumeration [11] | Windows 2000 and later [11] | Documented and certain |
| Set wallpaper from Session 0 / a service | No. Interactive desktop required [12][33][34] | n/a | Documented (indirect) plus repeated field reports |
| API deprecated? | No deprecation notice on any `IDesktopWallpaper` page [1][2] | n/a | Documented absence |

## The central question, answered explicitly

**Does `IDesktopWallpaper` support per-monitor wallpaper? Yes, and it is the documented way to do
it.** `SetWallpaper` takes a `monitorID` parameter, obtained from `GetMonitorDevicePathAt`, and
passing `NULL` for `monitorID` sets the image on all monitors instead [2]. The monitor list comes
from `GetMonitorDevicePathCount` and `GetMonitorDevicePathAt(index)` [1][9]. The interface is COM,
`IUnknown`-derived, in `shobjidl_core.h`, and Microsoft documents it as Windows 8 and Windows
Server 2012, desktop apps only [1]. `GetWallpaper` with `monitorID = NULL` returns `S_FALSE` and an
empty string when monitors are displaying different wallpapers or a slideshow is running, which is
Microsoft explicitly acknowledging the multi-wallpaper state [3].

**Does it work per virtual desktop? No, and it does not model virtual desktops at all.** Nothing in
the interface takes a desktop identifier [1]. Three independent sources say the per-monitor and
per-virtual-desktop modes are mutually exclusive in practice: a Microsoft Q&A thread for Windows 11
states that per-monitor background configuration is not supported with multiple desktops open and
that the desktop falls back to a static picture [38]; the superuser report for Bing Wallpaper
describes the same failure mode from the application side, that on Windows 11 the updater changes
only the currently active desktop and switching desktops makes the machine replay the old wallpaper
[30]; and the maintainer of Auto Dark Mode states the constraint flatly, "You can either do multi
monitor wallpapers OR vdesktop wallpapers. Not both, this is a windows limitation," and later
"Windows itself doesn't support multi-screen multi-vdesktop wallpapers itself" [19]. The same
maintainer confirmed in October 2025 that the issue is still open and still relevant [19].

**How does Windows 11 do per-desktop wallpapers then?** Through shell COM that is not part of
`IDesktopWallpaper`. Windows added per-Virtual-Desktop backgrounds in Insider build 21337, announced
on the Windows Insider blog as a Settings feature, not as an API [18]. The community implementation
that drives it, `MScholtes/VirtualDesktop`, calls
`IVirtualDesktopManagerInternal::SetDesktopWallpaper(IVirtualDesktop, HSTRING path)` and
`UpdateWallpaperPathForAllDesktops(HSTRING path)` [20]. Those are undocumented interfaces, the tool
ships a separate source file per Windows build because the COM GUIDs change between builds, and the
wallpaper command is available only in the Windows 11 binaries [21]. So the answer is: per-desktop
wallpaper exists as a Windows 11 shell feature, is reachable only through unsupported per-build COM,
and is not something a wallpaper daemon should depend on.

**Comparison with `SystemParametersInfoW`.** `SPI_SETDESKWALLPAPER` (0x0014) takes a single file path
in `pvParam`; the only related getter, `SPI_GETDESKWALLPAPER` (0x0073), returns one path and caps it
at `MAX_PATH` characters [11]. There is no monitor parameter anywhere in the desktop-parameter table
[11]. It cannot enumerate monitors, cannot set one monitor differently from another, and cannot report
which monitor shows what. The replacement is `IDesktopWallpaper`, and two well-known implementations
still default to the old call: `reujab/wallpaper.rs` uses `SystemParametersInfoW` for both get and set
and writes the style to the registry [23], and Auto Dark Mode still has a `SetGlobalWallpaper` path
that calls `SystemParametersInfo(0x0014, 0, path, 1 | 2)` [27]. So `SPI_SETDESKWALLPAPER` is not
broken, it is simply single-image and per-user.

## 1. `IDesktopWallpaper` interface, and what it costs from Rust

### Methods that matter to us

| Method | Signature | What it does for us | Source |
|---|---|---|---|
| `SetWallpaper` | `HRESULT SetWallpaper(LPCWSTR monitorID, LPCWSTR wallpaper)` | Sets one monitor, or all when `monitorID` is `NULL` | [2] |
| `GetWallpaper` | `HRESULT GetWallpaper(LPCWSTR monitorID, LPWSTR *wallpaper)` | Reads back one monitor; `S_FALSE` + empty string when monitors differ or a slideshow runs; `NULL` means "one image on all" | [3] |
| `GetMonitorDevicePathCount` | `HRESULT GetMonitorDevicePathCount(UINT *count)` | Size of the monitor list | [1] |
| `GetMonitorDevicePathAt` | `HRESULT GetMonitorDevicePathAt(UINT monitorIndex, LPWSTR *monitorID)` | Stable monitor ID string, index-based | [9] |
| `GetMonitorRECT` | `HRESULT GetMonitorRECT(LPCWSTR monitorID, RECT *displayRect)` | Physical-pixel bounds; also distinguishes attached from detached monitors | [1][9] |
| `SetPosition` | `HRESULT SetPosition(DESKTOP_WALLPAPER_POSITION position)` | System-wide display mode; `S_FALSE` if already in that state | [4] |
| `SetBackgroundColor` | `HRESULT SetBackgroundColor(COLORREF color)` | Colour shown when no image is displayed, also the letterbox border | [1][5] |
| `Enable` | `HRESULT Enable(BOOL enable)` | `FALSE` disables the background entirely (solid colour); `E_FILE_NOT_FOUND` if the remembered image is gone | [5] |
| `SetSlideshow` | `HRESULT SetSlideshow(IShellItemArray *items)` | Sets the slideshow images as a folder or a file list; any other array shape fails | [6] |
| `SetSlideshowOptions` | `HRESULT SetSlideshowOptions(DESKTOP_SLIDESHOW_OPTIONS options, UINT slideshowTick)` | Shuffle flag plus interval in milliseconds | [7] |
| `AdvanceSlideshow` | `HRESULT AdvanceSlideshow(LPCWSTR monitorID, DESKTOP_SLIDESHOW_DIRECTION direction)` | Steps the slideshow forward or backward, per monitor | [1] |
| `GetStatus` | `HRESULT GetStatus(DESKTOP_SLIDESHOW_STATE *state)` | `DSS_ENABLED` (0x01), `DSS_SLIDESHOW` (0x02), `DSS_DISABLED_BY_REMOTE_SESSION` (0x04) | [8] |

Two behavioural details worth flagging up front.

`Enable(FALSE)` is **not** "stop the slideshow". It disables the desktop background and shows a solid
colour, and Microsoft notes that a later `SetWallpaper` or `SetSlideshow` call re-enables the
background even if it was disabled [5]. `IDesktopWallpaper::GetStatus` is the documented way to ask
whether a slideshow is configured at all [8]. A community project that suppresses wallpaper during
full-screen games uses exactly this: it declares `Enable` on the interface [40] and calls
`_wallpaper.Enable(true)` / `Enable(false)` to toggle wallpaper display [39][40].

`SetPosition` is per system, not per monitor. The documentation says the position value determines
"how the image will be displayed on the system's monitors" [4]. There is no per-monitor position
call. If whirl ever wants one monitor filled and another fitted, the only way is the composite-image
trick below, and that is a workaround, not an API feature.

### Calling it from Rust

The interface is a plain COM interface, and the `windows` crate (windows-rs) already binds it, so no
hand-written vtable is needed [28][42]. `windows::Win32::UI::Shell::IDesktopWallpaper` exposes every
method above with typed signatures, including `SetWallpaper`, `GetMonitorDevicePathAt`,
`SetPosition`, `SetSlideshowOptions`, `AdvanceSlideshow`, `GetStatus` and `Enable` [28]. The coclass
is exposed as `windows::Win32::UI::Shell::DesktopWallpaper` [24].

A maintained, merged Rust implementation shows the whole pattern in about 145 lines:
`sindresorhus/windows-wallpaper` calls `CoInitialize(None)`, then
`CoCreateInstance(&DesktopWallpaper, None, CLSCTX_LOCAL_SERVER)`, then `GetMonitorDevicePathCount`,
`GetMonitorDevicePathAt(index)`, `SetWallpaper(monitor, path)` and `SetPosition(position)`, and
releases with `CoFreeUnusedLibraries` and `CoUninitialize` on `Drop` [24]. Its `Cargo.toml` pins
`windows = { version = "0.44.0", features = ["Win32_Foundation", "Win32_System_Com",
"Win32_System_Memory", "Win32_UI_Shell"] }`, so the cost is two feature flags beyond COM and one
dependency, not a new runtime [41].

Three practical costs to plan for. First, COM apartment setup is required: `CoInitialize` before
`CoCreateInstance`, `CoUninitialize` after, which in Rust means either living with a `Drop` guard or
using `windows::core::initialize_sta`-style helpers [24]. Second, `CLSCTX_LOCAL_SERVER` works, which
tells us the coclass is out-of-process rather than a DLL loaded into our address space [24][29]; the
registration shape that implies is the one Microsoft documents under `LocalServer32` [13], including
the rule that a local server has 60 seconds to register its class object before the activation
times out [13]. A cross-process call per wallpaper change is cheap at slideshow frequency but not a hot-loop operation.
Third, the out-of-process server is not always immediately available: `WinDynamicDesktop` wraps
coclass creation in a retry loop that catches `REGDB_E_CLASSNOTREG` and retries three times with a
one-second sleep, which is the shape of a bug you get when you touch this API during logon or boot
[22].

`IDesktopWallpaper` is also not usable from a UWP or WinUI3 sandbox: Microsoft's answer on that is
that the interface supports desktop apps only, so a UWP app cannot use it and should use
`UserProfilePersonalizationSettings` instead, which offers no per-monitor capability [29][1].

Other independent implementations agree on the shape: Microsoft PowerToys creates the same coclass
with `CLSCTX.ALL` and reads wallpaper paths with `GetMonitorDevicePathCount`, `GetMonitorDevicePathAt`
and `GetWallpaper` [26], and `WinDynamicDesktop` declares the full method order, including
`SetPosition` and `GetStatus`, on a single `IUnknown`-derived interface [22].

The two GUIDs, for whoever writes the binding by hand: `CLSID_DesktopWallpaper` is
`{C2CF3110-460E-4fc1-B9D0-8A1C0C9CC4BD}` and `IID_IDesktopWallpaper` is
`{B92B56A9-8B55-4E14-9A89-0199BBB6F93B}` [22][27]. Two independent implementations agree on both GUIDs [22],
and there are GUIDs quoted in forum comments that disagree with them [29]; take the values from the
SDK header or from the `windows` crate, not from a forum thread.

## 2. Wallpaper position and style

`DESKTOP_WALLPAPER_POSITION` has six values: `DWPOS_CENTER` (0), `DWPOS_TILE` (1), `DWPOS_STRETCH`
(2), `DWPOS_FIT` (3), `DWPOS_FILL` (4), `DWPOS_SPAN` (5) [10]. The documentation defines each in
terms of the older `IActiveDesktop`/`WPSTYLE_*` styles: `DWPOS_FIT` stretches to one dimension
without cropping and may letterbox, `DWPOS_FILL` crops to avoid letterbox bars, `DWPOS_SPAN` spans
one image across all attached monitors [10].

Programmatically: call `IDesktopWallpaper::SetPosition` [4]. Microsoft documents that call and the
`S_FALSE` "already in that state" return, and nothing else.

Where it is stored: `HKCU\Control Panel\Desktop`, as `WallpaperStyle` (REG_SZ) plus `TileWallpaper`
(REG_SZ, `1` for tiled, `0` otherwise), with the image path in the same key's `Wallpaper` value [23][31][32].
The community-documented number map is `0` centre or tile, `2` stretch, `6` fit, `10` fill, `22`
span, and it matches the `DWPOS_*` ordering one-for-one [31][10]. Independently, the Firefox tree
carries the same mapping in `nsWindowsShellService.cpp`, which is where `reujab/wallpaper.rs` copied
it from, and that implementation sets `WallpaperStyle` to `"0"` for centre and tile, `"6"` fit,
`"22"` span, `"2"` stretch, `"10"` fill, with `TileWallpaper` `"1"` only for tile [23]. `WallcatWindows`
writes the identical numbers and adds the comment "Windows 8 or newer only!" against span [25].

Two gotchas. Style is per system, not per monitor: the registry value is a single `WallpaperStyle`
under `HKCU\Control Panel\Desktop`, and the multi-monitor variant lives in a machine-generated
`HKCU\Control Panel\Desktop\PerMonitorSettings\<monitorID>` subtree that Microsoft's own answer
describes as created by the OS when the user manipulates display settings, not as something to author
by hand, and which it says would require a logoff and logon to take effect [43][31]. And writing the
registry alone does not repaint the desktop: both implementations
that set the style also re-apply the wallpaper afterwards, `reujab` by calling
`set_from_path(&get()?)` at the end of `set_mode` [23] and `WallcatWindows` by writing the registry
after `SetWallpaper` [25]. So the safe order is: set the style, then set the wallpaper, and expect to
need the second step for the change to be visible.

## 3. Virtual desktops, monitor hotplug, resolution change

**Virtual desktop switch.** `IDesktopWallpaper` has no concept of a virtual desktop, so on Windows 10
the practical answer is "one wallpaper set covers all desktops". On Windows 11, where per-desktop
backgrounds exist, the behaviour is defensive: Windows treats per-desktop wallpaper mode and
per-monitor wallpaper mode as mutually exclusive [19], and multiple sources report that a wallpaper
written by an application lands on the currently active desktop only [19][30]. Auto Dark Mode's
maintainer describes the mode detection explicitly, "If it detects virtual desktops open with
different wallpapers present, the v-desktop wallpaper mode is considered active," and the workaround
their users confirm working is to close all virtual desktops, apply the wallpaper through the
multi-monitor path, then reopen them [19]. Lively Wallpaper, a widely used wallpaper engine, still
carries per-virtual-desktop wallpapers as an open feature request rather than a shipped capability
[37]. So a whirl daemon that sets wallpaper while the user has several virtual
desktops open should expect either a per-desktop-only change or a silent revert.

One sourcing caveat. The WinDynamicDesktop virtual desktop thread [35] is an open proposal from a
contributor, not merged behaviour, and its reproduction claims are the contributor's own, so it is
cited here only as corroboration of the shape of the problem and not as proven behaviour. WinDynamicDesktop
removed its own virtual desktop support in 2022, with the commit message "Remove VirtualDesktop API
and add download mirror list", which is the stronger evidence that a mainstream tool tried this and
backed out [36].

**Monitor hotplug.** The API is index-based and path-based, and the documented remark on
`GetMonitorDevicePathAt` is the important one: it "can be called on monitors that are currently
detached but that have an image assigned to them", and `GetMonitorRECT` is the way to tell attached
from detached [9]. In other words Windows keeps an image assignment for a monitor that is not
currently plugged in, and the enumeration will hand you its ID. Auto Dark Mode handles the
consequence by diffing configuration against a live enumeration: `DetectMonitors` adds a new entry
for a monitor ID it has not seen and seeds it with `GetWallpaper(monitor.DeviceId)`, and
`CleanUpMonitors` removes entries for monitors no longer connected [27]. That is the model to copy:
treat the monitor set as dynamic, key everything by the device-path string, and reconcile on change
rather than assuming a fixed topology.

**Resolution change.** No source found that states what `IDesktopWallpaper` does on a resolution or
DPI change. Open source implementations do not special-case it; `WallcatWindows` reads its geometry
from the API and re-applies on demand [25], and the WinDynamicDesktop proposal for virtual desktop
sync uses `GetMonitorRECT` specifically because it returns physical pixel bounds that stay correct
under DPI scaling and mixed resolutions [35]. Treat "Windows re-renders the wallpaper correctly after
a resolution change" as unverified and put it on the test list.

## 4. Session 0, services, and the correct hosting model

**A service cannot set the logged-on user's wallpaper.** Microsoft states that services cannot
directly interact with a user as of Windows Vista, that the interactive-service techniques "should not
be used in new code", and that by default services use a noninteractive window station [12]. All
services run in Terminal Services session 0, which is not a session any user logs into [12]. A
developer trying it from a service reports the concrete failure, Win32 error 1459, "This operation
requires an interactive window station", and in that thread the accepted diagnosis is session 0
isolation [34]. A later question asking the same thing gets the direct answer: the wallpaper is a
per-user setting, a service runs as a different principal and has no desktop, and the recommendation
is to abandon the service model and run the application at startup as a normal program, or to use
`CreateProcessAsUser` to spawn a helper in the user's session if a service really is required [33].
Note the second half of that: even impersonating the user is not enough, because the update message
cannot be broadcast across the session boundary [33].

**The correct model for whirl on Windows is a per-user autostart process in the interactive session,
not a service.** That is what the existing wallpaper tools do: Auto Dark Mode runs a service plus a
per-user shell component and does wallpaper work from the user-side process [27], and the
`windows-wallpaper` Rust binary is a plain interactive process that calls `CoInitialize` on itself
[24]. For a personal wallpaper rotator the only reason to want a service is "run before logon", and
wallpaper is meaningless before logon anyway. If cross-user wallpaper is ever needed, Microsoft's
supported pattern is a hidden GUI application launched with `CreateProcessAsUser` from the service
and talking to it over IPC, with the per-user application registered under
`HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Run` [12].

**A third option exists and is worth knowing about.** `Personalization` CSP, at
`./Vendor/MSFT/Personalization/DesktopImageUrl`, sets the desktop image as a device-scoped policy,
downloads or copies the image, reports status via `DesktopImageStatus`, and prevents the user from
changing it [14]. It is documented for Windows 10 1709 and later and is driven through an MDM channel,
not a Win32 call [14][15]. It is also edition-gated: Enterprise and Education, plus Pro only when
`SetEduPolicies` in SharedPC CSP is set [14]. That makes it a non-starter for a personal rotator, and
it is listed here only so nobody rediscovers it and mistakes it for a general-purpose API.

## 5. Windows Spotlight and slideshows: taking over and releasing control

There are four background types a user can be in, and the API surface treats only some of them as
first class. Windows Spotlight is the one shipped as the default: after KB5046633 (October 2024) and
KB5048652 (December 2024) the default Windows wallpaper changes to Windows spotlight [17]. It sits
behind personalization settings and the `AllowSpotlightCollection` policy, which is documented for
Windows 11 21H2 and later and controls whether "Spotlight collection" even appears as a Background
option [16]. Microsoft's own guidance for taking over from Spotlight is policy-shaped: configure a
custom lock screen and background image, and the background image is replaced while spotlight
suggestions continue [15][17].

Programmatically, the tools that matter are:

1. `GetStatus` to find out whether a slideshow is enabled or configured at all, via `DSS_ENABLED`
   and `DSS_SLIDESHOW` [8].
2. `SetWallpaper` to install your image. Microsoft documents that this call re-enables the desktop
   background if it had been disabled [5], but it does **not** document that it terminates a running
   slideshow.
3. `Enable(FALSE)` to suppress the desktop background entirely while still leaving the machine in a
   known state, which is what the game-mode tool does [39][40], and `Enable(TRUE)` to restore it [40].
   Disabling is documented as being for performance reasons and results in a solid colour, with the
   colour set by `SetBackgroundColor` [5].
4. `SetSlideshow` and `SetSlideshowOptions` to hand the slideshow back, as a folder or a list of
   items plus a shuffle flag and an interval in milliseconds [6][7].

**Releasing control cleanly.** The honest position is that the documented API lets you install your
own image, save the previous state first, and restore it, and nothing more. `GetWallpaper` with
`monitorID = NULL` returns `S_FALSE` and an empty string when the monitors disagree or a slideshow is
running [3], so read per monitor instead, using `GetMonitorDevicePathAt` to walk the list [9]. Capture
`GetPosition`, `GetBackgroundColor` and `GetStatus` before touching anything, since all three are
readable [4][1][8]. To hand control back, restore each monitor's previous path and then restore the
position and background colour [2][4][1]. Whether a slideshow that was running before whirl started
resumes, or whether the user has to reselect it, is **not documented anywhere I could find** and is on
the test list.

What is worth saying plainly: turning Spotlight or a slideshow back on is a settings-level action, and
the documented, supported way to make a machine use a fixed image instead of Spotlight is the
`AllowSpotlightCollection` policy or a custom background image configured through policy [16][15].
For a personal tool, the pragmatic sequence is to detect the state, set the image, and provide an
explicit "give it back" that restores the values captured on the way in, rather than pretending the
API has a suspend/resume.

## 6. Minimum versions and support status

| Capability | Minimum | Notes | Source |
|---|---|---|---|
| `IDesktopWallpaper` (whole interface) | Windows 8 / Windows Server 2012 | Desktop apps only | [1] |
| `SetWallpaper`, `GetWallpaper` | Windows 8 / Server 2012 | Desktop apps only | [2][3] |
| `SetPosition`, `GetPosition` | Windows 8 / Server 2012 | Desktop apps only | [4] |
| `Enable` | Windows 8 / Server 2012 | Desktop apps only | [5] |
| `SetSlideshow`, `SetSlideshowOptions`, `AdvanceSlideshow`, `GetStatus` | Windows 8 / Server 2012 | Desktop apps only | [6][7][8] |
| `DWPOS_SPAN` position | Windows 8 or newer | Comment in a maintained implementation | [25] |
| Per-Virtual-Desktop wallpaper in the Settings UI | Windows 11, Insider build 21337 first | Announced as a Settings feature, not an API | [18] |
| Per-Virtual-Desktop wallpaper via COM | Windows 11 only, and per build | Undocumented; GUIDs change between builds | [20][21] |
| `Personalization` CSP desktop image | Windows 10 1709 (10.0.16299) | Enterprise/Education, Pro only with SharedPC `SetEduPolicies` | [14][15] |
| `AllowSpotlightCollection` policy | Windows 11 21H2 (10.0.22000) | User scope, Enterprise/Education | [16] |
| `SystemParametersInfoW(SPI_SETDESKWALLPAPER)` | Windows 2000 and later | Single image, no monitor parameter | [11] |

**Is Microsoft deprecating any of this?** No. The `IDesktopWallpaper` interface page carries a
requirements table and no deprecation notice [1]. The same is true of the individual method pages for
`SetWallpaper`, `SetPosition` and `Enable` [2][4][5], and of `SetSlideshow`, `SetSlideshowOptions` and
`GetStatus` [6][7][8]. That is also true of `GetMonitorDevicePathAt` [9]. That is an absence of a
deprecation notice, not an affirmative statement of support, and it is the correct reading to take
from reference pages that have simply never been marked deprecated. Note the asymmetry: the
documented, supported API is the one that cannot do per-desktop wallpaper, and the thing that can is
undocumented and build-specific [21].

## Confidence and verification levels

**Documented and certain** (Microsoft reference documentation, quoted above):

- `IDesktopWallpaper::SetWallpaper` is per monitor, with `NULL` meaning all monitors [2].
- `GetWallpaper(NULL)` returns `S_FALSE` and an empty string when monitors differ or a slideshow runs [3].
- `SetPosition` is system-wide, applies to "the system's monitors", and returns `S_FALSE` if already
  in that state [4].
- The six `DWPOS_*` values and what each means [10].
- `Enable(FALSE)` shows a solid colour and is intended for performance, and a later `SetWallpaper` or
  `SetSlideshow` re-enables the background [5].
- `SetSlideshow` takes an `IShellItemArray` that is either a folder or a set of items in one folder [6].
- `SetSlideshowOptions` is a shuffle flag plus an interval in milliseconds [7].
- `GetStatus` returns `DSS_ENABLED` / `DSS_SLIDESHOW` / `DSS_DISABLED_BY_REMOTE_SESSION` [8].
- `GetMonitorDevicePathAt` can enumerate monitors that are currently detached but have an image
  assigned, and `GetMonitorRECT` distinguishes attached from detached [9].
- `SystemParametersInfoW` desktop parameters carry no monitor handle, and `SPI_GETDESKWALLPAPER` is
  capped at `MAX_PATH` [11].
- Services cannot interact with the user, all services run in session 0, and the supported pattern is
  a per-session helper launched with `CreateProcessAsUser` plus IPC [12].
- `Personalization` CSP sets the desktop image at device scope, reports status, blocks the user from
  changing it, and is edition-gated [14].
- Windows spotlight is the default desktop background since the October 2024 and December 2024
  updates, and a custom background image replaces the spotlight image [15][17].
- Per-Virtual-Desktop backgrounds were announced as a Settings feature in Insider build 21337 [18].
- The `windows` crate exposes the whole interface, and `CLSCTX_LOCAL_SERVER` is a working creation
  context [28][24][42].

**Implemented elsewhere and likely** (maintained open source code that demonstrably does it, or
consistent multi-source reporting):

- The `WallpaperStyle` / `TileWallpaper` numeric map, `0`/`2`/`6`/`10`/`22`, and that span needs
  Windows 8 or newer [23][25][31].
- That writing the style to the registry does not repaint, and the wallpaper must be re-applied [23][25].
- That a style change is system-wide and the per-monitor variant lives in an OS-generated
  `PerMonitorSettings` subtree [31].
- That the coclass is an out-of-process server and can be transiently unavailable during logon
  (inferred from the working `CLSCTX_LOCAL_SERVER` call and from the retry-on-`REGDB_E_CLASSNOTREG`
  loop in two independent implementations) [24][29][22].
- That per-monitor and per-Virtual-Desktop wallpaper modes are mutually exclusive, and that applying
  a wallpaper with virtual desktops open may only affect the active desktop [19][30][35].
- That `IVirtualDesktopManagerInternal::SetDesktopWallpaper` is the mechanism behind Windows 11
  per-desktop wallpapers, and that it is undocumented and per-build [20][21].
- That `Enable` is the right call to suppress wallpaper without removing the image assignment [39][40].
- That monitor sets must be treated as dynamic, keyed by device-path string, and reconciled by
  diffing against a live enumeration [27].

**Unverified hypothesis** (no source found, do not build on it):

- Whether `SetWallpaper` terminates a running slideshow or merely overwrites the current frame until
  the next slideshow tick. No Microsoft statement found; the docs only say `SetWallpaper` re-enables a
  disabled background [5]. Test it.
- Whether a slideshow that was running before whirl took over resumes when whirl releases control.
- Whether Windows re-renders the wallpaper correctly after a resolution or DPI change, and whether it
  re-applies it to the right monitor.
- Whether a per-monitor image assignment survives unplug and replug of the same physical monitor. The
  `GetMonitorDevicePathAt` remark implies the assignment is retained for detached monitors but does
  not say the device path is stable across a re-plug [9].
- Whether `GetMonitorDevicePathAt` index order is stable. The interface is index-based and no source
  states an ordering guarantee [9]. Key by the returned string, never by index.
- What the `WallpaperStyle` registry value is on a Windows 11 machine that is in per-desktop
  wallpaper mode, or whether `HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\VirtualDesktops\Desktops\<GUID>\Wallpaper`
  exists on supported builds. A community gist documents an `Explorer\Wallpapers\BackgroundType`
  value with `0` picture, `1` solid colour, `2` slideshow, `3` spotlight [32], and a Microsoft Q&A
  answer references deleting a `Wallpaper` value under `VirtualDesktops\Desktops` [38]; neither is a
  Microsoft reference page and neither should be written to by whirl.
- Whether `reg add` for `WallpaperStyle` (the current prototype's approach) behaves identically to
  `IDesktopWallpaper::SetPosition`. Both write the same registry location per the sources, but no
  source states they are equivalent [23][25][31].

## What must be tested on real Windows before release

Ordered so that the earliest failures kill the most design work.

1. **Per-monitor set, two monitors, different images.** Enumerate with `GetMonitorDevicePathCount`
   and `GetMonitorDevicePathAt`, then `SetWallpaper` each ID separately. Confirm both monitors differ
   and that `GetWallpaper(monitorID)` returns what you set [2][9].
2. **Monitor ID stability.** Record the device-path strings, reboot, and compare. Then unplug one
   monitor, re-enumerate, and confirm whether the detached monitor still appears and whether its ID
   is the same string [9].
3. **Per-monitor styling.** Confirm there is no way to give two monitors different position values,
   and record what `SetPosition` does to an already-per-monitor set. Confirm `DWPOS_SPAN` and
   `WallpaperStyle=22` behave the same on a single-monitor machine and on two [4][10][25].
4. **Virtual desktops, Windows 11.** Create three virtual desktops, then call `SetWallpaper(NULL, path)`.
   Check whether one, all, or none of the desktops change, then switch between them [19][30].
5. **Virtual desktops, per-monitor conflict.** With per-desktop backgrounds already set in Settings,
   apply a per-monitor set and observe whether Windows reverts to per-desktop mode [19].
6. **Slideshow interaction.** Enable a slideshow in Settings, then `SetWallpaper`. Does the slideshow
   stop, or does it overwrite the image at the next tick? Then `Enable(FALSE)` and confirm the solid
   colour, then `Enable(TRUE)` and confirm what comes back [5][8].
7. **Spotlight interaction.** With Windows spotlight as the background, apply an image and confirm the
   apply survives a logon and a reboot. Record what happens to the Spotlight settings [16][17].
8. **Release path.** Capture path, position, background colour and `GetStatus` before taking over,
   then restore all four and confirm the desktop is genuinely back to its prior state [1][3][4].
9. **Resolution and DPI change.** Apply a per-monitor set, change resolution on one monitor and DPI
   scaling on the other, and confirm the correct image is still on the correct monitor [35].
10. **Cold boot and logon timing.** Start the daemon at logon and confirm `CoCreateInstance` does not
    hit `REGDB_E_CLASSNOTREG`. If it can, copy the retry-with-sleep shape [22].
11. **Session 0, to confirm the negative.** Try setting the wallpaper from a service or a scheduled
    task running as SYSTEM and record the exact failure, expected to be error 1459 or an HRESULT
    failure [12][33][34].
12. **`SystemParametersInfoW` against `SetPosition`.** Set the style through `reg add` and through
    `SetPosition`, and record whether the two paths produce identical registry state and identical
    visual results [23][25].
13. **Slideshow resumption.** After releasing control, does a slideshow that was running before come
    back on its own, or does the machine stay on the last static image [8]?

## What this means for the current prototype

Not a code review, but the parts of `prototype/wh-rotate/set_windows.go` that this research touches:

- The comment "Windows has no per-virtual-desktop wallpaper: one image covers all of them" is correct
  for Windows 10 and for the API in general, and is wrong for a Windows 11 machine that has
  per-desktop backgrounds set, where the outcome is a per-desktop-only change or a revert rather than
  "one image covers all" [19][30].
- Dropping `allSpaces` is defensible: there is no supported way to set all virtual desktops from this
  API [1][20].
- `SystemParametersInfoW(SPI_SETDESKWALLPAPER)` cannot address a single monitor, so if whirl ever
  wants per-monitor rotation the call has to be replaced with `IDesktopWallpaper` rather than
  extended [11][2].
- Writing `WallpaperStyle`/`TileWallpaper` straight into the registry is what `reujab/wallpaper.rs`
  does, so it is not unreasonable, but it is the undocumented-by-Microsoft path, and both
  implementations that do it also re-apply the wallpaper afterwards because the registry write alone
  does not repaint [23][25][31].

## Sources

[1] https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-idesktopwallpaper
[2] https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-idesktopwallpaper-setwallpaper
[3] https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-idesktopwallpaper-getwallpaper
[4] https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-idesktopwallpaper-setposition
[5] https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-idesktopwallpaper-enable
[6] https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-idesktopwallpaper-setslideshow
[7] https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-idesktopwallpaper-setslideshowoptions
[8] https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-idesktopwallpaper-getstatus
[9] https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-idesktopwallpaper-getmonitordevicepathat
[10] https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/ne-shobjidl_core-desktop_wallpaper_position
[11] https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-systemparametersinfow
[12] https://learn.microsoft.com/en-us/windows/win32/services/interactive-services
[13] https://learn.microsoft.com/en-us/windows/win32/com/localserver32
[14] https://learn.microsoft.com/en-us/windows/client-management/mdm/personalization-csp
[15] https://learn.microsoft.com/en-us/windows/configuration/background
[16] https://learn.microsoft.com/en-us/windows/client-management/mdm/policy-csp-experience
[17] https://learn.microsoft.com/en-us/windows/configuration/windows-spotlight
[18] https://blogs.windows.com/windows-insider/2021/03/17/announcing-windows-10-insider-preview-build-21337
[19] https://github.com/AutoDarkMode/Windows-Auto-Night-Mode/issues/469
[20] https://github.com/MScholtes/VirtualDesktop/blob/master/VirtualDesktop11.cs
[21] https://github.com/MScholtes/VirtualDesktop/blob/master/README.md
[22] https://github.com/t1m0thyj/WinDynamicDesktop/blob/master/src/COM/DesktopWallpaper.cs
[23] https://github.com/reujab/wallpaper.rs/blob/master/src/windows.rs
[24] https://github.com/sindresorhus/windows-wallpaper/blob/main/src/lib.rs
[25] https://github.com/PaitoAnderson/WallcatWindows/blob/master/Util/SetWallpaper.cs
[26] https://github.com/microsoft/PowerToys/blob/main/src/modules/cmdpal/Microsoft.CmdPal.UI/Helpers/WallpaperHelper.cs
[27] https://github.com/AutoDarkMode/Windows-Auto-Night-Mode/blob/master/AutoDarkModeSvc/Handlers/WallpaperHandler.cs
[28] https://microsoft.github.io/windows-docs-rs/doc/windows/Win32/UI/Shell/struct.IDesktopWallpaper.html
[29] https://learn.microsoft.com/en-us/answers/questions/1160562/modifying-the-desktop-wallpaper
[30] https://superuser.com/questions/1701360/disable-unique-backgrounds-on-virtual-desktops-in-windows-11
[31] https://github.com/nikvoronin/LastWallpaper/blob/main/docs/windows_desktop_wallpaper.md
[32] https://gist.github.com/Postrediori/f89ab31d0f7e10b1e9c09a3ac779bc59
[33] https://stackoverflow.com/questions/74689465/how-to-change-the-wallpaper-of-a-windows-machine-through-a-service-c
[34] https://stackoverflow.com/questions/24974756/change-wallpaper-with-service
[35] https://github.com/t1m0thyj/WinDynamicDesktop/issues/694
[36] https://github.com/t1m0thyj/WinDynamicDesktop/commit/786be9bdf4192bf1250063537aace8d1204fd37e
[37] https://github.com/rocksdanister/lively/issues/488
[38] https://learn.microsoft.com/en-us/answers/questions/5824740/windows-11-keeps-changing-the-background-setting-s
[39] https://github.com/PinkD/WallpaperAutoDisabler/blob/master/README.md
[40] https://github.com/PinkD/WallpaperAutoDisabler/blob/master/WallpaperSliderAutoDisable/Util/IWallpaperTool.cs
[41] https://github.com/sindresorhus/windows-wallpaper/blob/main/Cargo.toml
[42] https://github.com/microsoft/windows-rs
[43] https://learn.microsoft.com/en-us/answers/questions/3768176/creating-key-under-the-hkey-current-usercontrol-pa
