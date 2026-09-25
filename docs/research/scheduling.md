# Scheduling: launchd, Task Scheduler and systemd user timers vs a daemon-owned timer

Status: draft for review. Written 2026-09-25 on macOS 26.5.2 (build 25F84), Darwin 25.5.0,
arm64. macOS behaviour below was measured on this machine; Windows and Linux behaviour is
cited from primary documentation and is marked **unverified here** throughout.

The question this document has to answer: **can the OS scheduler be trusted to trigger
rotations on all three platforms, or must the daemon own its own timer?**

The short answer, per platform, is at the end of this file. It is not "it depends": the three
platforms divide the work the same way, for the same reason, and the reason is not a matter of
taste.

## What "trusted" means here

Two different jobs get confused in this debate, so keep them apart:

- **Process supervision**: something starts the daemon at login, restarts it when it dies, and
  captures its output. Every OS here does this well, and it is not something a daemon can do
  for itself.
- **The clock**: something decides that a rotation is due *now*, including the awkward case
  where the machine was asleep when the rotation was due. This is where the platforms differ,
  and where "the OS can do it" is only true for one shape of schedule.

## Part 1: what each platform actually provides

### macOS: launchd

Primary source for the key semantics is the `launchd.plist(5)` man page shipped with this
machine (macOS 26.5.2, build 25F84); a verbatim copy is committed at
`probes/man-launchd.plist-macos-26.5.2.txt`. It is quoted rather than paraphrased because the
wording is the whole finding. Apple's archived *Scheduling Timed Jobs* chapter says the same
thing in prose [1].

`StartInterval` — quote from the man page on this machine:

> This optional key causes the job to be started every N seconds. If the system is asleep
> during the time of the next scheduled interval firing, that interval will be missed due to
> shortcomings in kqueue(3). If the job is running during an interval firing, that interval
> firing will likewise be missed.

That is the crux: **an `StartInterval` job loses the firing entirely when the machine is
asleep.** Apple states the general rule as "all other launchd jobs are skipped when the
computer is turned off or asleep; they will not run until the next designated time occurs"
[1]. A nightly-sleeping laptop therefore rotates on the first interval boundary *after* wake,
which for a long interval can be hours late.

`StartCalendarInterval` — same man page:

> Unlike cron which skips job invocations when the computer is asleep, launchd will start the
> job the next time the computer wakes up. If multiple intervals transpire before the computer
> is woken, those events will be coalesced into one event upon wake from sleep.

So the calendar form *does* catch up on wake, and a burst of missed calendar events collapses
to one run. Apple's guidance agrees and adds the boundary: "if the machine is off when the job
should have run, the job does not execute until the next designated time occurs" [1]. Two
sentences later the same page adds that `StartInterval` and `StartCalendarInterval` "are not
aware of each other. They are evaluated completely independently by the system", and Apple's
chapter on the plist format notes that missing keys in the calendar dictionary are wildcards
[2].

The cost of the calendar form is expressiveness: it is a wall-clock clock, not an interval. A
schedule of "every 30 minutes" is `Minute` 0 and 30; "every 90 minutes" is not expressible at
all without an array of dictionaries spelling out every firing time in a day. Any interval
that does not divide a day evenly, or that the user changes to an arbitrary number of minutes,
cannot be represented.

`ThrottleInterval` — the man page: "by default, jobs will not be spawned more than once every
10 seconds". Measured here (probes A, B, F): a `StartInterval` of 5 s and of 1 s both fire
every **10.1 s**, and the same job with `ThrottleInterval` 1 and `StartInterval` 2 fires every
**2.1 s**. The 10 s floor is the throttle, and it also governs restart cadence, so a crash loop
of a small daemon costs at most one restart per 10 s.

`KeepAlive` — the man page: "The value may be set to true to unconditionally keep the job
alive... The use of this key implicitly implies `RunAtLoad`, causing launchd to speculatively
launch the job." Measured (probes E, I): a job whose body exits immediately is restarted
forever at 10.1 s intervals, and a job that dies by `SIGKILL` is restarted on the same cadence.
That is a complete crash-restart story with no code in the daemon.

`RunAtLoad` — starts the job once when it is loaded, i.e. at login for a `LaunchAgent`; the man
page warns it "should be avoided, as speculative job launches have an adverse effect on
system-boot and user-login scenarios".

`StandardOutPath` / `StandardErrorPath` — measured (probe J): the job's stdout and stderr land
in the named files verbatim, and the files are created even when nothing is written (probes
A–G left 0-byte files). Log capture is free.

`launchctl` install/uninstall, measured on this machine:

```sh
# install
cp com.guruor.whirl.plist ~/Library/LaunchAgents/
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.guruor.whirl.plist
# manual trigger (also restarts a running job)
launchctl kickstart -k gui/$(id -u)/com.guruor.whirl
# uninstall
launchctl bootout gui/$(id -u)/com.guruor.whirl
rm ~/Library/LaunchAgents/com.guruor.whirl.plist
```

The legacy `launchctl load -w` / `unload -w` pair still works on 26.5.2 (measured, probe I) but
`bootstrap`/`bootout` is what `launchctl(1)` documents for a per-user agent, and it is what the
probes used.

**What was not tested here**: whether a *sleeping* (not merely unloaded) machine behaves as the
man page says, because that requires suspending this machine. Probes G/H tested the closest
analogue that can be tested without sleeping: a calendar firing that elapsed while the job was
not registered was **not** replayed on the next load (`runs = 0`), which matches Apple's
documented "machine is off" case [1] and is the correct behaviour for the "off" case but says
nothing about the sleep case. Probe G2 confirmed the generator itself works while loaded: a
`Minute`-triggered job fired once, 5.39 s after the calendar boundary (`runs = 1`, exit 0). One
sample is not a latency distribution; the recipe that would settle both questions is in
"Untested" at the end.

### Windows: Task Scheduler

No Windows host was available, so every claim in this section is from Microsoft documentation
and is **unverified here**.

- `StartWhenAvailable` is the "Run task as soon as possible after a scheduled start is missed"
  checkbox. The schema default is `false` [3]. When `true`, a task whose scheduled time passed
  "are queued in the Task Scheduler service's queue of tasks and they are started after a
  delay. The default delay is 10 minutes" [4]. The same page scopes it: the property "applies
  only to time-based tasks with an end boundary or time-based tasks that are set to repeat
  infinitely" [4].
- With `StartWhenAvailable` false, a missed run is simply skipped. The Task Scheduler records
  "Task Scheduler did not launch task ... as it missed its schedule. Consider using the
  configuration option to start the task when available, if schedule is missed" [17] (a
  Microsoft Learn Q&A thread quoting the event text, not product documentation).
- `WakeToRun` is the "Wake the computer to run this task" checkbox. It "indicates that the Task
  Scheduler will wake the computer when it is time to run the task, and keep the computer awake
  until the task is completed" [5][6]. It depends on the power plan: Microsoft's own
  maintenance documentation says to "Check whether Allow Wake Timer is enabled in Power
  Options" and lists `WakeToRun` as a condition for a scheduled wake to happen, and warns that
  "with the advent of laptops... machines are no longer configured to allow S3 wakeup in most
  circumstances" [7]. Wake support is thus a per-machine, per-power-plan, possibly per-firmware
  property, not a property of the task.
- Per-user vs system: by default a task runs with the current user's permissions and only
  interactively [8]. `/it` (interactive-only) means "run the scheduled task only when the run
  as user is logged on to the computer" [8]; `Logon Mode: Interactive only` is how a verbose
  query shows it. Running while logged off needs stored credentials (`/ru` + `/rp`, which is
  why `/np` exists: "No password is stored. The task runs non-interactively as the given user.
  Only local resources are available" [8]) or the `SYSTEM` account [8][10]. `schtasks /change`
  cannot remove the interactive-only property once set [10].
- Overlap: `MultipleInstancesPolicy` defaults to `IgnoreNew`, "Does not start a new instance if
  an existing instance of the task is running" [14].
- Crash restart: `RestartOnFailure` (child elements `Count` and `Interval`, both required) [15],
  reachable from `schtasks /create /xml` [8] or PowerShell
  `New-ScheduledTaskSettingsSet -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1)` [11].
- Two defaults that bite a wallpaper rotator: `DisallowStartIfOnBatteries` "The default setting
  for this element is True" [12], and `ExecutionTimeLimit` defaults such that "a task will be
  stopped 72 hours after it starts to run" [16] (set `PT0S` to run indefinitely [13]).
- Install / uninstall: `schtasks /create` (including `/xml <xmlfile>` for settings the command
  line cannot express) [8], `schtasks /delete /tn <taskname> /f` [9], `schtasks /change` [10].

The consequence for a rotation: the OS can be trusted to run a missed task, but "as soon as
possible" means *within about ten minutes of wake*, or never if the machine is on battery and
`DisallowStartIfOnBatteries` was left at its default, or if a wake timer was needed and the
plan or the firmware does not do wake timers.

### Linux: systemd user timers

No Linux host was available; **unverified here**, cited from systemd's own documentation. The
quoted text is taken from the upstream source that generates the man pages (`man/systemd.timer.xml`
on the systemd main branch), with the rendered page given alongside, because the rendered page
is what a reader will open [18][19].

- `OnCalendar=` is wall-clock. "When a calendar timer elapses while the system is sleeping it
  will not be acted on immediately, but once the system is later resumed it will catch up and
  process all timers that triggered while the system was sleeping. Note that if a calendar
  timer elapsed more than once while the system was continuously sleeping the timer will only
  result in a single service activation" [19]. That is the same promise launchd makes for its
  calendar form and the same coalescing rule.
- `Persistent=` — "If true, the time when the service unit was last triggered is stored on
  disk. When the timer is activated, the service unit is triggered immediately if it would have
  been triggered at least once during the time when the timer was inactive... This is useful to
  catch up on missed runs of the service when the system was powered down. Note that this
  setting only has an effect on timers configured with `OnCalendar=`. Defaults to false" [19].
  Without it, a power-off window loses the run; with it, `systemctl clean --what=state` must be
  run before uninstalling [19].
- `OnUnitActiveSec=` is monotonic, anchored to when the unit was last activated; monotonic
  clocks "generally pause" during suspend, and only `OnBootSec=`/`OnStartupSec=` have the
  "already in the past... it will immediately elapse" behaviour ("This is not the case for
  timers defined in the other directives") [19]. So an interval timer is the wrong tool for the
  sleep case on Linux too.
- `AccuracySec=` "Defaults to 1min. The timer is scheduled to elapse within a time window
  starting with the time specified in `OnCalendar=`... Within this time window, the expiry time
  will be placed at a host-specific, randomized, but stable position... To get best accuracy,
  set this option to 1us" [19]. A "daily at 09:00" timer may fire anywhere in 09:00–09:01.
  `RandomizedDelaySec=` defaults to 0 [19].
- `WakeSystem=` can resume a suspended machine, but "this functionality requires privileges and
  is thus generally only available in the system service manager" [19] — a `systemd --user`
  timer cannot wake anything.
- Timer units only do anything while the user manager is running. Lingering is what changes
  that: "If enabled for a specific user, a user manager is spawned for the user at boot and
  kept around after logouts" [22], and logind's `KillUserProcesses=` documentation points at
  "the description of `enable-linger`" for whether user processes survive logout [25].
- A service that is already active when the timer elapses is "not restarted, but simply left
  running" [18] — timers fire units, they do not relaunch a running daemon.
- Interval syntax, for reference: components may be a list, and "values may be suffixed with
  `/` and a repetition value, which indicates that the value itself and the value plus all
  multiples of the repetition value are matched" [20][21], so `OnCalendar=*-*-* 00/6:00:00` is
  every six hours, and `hourly`/`daily` are shorthands for `*-*-* *:00:00` and
  `*-*-* 00:00:00` [20][21].
- Install / uninstall (user units):

```sh
systemctl --user daemon-reload
systemctl --user enable --now whirl.service
loginctl enable-linger "$USER"        # only if it must run with no session open
# uninstall
systemctl --user disable --now whirl.service
loginctl disable-linger "$USER"
systemctl --user daemon-reload
```

## Part 2: the comparison that decides the design

For each capability, what the OS hands you for free. "Source" is the citation, and "verified"
says whether it was observed on a machine or only read. Every row carries one or the other;
nothing here is asserted without one.

| capability | launchd (macOS) | Task Scheduler (Windows) | systemd --user (Linux) | source |
|---|---|---|---|---|
| Interval trigger | `StartInterval`, floored at 10 s by the default throttle | `/ri <interval>` (minutes) with `MINUTE`/`HOURLY` schedule, or repetition on a time trigger | `OnUnitActiveSec`/`OnBootSec` (monotonic) | measured A/B/F; [8]; [19] |
| Calendar trigger | `StartCalendarInterval` (wildcard keys, array of dicts) | time triggers + `StartWhenAvailable` | `OnCalendar` | [launchd.plist(5)], [2]; [4]; [19] |
| Missed run while asleep | `StartCalendarInterval` runs it on wake; `StartInterval` **loses it** | only with `StartWhenAvailable=true`, ~10 min after wake; `WakeToRun` can wake the machine if the plan allows | `OnCalendar` catches up on resume; monotonic timers do not | [launchd.plist(5)] quoted; [4]; [19] quoted |
| Burst of missed runs | coalesced into one event | one delayed start per missed trigger; no coalescing language in the docs | coalesced into a single activation | [launchd.plist(5)]; [4] (silent); [19] |
| Missed run while off / job unregistered | not replayed, next designated time instead (measured: `runs = 0`) | `StartWhenAvailable` queues it after the next start; otherwise skipped | only with `Persistent=true` | measured G/H; [1]; [4]; [19] |
| Crash restart | `KeepAlive` true: unconditional, 10 s throttle (measured) | `RestartOnFailure` Count+Interval; off unless configured | `Restart=always`, `RestartSec` (default 100 ms); `no` by default | measured E/I; [15][11]; [23][24] |
| Log capture | `StandardOutPath`/`StandardErrorPath` (measured) | Task Scheduler history in its own event log; no per-task stdout redirection documented | journald: `StandardOutput=` "defaults to... journal" for a user service | measured J; [17]; [26][27] |
| Overlap of two runs | interval firings during a run are missed (measured C) | `MultipleInstancesPolicy`, default `IgnoreNew` | a service already active is left running | measured C; [14]; [18] |
| Requires an always-on daemon? | no, but the *wallpaper* needs a GUI session | no, but session 0 / logged-off tasks cannot set a desktop wallpaper | no, but a user manager must exist (session or linger) | reasoning from [1][8][22]; **unverified** |
| Can it wake the machine | no | yes, `WakeToRun` + the power plan's wake timers | only the system manager (`WakeSystem=` requires privileges) | [launchd.plist(5)] is silent, [1] implies no; [5][7]; [19] |
| Install / uninstall | plist file + `launchctl bootstrap`/`bootout` | `schtasks /create` / `/delete` | unit files + `systemctl --user enable`/`disable` | measured; [8][9]; [22] |

What a daemon-owned timer would have to reimplement, in exchange for owning the clock:

| capability | cost in the daemon | the prototype's answer |
|---|---|---|
| Sleep/wake detection | either a platform API (IOKit power notifications, `WM_POWERBROADCAST`, logind `PrepareForSleep`) or a wall-clock comparison that does not need one | wall-clock comparison: sleep in bounded 1–20 s slices, rotate once when `now >= next_at` (`prototype/whd/whd.rs`, `run_scheduler`) |
| Drift correction | keep the deadline independent of how long a rotation took: advance the previous deadline by whole intervals instead of re-anchoring it at completion | not done today: `next_at = now_secs() + interval` is set when the rotation finishes (`prototype/whd/whd.rs:165`), so each rotation's duration is added to the period. Fix is a loop (`while next_at <= now: next_at += interval`), which also makes the missed-run skip exact |
| Crash recovery | nothing, without help: a dead daemon is a dead timer | delegated to the OS supervisor (this document's recommendation) |
| Coalescing a burst | explicit rule, or a wake after a week offline rotates N times | one rotation on catch-up; the skip rule is one comparison |
| Restarting a running rotation | overlap rule for a job that takes longer than the interval | `st.rotating` guard plus a 5 s poll slice |

The asymmetry that decides the design is in the last two tables together: the daemon gets the
clock almost for free (the code already exists), whereas the OS scheduler gets *supervision*
for free and cannot be made to hold an arbitrary interval plus a wake catch-up without giving
up one of the two.

### Failure modes, spelled out

- **Machine asleep at the trigger moment.** launchd `StartInterval`: run lost; launchd
  `StartCalendarInterval`: one run on wake. Task Scheduler: run ~10 min after wake if
  `StartWhenAvailable` is set, otherwise lost; `WakeToRun` and the power plan can wake the
  machine instead. systemd `OnCalendar`: one run on resume; `Persistent=true` additionally
  covers the power-off case. A daemon that is running and comparing wall-clock time rotates
  within its poll slice of wake on all three, with no configuration and no wake timer.
- **The daemon dies.** Nothing on any of these platforms restarts a process unless you told it
  to. `KeepAlive` is one key; `RestartOnFailure` is two values; `Restart=always` is one line.
  Whichever platform, this is the OS scheduler's job, and it is the job that justifies having
  a scheduler at all in this design.
- **The user logs out.** A `LaunchAgent` stops; a per-user Windows task marked interactive-only
  stops; a `systemd --user` service stops unless lingering is enabled. For a wallpaper rotator
  this is the correct behaviour rather than a failure: none of the three wallpaper APIs works
  without a session, so a rotation while nobody is logged in has nothing to do. The one
  exception is housekeeping (cache pruning, history expiry), which is worth stating separately
  if whirl ever wants it off-session.

## Part 3: the recommendation

**Split by job, identically on all three platforms: the OS scheduler owns the process, the
daemon owns the clock. Neither platform's OS scheduler should be given the rotation schedule.**

Concretely:

### macOS

- Ship a `LaunchAgent` (`~/Library/LaunchAgents/com.guruor.whirl.plist`) with `RunAtLoad` true
  and `KeepAlive` true, `StandardOutPath`/`StandardErrorPath` pointed at the log, and **no
  `StartInterval` and no `StartCalendarInterval`**. Rationale: `StartInterval` is documented to
  lose firings across sleep (the exact failure we are trying to avoid), and the calendar form
  cannot express an arbitrary interval; the daemon's own deadline covers both, and `KeepAlive`
  covers the death case. `KeepAlive` also implies `RunAtLoad`, so one key gives login start plus
  crash restart.
- Rotation interval, pause/resume and "rotate now" stay in the daemon, reachable over the
  socket (`whctl rotate`). `launchctl kickstart -k` is not the user-facing "rotate now" path:
  it restarts the daemon, which is a supervisor action, not a rotation request.
- The daemon's deadline comparison must use the **wall clock** (`SystemTime`), not a monotonic
  clock, because that is what makes "the machine slept through the slot" visible. systemd
  states the general rule for the monotonic clock ("if the computer is temporarily suspended,
  the monotonic clock generally pauses, too") [19]; the prototype already uses `now_secs()`
  (wall clock) for `next_at`, so no change is needed there.
- Recommended plist (the keys above, nothing else):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.guruor.whirl</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/local/bin/whd</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ProcessType</key>
    <string>Background</string>
    <key>StandardOutPath</key>
    <string>/Users/me/Library/Logs/whirl/whd.log</string>
    <key>StandardErrorPath</key>
    <string>/Users/me/Library/Logs/whirl/whd.err</string>
</dict>
</plist>
```

(`ProcessType` `Background` is the man page's classification for "processes that do work that
was not directly requested by the user", whose resource limits "are intended to prevent them
from disrupting the user experience" [launchd.plist(5)]. `Standard` is equivalent to omitting
the key.)
- Install / uninstall (user-facing, tested on this machine):

```sh
cp com.guruor.whirl.plist ~/Library/LaunchAgents/
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.guruor.whirl.plist
# uninstall
launchctl bootout gui/$(id -u)/com.guruor.whirl
rm ~/Library/LaunchAgents/com.guruor.whirl.plist
```

### Windows

- One per-user task, created once at install, with **no time trigger of its own**: an
  `AtLogOn` trigger running `whd.exe`, run only when the user is logged on (interactive,
  `/it`), `RestartOnFailure` on the settings (Count ≥ 3, Interval 1 minute),
  `ExecutionTimeLimit` `PT0S` so the daemon is not killed after 72 hours [13], and
  `-AllowStartIfOnBatteries`, because "Task Scheduler starts if the computer is running on
  battery power" is not the default ([12] says the default is the opposite, [11] gives the
  switch). The daemon owns the interval.
- Do not rely on `StartWhenAvailable` for rotation: it delivers a missed start about ten
  minutes after wake, which is worse than the daemon's own catch-up, and it does nothing at all
  if the task is not already scheduled to run. Do not use `WakeToRun`: it depends on the power
  plan and on firmware, and a wallpaper rotation is not worth waking a laptop for.
- Install / uninstall (user-facing; PowerShell is the path that can express the settings, the
  `schtasks` equivalents are given because they are the documented CLI):

```powershell
$action  = New-ScheduledTaskAction -Execute "$env:LOCALAPPDATA\whirl\whd.exe"
$trigger = New-ScheduledTaskTrigger -AtLogOn
$settings = New-ScheduledTaskSettingsSet -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1) `
    -MultipleInstances IgnoreNew -ExecutionTimeLimit (New-TimeSpan -Seconds 0) `
    -AllowStartIfOnBatteries
Register-ScheduledTask -TaskName whirl -Action $action -Trigger $trigger -Settings $settings
# uninstall
Unregister-ScheduledTask -TaskName whirl -Confirm:$false
# CLI equivalents: schtasks /create /tn whirl /tr "%LOCALAPPDATA%\whirl\whd.exe" /sc onlogon /it /f
#                  schtasks /delete /tn whirl /f
```

`StartWhenAvailable` and `WakeToRun` are left at their defaults (false), so they do not appear
in the snippet. None of this was run: there is no Windows host here. Verify the switches
against the exported XML (`schtasks /query /xml`) on the first Windows machine that installs
whirl.

### Linux

- A `systemd --user` service with `Restart=always` and `RestartSec=5`, installed with
  `systemctl --user enable --now whirl.service`. `loginctl enable-linger "$USER"` only if
  whirl must do anything with no session open (cache/history housekeeping); rotation itself is
  pointless without a session.
- **No timer unit.** If one is ever wanted anyway (because the daemon's supervision is
  considered insufficient), it must be `OnCalendar` with `Persistent=true` — `OnUnitActiveSec`
  is the wrong tool, and without `Persistent=true` a power-off window loses the run.

```ini
# ~/.config/systemd/user/whirl.service
[Unit]
Description=whirl wallpaper daemon

[Service]
ExecStart=%h/.local/bin/whd
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
```

- Install / uninstall:

```sh
systemctl --user daemon-reload
systemctl --user enable --now whirl.service
# uninstall
systemctl --user disable --now whirl.service
rm ~/.config/systemd/user/whirl.service
systemctl --user daemon-reload
```

### The general rule, stated once

- The OS scheduler's comparative advantage is that it survives the daemon (start at login,
  restart on death, log capture, install/uninstall). Give it that.
- The daemon's comparative advantage is that it can hold an arbitrary interval, compare
  wall-clock deadlines, and collapse a missed window into exactly one rotation with a
  comparison rather than a configuration key. Give it that.
- The one thing worth reconsidering later: if `docs/research/linux.md` or `windows.md` conclude
  that the daemon cannot run in the session at all, the calculus changes and an OS calendar
  timer becomes the only trigger that exists. That is a finding for those documents, not this
  one.

## Untested: what remains, and what would settle it

| behaviour | status | what would settle it |
|---|---|---|
| launchd `StartInterval` across real sleep | cited from the man page, **unverified here** | on the test macOS box: probe with `StartInterval` 60 and a log line, then `sudo pmset schedule wake "$(date -v+3M '+%m/%d/%y %H:%M:%S')"; sudo pmset sleepnow`, read the log after wake, then `sudo pmset schedule cancelall`. Not run here: it suspends the user's machine. |
| launchd `StartCalendarInterval` catch-up and coalescing across real sleep | cited, **unverified here** | same recipe with `StartCalendarInterval` on a minute that elapses while asleep; count runs (expect exactly one). |
| launchd fire latency after a calendar boundary | one sample: 5.39 s late (probe G2) | repeat the probe ten times and take the spread. |
| Every Task Scheduler claim | **unverified here** (no Windows host) | a Windows VM or PC: a 2-minute task, sleep the machine across the trigger, then read `Last Run Time` and `Microsoft-Windows-TaskScheduler/Operational`; repeat with `StartWhenAvailable` on and off, on battery and on AC. |
| Every systemd claim | **unverified here** (no Linux host) | a Linux box or VM: a `--user` timer with `OnCalendar=*:*:00`, `Persistent=true`, suspend for 5 minutes, and read `systemctl --user list-timers` plus the journal; then repeat after `loginctl disable-linger` and a logout. |
| Does a wallpaper API work with no session (Windows session 0, Linux without a session bus) | outside this card | `docs/research/windows.md` and `linux.md`. |

## Sources

[1] https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/ScheduledJobs.html
[2] https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html
[3] https://learn.microsoft.com/en-us/windows/win32/taskschd/taskschedulerschema-startwhenavailable-settingstype-element
[4] https://learn.microsoft.com/en-us/windows/win32/api/taskschd/nf-taskschd-itasksettings-get_startwhenavailable
[5] https://learn.microsoft.com/en-us/windows/win32/taskschd/taskschedulerschema-waketorun-settingstype-element
[6] https://learn.microsoft.com/en-us/windows/win32/api/taskschd/nf-taskschd-itasksettings-put_waketorun
[7] https://learn.microsoft.com/en-us/windows/win32/taskschd/task-maintenence
[8] https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/schtasks-create
[9] https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/schtasks-delete
[10] https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/schtasks-change
[11] https://learn.microsoft.com/en-us/powershell/module/scheduledtasks/new-scheduledtasksettingsset
[12] https://learn.microsoft.com/en-us/windows/win32/taskschd/taskschedulerschema-disallowstartifonbatteries-settingstype-element
[13] https://learn.microsoft.com/en-us/windows/win32/taskschd/taskschedulerschema-executiontimelimit-settingstype-element
[14] https://learn.microsoft.com/en-us/windows/win32/taskschd/taskschedulerschema-multipleinstancespolicy-settingstype-element
[15] https://learn.microsoft.com/en-us/windows/win32/taskschd/taskschedulerschema-restartonfailure-settingstype-element
[16] https://learn.microsoft.com/en-us/windows/win32/taskschd/tasksettings
[17] https://learn.microsoft.com/en-us/answers/questions/578167/why-is-my-scheduled-task-not-running-when-i-am-log
[18] https://www.freedesktop.org/software/systemd/man/latest/systemd.timer.html
[19] https://raw.githubusercontent.com/systemd/systemd/main/man/systemd.timer.xml
[20] https://www.freedesktop.org/software/systemd/man/latest/systemd.time.html
[21] https://raw.githubusercontent.com/systemd/systemd/main/man/systemd.time.xml
[22] https://www.freedesktop.org/software/systemd/man/latest/loginctl.html
[23] https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html
[24] https://raw.githubusercontent.com/systemd/systemd/main/man/systemd.service.xml
[25] https://raw.githubusercontent.com/systemd/systemd/main/man/logind.conf.xml
[26] https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html
[27] https://raw.githubusercontent.com/systemd/systemd/main/man/systemd.exec.xml
