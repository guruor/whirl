# Local launchd probes: raw observations

Machine: macOS 26.5.2 (build 25F84), Darwin 25.5.0, arm64 (Apple Silicon), 2026-09-25 IST.
Domain: gui/501 (the logged-in user's launchd context). All jobs below were bootstrapped
and booted out within this session; nothing from this research is left loaded.

The probe plists are next to this file (`com.whirl.research.probe*.plist`). Every observed
run was at the shell; the transcripts below are the command and its output, copied as printed.

## A: StartInterval 5 s, RunAtLoad true (log capture to a file)

```
$ launchctl bootstrap gui/501 ./com.whirl.research.probeA.plist; sleep 46
$ cat ~/whirl-probe-a.log
A fire 1790320481.996973000
A fire 1790320492.101600000
A fire 1790320502.209420000
A fire 1790320512.303008000
A fire 1790320522.401031000
```
Fires are 10.1 s apart, not 5 s; the first fire lands at the moment of load (RunAtLoad).

## B: StartInterval 1 s

```
$ cat ~/whirl-probe-b.log
B fire 1790320483.594999000
B fire 1790320493.802987000
B fire 1790320503.895214000
B fire 1790320514.012661000
B fire 1790320524.127843000
```
10.1 s apart: a 1 s interval is not honoured.

## F: StartInterval 2 s with ThrottleInterval 1 (throttle causation test)

```
$ cat ~/whirl-probe-f.log
F fire 1790320551.485065000
F fire 1790320553.615087000
F fire 1790320555.795092000
F fire 1790320557.908657000
F fire 1790320560.100222000
F fire 1790320562.203370000
F fire 1790320564.300782000
F fire 1790320566.368848000
F fire 1790320568.454172000
F fire 1790320570.518512000
F fire 1790320572.595738000
F fire 1790320574.660418000
F fire 1790320576.730489000
F fire 1790320578.813005000
F fire 1790320580.890293000
F fire 1790320582.959499000
F fire 1790320585.027066000
F fire 1790320587.096284000
F fire 1790320589.179877000
F fire 1790320591.255770000
F fire 1790320593.367545000
```
2.1 s apart. The 10 s floor in A and B is the documented default ThrottleInterval, not a
property of StartInterval itself.

## C: StartInterval 5 s with a body that runs 12 s (overlap)

```
$ cat ~/whirl-probe-c.log
C start 1790320549.286808000 pid=40152
C end   1790320561.426743000 pid=40152
C start 1790320566.490795000 pid=40960
C end   1790320578.574502000 pid=40960
C start 1790320583.683103000 pid=41458
```
No overlap: the second start comes 5.1 s after the first exit, not at the 5 s interval
boundary that elapsed while the first run was still going.

`launchctl print` while C was loaded:

```
gui/501/com.whirl.research.probeC = {
	active count = 1
	path = /Users/govind.rajpurohit/Workspace/Personal/whirl/.worktrees/t_b9e349e6/docs/research/probes/com.whirl.research.probeC.plist
	type = LaunchAgent
	state = running
	program = /bin/sh
	stdout path = /tmp/whirl-probe-c.out
	stderr path = /tmp/whirl-probe-c.err
	domain = gui/501 [100025]
	minimum runtime = 10
	exit timeout = 5
```

## E: KeepAlive true, body exits 0 immediately

```
$ cat ~/whirl-probe-e.log
E run 1790320482.437727000 pid=37461
E run 1790320492.523862000 pid=37747
E run 1790320502.632115000 pid=38058
E run 1790320512.707774000 pid=38361
E run 1790320522.830089000 pid=38780
```
Restarted forever, 10.1 s apart.

## I: KeepAlive true, body kills itself with SIGKILL

```
$ cat /tmp/whirl-probe-i.log
I run 1790320981.670714000 pid=51892
I run 1790320991.748067000 pid=52066
I run 1790321001.822548000 pid=52238
I run 1790321011.898378000 pid=52462

$ launchctl print gui/501/com.whirl.research.probeI | grep -E "state =|runs ="
	state = spawn scheduled
	runs = 4
```
A signal-killed job is restarted too, on the same 10 s throttle.
The same script used the legacy interface: `launchctl load -w /tmp/probe-i.plist` and
`launchctl unload -w /tmp/probe-i.plist` both still work on 26.5.2 (no error, job loaded
and removed), and `launchctl remove` removed the submitted leftovers.

## G2: StartCalendarInterval Minute=53, job left loaded (baseline)

```
now=2026-09-25T12:51:01.784871  Minute=53  expected fire=2026-09-25T12:53:00
$ python3 probe_g2.py
launchctl print:
  state = not running
  minimum runtime = 10
  runs = 1
  last exit code = 0
--- G2 log ---
G2 fire 1790320985.386784000
```
Start of the 12:53:00 minute is epoch 1790320980, so the single run started 5.39 s after the
calendar boundary. Note this is one sample: the man page quantifies nothing about the
sub-minute promptness of a calendar fire, and the delay is not explained by process start-up
alone.

## G and H: calendar firing that passes while the job is not registered

`probe_calendar.py` installed two calendar jobs: G fires at minute m+1 and stayed loaded,
H fires at minute m+2 and was booted out 8 s before its firing minute and bootstrapped again
20 s after it.

```
now=2026-09-25T12:47:04.374131  G minute=48 fires at 2026-09-25T12:48:00  H minute=49 fires at 2026-09-25T12:49:00
bootstrap G: (0, '')
bootstrap H: (0, '')
bootout H at 2026-09-25T12:48:52.125413 (before its 2026-09-25T12:49:00 fire) (0, '')
bootstrap H again at 2026-09-25T12:49:20.273812 (0, '')
  G print: state = not running
  G print: runs = 1
  G print: last exit code = 0
  H print: state = not running
  H print: runs = 0
  H print: last exit code = (never exited)
--- G log ---
G fire %s
--- H log ---
(no such file: the job never fired)
```
G fired once (runs = 1). H never ran: a calendar firing that elapsed while the job was not
registered is not replayed on the next load. (The G log line is garbled because the probe
template double-escaped its printf format; `runs = 1` and `last exit code = 0` are the
evidence for G. The G2 probe above is the clean version of the same test.)

## J: stdout/stderr capture and `launchctl kickstart`

```
$ launchctl bootstrap gui/501 /tmp/probe-j.plist
$ launchctl kickstart -k gui/501/com.whirl.research.probeJ
$ cat /tmp/whirl-probe-j.out
J stdout 1790321144
$ cat /tmp/whirl-probe-j.err
J stderr 1790321144
```
kickstart fires the job on demand; StandardOutPath and StandardErrorPath capture the job's
stdout and stderr verbatim.

## R: probe F re-run in the round-2 session (2026-09-25 13:04-13:10 IST)

The round-1 review could not re-run any launchd probe (its session refused
`launchctl bootstrap`), so it corroborated against the on-disk artifacts instead. This
section is that re-run, in a session where `launchctl bootstrap`/`bootout` were permitted.
It repeats probe F: `StartInterval 2` with `ThrottleInterval 1`.

`docs/research/probes/com.whirl.research.probeR.plist`, in full:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Probe R: re-run of probe F under the round-2 review request.
     StartInterval 2 with ThrottleInterval 1. Probe F measured a 2.1 s cadence,
     i.e. the throttle, not StartInterval, sets the floor. -->
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.whirl.research.probeR</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/sh</string>
    <string>-c</string>
    <string>date +%s.%N &gt;&gt; /tmp/whirl-probe-R.log</string>
  </array>
  <key>StartInterval</key>
  <integer>2</integer>
  <key>ThrottleInterval</key>
  <integer>1</integer>
  <key>RunAtLoad</key>
  <false/>
  <key>StandardOutPath</key>
  <string>/tmp/whirl-probe-R.out</string>
  <key>StandardErrorPath</key>
  <string>/tmp/whirl-probe-R.err</string>
</dict>
</plist>
```

Run 1, first bootstrap in this session:

```
$ launchctl bootstrap gui/501 docs/research/probes/com.whirl.research.probeR.plist
bootstrap exit=0
1790321692.899995000          <- date, immediately after bootstrap
$ sleep 25; cat /tmp/whirl-probe-R.log
1790321694.941828000
1790321696.998041000
1790321699.065373000
1790321701.127036000
1790321703.166000000
1790321705.238234000
1790321707.306919000
1790321709.370396000
1790321711.435752000
1790321713.481855000
1790321715.547908000
1790321717.616484000
1790321719.688871000
$ launchctl list | grep -i whirl
-  0  com.whirl.research.probeR
```

13 fires, 2.04-2.07 s apart. RunAtLoad is false here, so the first fire is a real interval
firing, not a load-time run.

Runs 2 and 3 (after `bootout`, to check it reproduces):

```
$ launchctl print gui/501/com.whirl.research.probeR | grep -E "state =|runs =|minimum runtime|path ="
path = /private/tmp/probeR.plist
state = not running
minimum runtime = 1
runs = 1
$ cat /tmp/whirl-probe-R.log
1790321790.884169000
1790321792.956749000
1790321795.028331000
1790321797.082115000
1790321799.153640000
1790321801.217735000
1790321803.341497000
1790321808.419345000          <- run 3 starts; the 5.08 s jump below is the reload gap
1790321810.492653000
1790321812.565496000
1790321814.640365000
1790321816.704919000
1790321818.775262000
$ awk '{if(NR>1) printf "%.6f\n", $1-p; p=$1}' /tmp/whirl-probe-R.log
2.072580
2.071582
2.053784
2.071525
2.064095
2.123762
5.077848                      <- between run 2's last fire and run 3's first fire (reload gap)
2.073308
2.072843
2.074869
2.064554
2.070343
```

`launchctl print` reports `minimum runtime = 1`, which is exactly the `ThrottleInterval`
this plist sets, and the observed cadence is that floor plus ~60 ms of process overhead.
That is the same conclusion probe F reached: the throttle, not `StartInterval`, sets the
floor when `ThrottleInterval` is below it.

One observed launchd quirk, recorded because a future reader will hit it: after
`launchctl bootout` of this label, bootstrapping the *same plist at the same path* failed
twice with `Bootstrap failed: 5: Input/output error`, while the identical plist copied to
`/private/tmp/probeR.plist` bootstrapped fine, and the worktree path was accepted again some
50 s later. The failure is not reproducible on demand and did not affect the observation
above (the run used for the cadence numbers is run 1, which bootstrapped first). Treat
"Input/output error" from `launchctl bootstrap` as a retry-after-bootout symptom, not as a
verdict on the plist.

### Source-link re-check for the Windows API pages cited as [4] and [6]

The round-1 review reported these two URLs as 404 with the slug `itasettings`. The committed
text uses the double-s slug. Fetched from this machine, in this session:

```
$ curl -s -o /dev/null -w "%{http_code}\n" \
  https://learn.microsoft.com/en-us/windows/win32/api/taskschd/nf-taskschd-itasksettings-get_startwhenavailable
200
$ curl -s -o /dev/null -w "%{http_code}\n" \
  https://learn.microsoft.com/en-us/windows/win32/api/taskschd/nf-taskschd-itasksettings-put_waketorun
200
$ curl -s -o /dev/null -w "%{http_code}\n" \
  https://learn.microsoft.com/en-us/windows/win32/api/taskschd/nf-taskschd-itasettings-get_startwhenavailable
404
```

Same result with and without a User-Agent header. `sed -n '433p;435p' scheduling.md | od -c`
shows `i t a k s e t t i n g s` in both lines, and the two evidence files
(`evidence/microsoft-startwhenavailable.txt`, `evidence/microsoft-waketorun.txt`) carry the
same double-s slug on their line 3. So the URLs in the committed artifact resolve 200; the
404 belongs to the single-s spelling, which appears nowhere in the branch (checked with
`grep -rn "nf-taskschd-ita"` across `docs/`).

## Cleanup

```
$ launchctl remove com.whirl.probe2   # leftover submitted job from an earlier run of this card
removed stale com.whirl.probe2
$ launchctl remove com.whirl.probe4   # ditto
removed stale com.whirl.probe4
$ launchctl bootout gui/501/com.whirl.research.probeR   # probe R, round 2
exit=0
$ launchctl list | grep -i whirl
none
$ ls ~/Library/LaunchAgents/ | grep -i whirl
none
```

Probe R was bootstrapped from its file in this repository and from a copy in `/tmp`; no plist
was ever copied into `~/Library/LaunchAgents`, so `bootout` removes it completely. Left on
disk and harmless: `/tmp/whirl-probe-R.log`, `/tmp/whirl-probe-R.out`,
`/tmp/whirl-probe-R.err`, `/private/tmp/probeR.plist` and the earlier `~/whirl-probe-*` logs.
No whirl job is loaded on this machine.
