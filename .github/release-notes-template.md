<!--
The shape of a release's notes. The procedure is docs/development.md section 5.

This file is read in two ways:

- a final release, `vX.Y.Z`: copy it to `docs/releases/vX.Y.Z.md`, fill every
  placeholder, and land that file in the promotion pull request, so the notes and
  the tagged commit are the same commit. The `release` workflow refuses to publish
  a final release whose notes file is missing, unfilled, or missing a required
  section.
- a prerelease, `vX.Y.Z-rc.N`: nothing to write. The workflow renders this file
  itself, substituting <TAG>, <DATE> and <COMMIT>, and publishes it as a GitHub
  prerelease. The checklist rows stay placeholders, which is what the prerelease
  flag says out loud.

Required sections, checked by .github/workflows/release.yml and therefore by
docs/development.md:

  ## What changed
  ## Per-platform checklist results      (### macOS, ### Windows, ### Linux)
  ## Config and protocol changes
  ## Upgrades and known issues
-->

# whirl <TAG>

Released <DATE>, from commit <COMMIT> on `main`, built from the tag by
`.github/workflows/release.yml`.

Artifacts, one per platform, each holding the three binaries (`whirld`, `whirl`,
`whirl-worker`) and nothing else:

- `whirl-<TAG>-linux-x86_64.tar.gz`
- `whirl-<TAG>-macos-arm64.tar.gz`
- `whirl-<TAG>-windows-x86_64.zip`

## What changed

<what someone running the previous release would notice: behaviour, defaults,
fixes. Not a commit list.>

## Per-platform checklist results

CI cannot prove that a wallpaper changes (docs/development.md section 3, "What CI
cannot prove"), so these rows come from real hardware and name the person who ran
them. A release waits for the rows, not for a green pipeline.

### macOS

- checklist: `docs/research/macos.md`, including the unverified list
- signed and notarized: <yes | no, and for a tarball that is not, what the user has to do>
- run by <name> on <machine, macOS version> on <YYYY-MM-DD>: <result, and every
  item that failed or was not run>

### Windows

- checklist: `docs/research/windows.md`, "What must be tested on real Windows
  before release"
- run by <name> on <machine, Windows version> on <YYYY-MM-DD>: <result, and every
  item that failed or was not run>
- the unsigned build's SmartScreen first-run warning: <what to tell the user, or
  why this build does not raise one>

### Linux

- checklist: `docs/research/linux.md`, the section for each desktop environment
  verified
- run by <name> on <distribution, desktop environments and versions> on
  <YYYY-MM-DD>: <result, and which environments were not verified>

## Config and protocol changes

<config keys added, removed, or defaulted differently, with the old default; the
protocol version and what a stale client does; "none" when there are none.>

## Upgrades and known issues

<stop, replace, start, with the caveat that a running daemon holds its lock
(docs/development.md section 5); what is known broken in this release; what is
deferred.>
