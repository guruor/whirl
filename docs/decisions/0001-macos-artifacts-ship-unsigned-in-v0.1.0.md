# 0001. Ship the v0.1.0 artifacts unsigned, on all three platforms

- **Status:** accepted
- **Date:** 2026-09-26
- **Deciders:** Guru (project owner, `guruor`)
- **Supersedes:** nothing

## Context

Three places in the repository disagreed about whether v0.1.0 signs its macOS
artifacts, and one of them contradicted itself:

- `docs/development.md` section 5, "Signing, notarization and packaging: what
  the research settled", decided for signing: "Decision: **v0.1 ships signed and
  notarized for the tarball and `.pkg`**, because the whole point of a wallpaper
  rotator is that it runs at login without a dialog". Three bullets earlier the
  same section said the opposite about what exists today: "The macOS artifacts
  are unsigned today, so the release notes have to say what a first run looks
  like".
- `docs/milestones.md`, "What is deliberately not in v0.1.0", lists "Packaging,
  code signing, notarization, installers, auto-update" as out of scope and says
  what ships: "What ships today is what `.github/workflows/release.yml` builds:
  three unsigned archives from `cargo build --workspace --release`."
- `.github/workflows/release.yml` has no signing and no notarization step, and the
  notes it assembles require none.

The facts the decision rests on are in `docs/research/macos.md` 8:

- **Signing is not what makes it run; notarization is what distribution needs.**
  A binary you built yourself, or one installed by a package manager, runs without
  a Gatekeeper prompt, and that is what `docs/development.md` section 5 already
  said before its contradictory decision sentence. What a Developer ID artifact
  distributed over the internet needs is notarization: "Beginning in macOS 10.15,
  all software built after June 1, 2019, and distributed with Developer ID must be
  notarized" [4]. Skipping notarization costs exactly one thing: "A Gatekeeper
  prompt on first run is the cost of skipping this, and there is no entitlement
  that avoids it."
- **The sandbox is not a way out of that prompt.** Building the write probe with
  `com.apple.security.app-sandbox` and signing it ad-hoc "kills it at launch,
  before a line of my code runs": `Trace/BPT trap: 5` (exit 133), with AMFI
  reporting "The file is adhoc signed or signed by an unknown certificate chain".
  The control, the same ad-hoc signature without the entitlement, "builds, signs,
  runs, sets the wallpaper, exit 0" ([V9]). The research drew the conclusion:
  "the cheapest route around the whole question is not to sandbox: whirl is a
  wallpaper rotator, it needs no App Store distribution".
- **The remaining route is priced and needs an account.** Developer ID signing
  plus notarization needs the Apple Developer Program membership, **99 USD per
  membership year** (Apple, "Membership Details", retrieved 2026-09-25), a
  certificate the maintainer holds, and a notarization step in the release
  workflow. The notary service takes "flat installer packages and disk images",
  so it is also the point at which a `.pkg` becomes worth building.

`docs/reviews/architecture-review.md` passes the current text and records the
vendor figures as dated and not re-fetched, so nothing in the repository has
contradicted either half; the two halves simply never met.

## Decision

Ship the v0.1.0 artifacts unsigned on all three platforms, exactly as
`.github/workflows/release.yml` builds them, and make the release notes carry the
macOS first-run step: that the download is unsigned and what the user does about
the Gatekeeper prompt. Developer ID signing plus notarization lands when an
artifact is handed to people who did not build it, and it is its own card, priced
at the membership above.

## Alternatives considered

- **Developer ID signing plus notarization for v0.1.0.** Lost on cost and order of
  work: 99 USD per membership year, a certificate, and a notarization step in the
  release workflow, to remove one dialog that the release notes can describe in
  two lines. It remains the route for distribution and becomes a card when the
  artifact is handed to someone who did not build it.
- **Ad-hoc signing, as a cheaper half of it.** Lost on the measurement: the probe
  is ad-hoc signed and the setter runs, so ad-hoc signing changes nothing a user
  sees, and it does not replace notarization for a downloaded artifact. It stays
  what contributors building locally need, which `docs/development.md` section 5
  already says.
- **App Sandbox, to get a quieter first run.** Lost on the measurement in the
  same section: `com.apple.security.app-sandbox` trapped the setter at launch
  (exit 133). It is not a lighter version of the same thing, it is a broken
  setter.
- **Building the `.pkg` or a `.dmg` for v0.1.0.** Lost: the notary service takes
  "flat installer packages and disk images", so an installer belongs with signing,
  not before it. The tarball and the zip are the install
  (`docs/development.md` section 5), and v0.1.0 ships both unsigned.
- **Leaving the three places as they were, with no ADR.** Lost: the contradiction
  is what forced this decision. `docs/development.md` promised signing while
  describing an unsigned workflow, and a reader could not tell which sentence the
  project meant.

## Consequences

- **Easier:** the release path holds no Apple account, no certificate and no
  notary submission, so the workflow stays what it says it is, and the three
  artifacts stay the three binaries and nothing else (`docs/development.md`
  section 5). A contributor can build, install and run without touching an
  Apple identity.
- **Harder:** the macOS first-run step exists only in the release notes, so notes
  that omit it hand the user a download they cannot open, and the cost of that
  mistake is a support question per user rather than a failing check.
- **Forbids:** a signing or notarization step in `.github/workflows/release.yml`
  for v0.1.0, and an installer in the v0.1.0 artifact set.
- **Reversed when:** an artifact is handed to people who did not build it, or
  download warnings become a support load. That needs an ADR that supersedes this
  one, and it carries the membership, the certificate and the workflow step with
  it.
- **Enforced by:** `.github/release-notes-template.md`, whose `### macOS` row
  requires the first-run step, and `docs/development.md` section 5, which names
  the artifacts unsigned. The `release` workflow requires the `### macOS` section
  to be present and a final release to have every placeholder filled, but it does
  not read the sentence inside the row: the wording is a human fill-in, and that
  is the one gap this decision leaves open.
- **Deliberately untouched:** `docs/milestones.md`, which is in flight on another
  branch and whose "not in v0.1.0" row already states that the archives are
  unsigned. It agrees with this decision as written and needs no edit.
