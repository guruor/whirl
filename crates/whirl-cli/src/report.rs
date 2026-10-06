//! `whirl report`: one checklist run becomes a report file and a release row.
//!
//! A release waits on the per-platform checklist rows of
//! `docs/releases/vX.Y.Z.md`, and those rows are produced on real hardware this
//! repository's CI cannot reach (`docs/development.md` section 3, "What CI cannot
//! prove"). Until this verb, the person who ran a checklist typed the row by
//! hand, from a run whose evidence already existed on disk. This emitter turns
//! that run into the record instead.
//!
//! It ships in `whirl` itself, one of the three binaries under test, so the
//! person on the machine under test needs no checkout, no toolchain and no
//! interpreter: the same binary they are verifying writes the report. Nothing
//! here opens a socket, reads a config, or reaches the network.
//!
//! The rule that decides every judgement call ([`run`]): **verdicts come from
//! the run, and an item the run did not exercise is not a pass.** An item with
//! no observation is reported `not-run`; an observation that claims a verdict
//! without evidence is refused rather than printed; a required field the caller
//! did not supply is a refusal naming it, never a blank, a `0` or a plausible
//! default. The two artefacts come from one invocation:
//!
//! - `--out <path>`: the machine-readable report, one JSON document.
//! - `--row <path>`: the platform's section of `docs/releases/vX.Y.Z.md`,
//!   exactly as `.github/release-notes-template.md` requires it, so pasting it
//!   into the notes needs no editing.
//!
//! Exit status: `0` the run completed and both files were written; `1` the run
//! could not be made (a required field is missing, an item is unknown, the log
//! cannot be read, an output path exists already); `3` the command line itself
//! was wrong. That split is the one thing a caller scripts against: `0` means
//! two files exist.
//!
//! What this emitter deliberately does not do: it does not run a probe, and it
//! cannot. The person on the machine under test supplies the per-item verdicts
//! and their evidence in the observations file, and this binary normalises them,
//! checks them against the checklist, digests the log, and writes the two
//! documents. A verdict, a timestamp, a machine name and a log digest are never
//! invented here; each is either taken from the caller's run or the run is
//! refused.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use crate::render::{EXIT_OK, EXIT_REFUSED, EXIT_USAGE};
use whirl_core::config::Backend;
use whirl_core::config::json::{self, Node, Value};
use whirl_core::protocol::Sha256;

/// The report's own schema version. It moves only with a change to the shape of
/// the machine-readable document, which is what `report_schema` lets a later
/// reader detect instead of guessing.
const REPORT_SCHEMA: u32 = 1;

/// The text of `--help` for this verb, printed by the caller's usage text and
/// here so the flag list has one home.
pub const USAGE: &str = "\
usage: whirl report <flags>

  --checklist <macos|windows|linux>  which checklist the run exercised
  --observations <path>              the run's per-item verdicts and evidence
  --log <path>                       the log the run wrote (path and sha256 go in the report)
  --build-version <version>          the build under test (never guessed)
  --build-commit <commit>            the build under test (never guessed)
  --backend <native|noop>            the backend the run exercised
  --machine <name>                   the machine, as the runner names it
  --os-version <version>             the platform version, as the platform reports it
  --by <name>                        who ran the checklist (never read from the environment)
  --at <YYYY-MM-DDThh:mm:ssZ>        when the run finished, RFC 3339
  --out <path>                       write the machine-readable report here
  --row <path>                       write the platform's release-notes row here

macOS only:
  --signed <yes|no>                  what the shipped artifact is
  --first-run <text>                 with --signed no: what a first run costs the user

Windows only:
  --smartscreen <text>               the unsigned build's first-run warning, or why there is none

The exit status says whether the run was made: 0 completed, 1 refused, 3 the command line was wrong.";

/// One checklist item: its id, exactly as the row and the checklist's evidence
/// table spell it, and a short title for the row.
#[derive(Debug)]
struct Item {
    id: &'static str,
    what: &'static str,
}

/// The extra bullet the template's section for a platform carries beyond the
/// run line, and the flags that fill it.
#[derive(Debug)]
enum Extra {
    /// `- signed and notarized: <yes | no>. ... first-run ...`.
    Macos,
    /// `- the unsigned build's SmartScreen first-run warning: ...`.
    Windows,
    /// A row with no bullet beyond the run line.
    None,
}

/// One platform's checklist: the research note it lives in, the template's
/// checklist label, and every item the note lists. The item ids are the ones the
/// existing release rows already use, so an emitted row is comparable with them
/// item by item instead of being a second vocabulary.
#[derive(Debug)]
struct Checklist {
    name: &'static str,
    /// The document the items live in, recorded in the report and the row.
    source: &'static str,
    /// The text after `checklist: ` in the template's row.
    label: &'static str,
    items: &'static [Item],
    extra: Extra,
}

const MACOS_ITEMS: &[Item] = &[
    Item {
        id: "V1",
        what: "the filesystem probes",
    },
    Item {
        id: "V2",
        what: "the provider names",
    },
    Item {
        id: "V3",
        what: "which store layers exist and which carry a `Desktop` slot",
    },
    Item {
        id: "V4",
        what: "the System Events route",
    },
    Item {
        id: "V5",
        what: "the `allSpaces` option",
    },
    Item {
        id: "V5b",
        what: "the `LastUse` churn across the store",
    },
    Item {
        id: "V5c",
        what: "a path that does not exist",
    },
    Item {
        id: "V5d",
        what: "that the store file is replaced rather than edited",
    },
    Item {
        id: "V6",
        what: "the `LastUse` age buckets",
    },
    Item {
        id: "V7",
        what: "a launchd-submitted job in the Aqua session",
    },
    Item {
        id: "V8",
        what: "the decoded store, with `Appearance` absent",
    },
    Item {
        id: "V9",
        what: "the sandbox comparison",
    },
    Item {
        id: "V10",
        what: "`desktoppr`'s install shape",
    },
    Item {
        id: "U1",
        what: "multi-display behaviour",
    },
    Item {
        id: "U2",
        what: "display hotplug and resolution change",
    },
    Item {
        id: "U3",
        what: "display sleep and wake",
    },
    Item {
        id: "U4",
        what: "a correctly signed sandboxed writer",
    },
    Item {
        id: "U5",
        what: "what wrote the stale SystemDefault, Displays[...] and Spaces[''] layers",
    },
    Item {
        id: "U6",
        what: "the pre-Sonoma path",
    },
    Item {
        id: "U7",
        what: "the semantics of the store's type",
    },
    Item {
        id: "C1",
        what: "the machine that has never run whirl before",
    },
];

const WINDOWS_ITEMS: &[Item] = &[
    Item {
        id: "W1",
        what: "per-monitor set, two monitors, different images",
    },
    Item {
        id: "W2",
        what: "monitor ID stability",
    },
    Item {
        id: "W3",
        what: "per-monitor styling",
    },
    Item {
        id: "W4",
        what: "virtual desktops, Windows 11",
    },
    Item {
        id: "W5",
        what: "virtual desktops, per-monitor conflict",
    },
    Item {
        id: "W6",
        what: "slideshow interaction",
    },
    Item {
        id: "W7",
        what: "Spotlight interaction",
    },
    Item {
        id: "W8",
        what: "release path",
    },
    Item {
        id: "W9",
        what: "resolution and DPI change",
    },
    Item {
        id: "W10",
        what: "cold boot and logon timing",
    },
    Item {
        id: "W11",
        what: "session 0, to confirm the negative",
    },
    Item {
        id: "W12",
        what: "SystemParametersInfoW against SetPosition",
    },
    Item {
        id: "W13",
        what: "slideshow resumption",
    },
];

const LINUX_ITEMS: &[Item] = &[
    Item {
        id: "gnome",
        what: "the GNOME section",
    },
    Item {
        id: "kde",
        what: "the KDE section",
    },
    Item {
        id: "sway",
        what: "the sway section",
    },
    Item {
        id: "hyprland",
        what: "the Hyprland section",
    },
    Item {
        id: "x11",
        what: "the generic X11 section",
    },
];

const MACOS: Checklist = Checklist {
    name: "macos",
    source: "docs/research/macos.md",
    label: "`docs/research/macos.md`, including the unverified list",
    items: MACOS_ITEMS,
    extra: Extra::Macos,
};

const WINDOWS: Checklist = Checklist {
    name: "windows",
    source: "docs/research/windows.md",
    label: "`docs/research/windows.md`, \"What must be tested on real Windows before release\"",
    items: WINDOWS_ITEMS,
    extra: Extra::Windows,
};

const LINUX: Checklist = Checklist {
    name: "linux",
    source: "docs/research/linux.md",
    label: "`docs/research/linux.md`, the section for each desktop environment verified",
    items: LINUX_ITEMS,
    extra: Extra::None,
};

const CHECKLISTS: &[&Checklist] = &[&MACOS, &WINDOWS, &LINUX];

/// The checklist that belongs to the platform this binary was built for.
/// `None` on a platform whirl does not ship a setter for, which is the same
/// refusal as asking for another platform's checklist.
fn platform_checklist() -> Option<&'static Checklist> {
    match std::env::consts::OS {
        "macos" => Some(&MACOS),
        "windows" => Some(&WINDOWS),
        "linux" => Some(&LINUX),
        _ => None,
    }
}

fn checklist_by_name(name: &str) -> Option<&'static Checklist> {
    CHECKLISTS.iter().copied().find(|list| list.name == name)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Pass,
    Fail,
    NotRun,
}

impl Verdict {
    fn as_str(self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Fail => "fail",
            Verdict::NotRun => "not-run",
        }
    }

    fn parse(text: &str) -> Option<Verdict> {
        match text {
            "pass" => Some(Verdict::Pass),
            "fail" => Some(Verdict::Fail),
            "not-run" => Some(Verdict::NotRun),
            _ => None,
        }
    }
}

/// One item's outcome, resolved against the checklist. `evidence` is `None`
/// only for a `not-run` item the run did not name a reason for; a `pass` or a
/// `fail` always carries evidence, because an observation without it is refused
/// before this type is built.
#[derive(Debug)]
struct ItemResult {
    item: &'static Item,
    verdict: Verdict,
    evidence: Option<String>,
    /// A correction to the probe's own harness, which a template cannot supply
    /// and which the existing handwritten rows carry. Carried, not dropped.
    correction: Option<String>,
    /// A caveat about what the item proves. Carried, not dropped.
    caveat: Option<String>,
}

/// One run, resolved. Every field is supplied by the caller: nothing here is
/// read from the environment, and nothing is defaulted to a plausible value.
#[derive(Debug)]
struct Run {
    checklist: &'static Checklist,
    platform: &'static str,
    build_version: String,
    build_commit: String,
    backend: Backend,
    machine: String,
    os_version: String,
    by: String,
    at: String,
    /// The `YYYY-MM-DD` prefix of `at`, which is the date the row carries.
    date: String,
    log_path: String,
    log_sha256: String,
    signed: Option<String>,
    first_run: Option<String>,
    smartscreen: Option<String>,
    out_path: PathBuf,
    row_path: PathBuf,
    items: Vec<ItemResult>,
}

/// `whirl report`. The argument list is everything after the verb.
pub fn run(args: &[String]) -> ExitCode {
    match build(args) {
        Ok(run) => match write(&run) {
            Ok(()) => {
                let counts = run.counts();
                println!("report: {}", run.out_path.display());
                println!("row: {}", run.row_path.display());
                println!(
                    "items: {} pass {}, fail {}, not run {}",
                    counts.total, counts.pass, counts.fail, counts.not_run
                );
                ExitCode::from(EXIT_OK)
            }
            Err(message) => {
                eprintln!("whirl: report: {message}");
                ExitCode::from(EXIT_REFUSED)
            }
        },
        Err(Refusal::Usage(message)) => {
            eprintln!("whirl: report: {message}");
            eprintln!("{USAGE}");
            ExitCode::from(EXIT_USAGE)
        }
        Err(Refusal::Run(message)) => {
            eprintln!("whirl: report: {message}");
            ExitCode::from(EXIT_REFUSED)
        }
    }
}

/// The two ways a report can fail to be made. The distinction is the exit code:
/// a malformed command line (3) is not the same failure as a run that cannot be
/// reported (1), and neither is a completed run (0).
#[derive(Debug)]
enum Refusal {
    Usage(String),
    Run(String),
}

struct Flags {
    pairs: Vec<(String, String)>,
}

impl Flags {
    fn parse(args: &[String]) -> Result<Flags, Refusal> {
        let mut pairs = Vec::new();
        let mut index = 0;
        while index < args.len() {
            let arg = &args[index];
            let Some(key) = arg.strip_prefix("--") else {
                return Err(Refusal::Usage(format!(
                    "unexpected argument `{arg}`; every input to `report` is a --flag"
                )));
            };
            if key.is_empty() {
                return Err(Refusal::Usage("`--` names no flag".to_string()));
            }
            let value = args.get(index + 1).ok_or_else(|| {
                Refusal::Usage(format!("--{key} needs a value, and none follows it"))
            })?;
            pairs.push((key.to_string(), value.clone()));
            index += 2;
        }
        Ok(Flags { pairs })
    }

    /// Take a required flag's value. Its absence is a refusal of the run, not a
    /// usage error: the command line was well formed and the run is incomplete.
    fn require(&mut self, key: &str) -> Result<String, Refusal> {
        match self.take(key) {
            Some(value) => Ok(value),
            None => Err(Refusal::Run(format!(
                "the run has no --{key}: it is required, and the emitter will not guess a value for it"
            ))),
        }
    }

    fn optional(&mut self, key: &str) -> Option<String> {
        self.take(key)
    }

    fn take(&mut self, key: &str) -> Option<String> {
        let position = self.pairs.iter().position(|(name, _)| name == key)?;
        Some(self.pairs.remove(position).1)
    }

    /// Anything left was not a flag this verb knows, which is a wrong command
    /// line rather than a refusal of the run.
    fn done(&self) -> Result<(), Refusal> {
        match self.pairs.first() {
            Some((name, _)) => Err(Refusal::Usage(format!(
                "--{name} is not a flag of `whirl report`"
            ))),
            None => Ok(()),
        }
    }

    /// A non-empty value, or a refusal naming the flag. An empty string is a
    /// blank, and a blank is what this verb exists to refuse.
    fn require_text(&mut self, key: &str) -> Result<String, Refusal> {
        let value = self.require(key)?;
        if value.trim().is_empty() {
            return Err(Refusal::Run(format!("--{key} is empty")));
        }
        Ok(value)
    }
}

fn build(args: &[String]) -> Result<Run, Refusal> {
    let mut flags = Flags::parse(args)?;

    let checklist_name = flags.require_text("checklist")?;
    let checklist = checklist_by_name(&checklist_name).ok_or_else(|| {
        Refusal::Run(format!(
            "unknown checklist `{checklist_name}`; known checklists are {}",
            CHECKLISTS
                .iter()
                .map(|list| list.name)
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })?;
    // The emitter runs on the machine under test, so the checklist it is asked
    // for is the platform it is on. Asking for another platform's checklist is a
    // refusal rather than a report full of fields this machine has no value for.
    match platform_checklist() {
        Some(platform) if platform.name == checklist.name => {}
        Some(platform) => {
            return Err(Refusal::Run(format!(
                "the {} checklist cannot be reported from a {} machine; this binary is on {}",
                checklist.name,
                std::env::consts::OS,
                platform.name
            )));
        }
        None => {
            return Err(Refusal::Run(format!(
                "this build has no checklist for the platform it runs on ({}); whirl ships a setter for macOS, Windows and Linux only",
                std::env::consts::OS
            )));
        }
    }

    let observations = flags.require_text("observations")?;
    let log = flags.require_text("log")?;
    let build_version = flags.require_text("build-version")?;
    let build_commit = flags.require_text("build-commit")?;
    let machine = flags.require_text("machine")?;
    let os_version = flags.require_text("os-version")?;
    let by = flags.require_text("by")?;
    let at = flags.require_text("at")?;
    let date = rfc3339_date(&at).ok_or_else(|| {
        Refusal::Run(format!(
            "--at `{at}` is not an RFC 3339 timestamp; the run's time is the caller's to know, and this emitter will not stamp one"
        ))
    })?;

    let backend_name = flags.require_text("backend")?;
    let backend = Backend::parse(&backend_name).ok_or_else(|| {
        Refusal::Run(format!(
            "--backend `{backend_name}` is not a backend; the run exercised native or noop"
        ))
    })?;

    // The template's section carries one platform-specific bullet; each of its
    // flags is required exactly where the template needs it, and refused where
    // it would fill nothing.
    let (signed, first_run, smartscreen) = match checklist.extra {
        Extra::Macos => {
            let signed = flags.require_text("signed")?;
            let first_run = match signed.as_str() {
                "yes" => {
                    if flags.optional("first-run").is_some() {
                        return Err(Refusal::Run(
                            "--first-run is for an unsigned artifact (`--signed no`) and this run is signed"
                                .to_string(),
                        ));
                    }
                    None
                }
                "no" => Some(flags.require_text("first-run")?),
                other => {
                    return Err(Refusal::Run(format!(
                        "--signed `{other}` is neither yes nor no"
                    )));
                }
            };
            (Some(signed), first_run, None)
        }
        Extra::Windows => {
            if flags.optional("signed").is_some() {
                return Err(Refusal::Usage(
                    "--signed is for the macOS checklist; the Windows row needs --smartscreen"
                        .to_string(),
                ));
            }
            let smartscreen = flags.require_text("smartscreen")?;
            (None, None, Some(smartscreen))
        }
        Extra::None => {
            if flags.optional("signed").is_some() || flags.optional("smartscreen").is_some() {
                return Err(Refusal::Usage(format!(
                    "the {} row has no signed or SmartScreen bullet",
                    checklist.name
                )));
            }
            (None, None, None)
        }
    };

    let out = PathBuf::from(flags.require_text("out")?);
    let row = PathBuf::from(flags.require_text("row")?);
    flags.done()?;

    let log_path = PathBuf::from(&log);
    let log_sha256 = digest(&log_path).ok_or_else(|| {
        Refusal::Run(format!(
            "the log at `{log}` cannot be read, so the report cannot carry its digest; a run that wrote no log is a run that cannot be reported"
        ))
    })?;

    let items = read_observations(&observations, checklist)?;

    Ok(Run {
        checklist,
        platform: std::env::consts::OS,
        build_version,
        build_commit,
        backend,
        machine,
        os_version,
        by,
        at,
        date,
        // The path is recorded as the caller named it, not canonicalised: the
        // report describes the run the caller made, not this process's view.
        log_path: log,
        log_sha256,
        signed,
        first_run,
        smartscreen,
        out_path: out,
        row_path: row,
        items,
    })
}

/// The `YYYY-MM-DD` prefix of an RFC 3339 timestamp, or `None` when the value
/// is not one. The date is taken from the string rather than from a clock: the
/// run's date is the run's, and this emitter has no way to know it otherwise.
fn rfc3339_date(at: &str) -> Option<String> {
    let bytes = at.as_bytes();
    if bytes.len() < 10 {
        return None;
    }
    for (index, byte) in bytes.iter().enumerate().take(10) {
        let expected_separator = matches!(index, 4 | 7);
        if expected_separator {
            if *byte != b'-' {
                return None;
            }
        } else if !byte.is_ascii_digit() {
            return None;
        }
    }
    if bytes.len() > 10 && bytes[10] != b'T' && bytes[10] != b't' {
        return None;
    }
    Some(at[..10].to_string())
}

/// The sha256 of the file at `path`, hex, or `None` when it cannot be read.
fn digest(path: &std::path::Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Some(hasher.hex())
}

/// Read the run's observations and resolve them against the checklist. The
/// checklist is the authority on which items exist and on what a verdict means:
/// an unknown item is a refusal, an item the run did not name is `not-run`, and
/// a claimed verdict with no evidence is refused rather than printed.
fn read_observations(path: &str, checklist: &Checklist) -> Result<Vec<ItemResult>, Refusal> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        Refusal::Run(format!(
            "the observations at `{path}` cannot be read: {error}"
        ))
    })?;
    let root = json::parse(&text).map_err(|error| {
        Refusal::Run(format!(
            "the observations at `{path}` are not JSON: {error}"
        ))
    })?;
    let object = root.as_object().ok_or_else(|| {
        Refusal::Run(format!(
            "the observations at `{path}` are a {}, not an object",
            root.kind_name()
        ))
    })?;

    for (key, _) in object {
        if key != "checklist" && key != "items" {
            return Err(Refusal::Run(format!(
                "the observations at `{path}` carry an unknown key `{key}`; only `checklist` and `items` are read"
            )));
        }
    }

    if let Some((_, node)) = object.iter().find(|(key, _)| key == "checklist") {
        let named = node.as_str().ok_or_else(|| {
            Refusal::Run(format!(
                "the observations `checklist` is a {}, not a string",
                node.kind_name()
            ))
        })?;
        if named != checklist.name {
            return Err(Refusal::Run(format!(
                "the observations are for the `{named}` checklist and --checklist names `{}`",
                checklist.name
            )));
        }
    }

    let (_, items_node) = object.iter().find(|(key, _)| key == "items").ok_or_else(|| {
        Refusal::Run(format!(
            "the observations at `{path}` carry no `items`: a run with no observations is a run with nothing to report, and this is stated rather than emitted as an empty report"
        ))
    })?;
    let entries = items_node.as_array().ok_or_else(|| {
        Refusal::Run(format!(
            "the observations `items` is a {}, not an array",
            items_node.kind_name()
        ))
    })?;

    let mut observed: Vec<ItemResult> = Vec::new();
    for entry in entries {
        let record = observe(entry, checklist)?;
        if observed.iter().any(|seen| seen.item.id == record.item.id) {
            return Err(Refusal::Run(format!(
                "the item `{}` is observed twice; one item has one verdict",
                record.item.id
            )));
        }
        observed.push(record);
    }

    // The checklist decides the order and the membership: every item the run
    // exercised carries its verdict, and every item it did not is `not-run`.
    let mut results = Vec::with_capacity(checklist.items.len());
    for item in checklist.items {
        match observed.iter().position(|seen| seen.item.id == item.id) {
            Some(position) => results.push(observed.remove(position)),
            None => results.push(ItemResult {
                item,
                verdict: Verdict::NotRun,
                evidence: None,
                correction: None,
                caveat: None,
            }),
        }
    }
    Ok(results)
}

/// One entry of the observations array, resolved.
fn observe(entry: &json::Node, checklist: &Checklist) -> Result<ItemResult, Refusal> {
    let object = entry.as_object().ok_or_else(|| {
        Refusal::Run(format!(
            "an observation is a {}, not an object",
            entry.kind_name()
        ))
    })?;
    for (key, _) in object {
        if !matches!(
            key.as_str(),
            "item" | "verdict" | "evidence" | "correction" | "caveat"
        ) {
            return Err(Refusal::Run(format!(
                "an observation carries an unknown key `{key}`; the keys are item, verdict, evidence, correction, caveat"
            )));
        }
    }
    let field = |key: &str| {
        object
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, node)| node)
    };

    let id = field("item")
        .and_then(json::Node::as_str)
        .ok_or_else(|| Refusal::Run("an observation names no `item`".to_string()))?;
    let item = checklist
        .items
        .iter()
        .find(|candidate| candidate.id == id)
        .ok_or_else(|| {
            Refusal::Run(format!(
                "unknown checklist item `{id}`: it is not in the {} checklist ({})",
                checklist.name, checklist.source
            ))
        })?;

    let verdict_text = field("verdict")
        .and_then(json::Node::as_str)
        .ok_or_else(|| Refusal::Run(format!("the item `{id}` names no `verdict`")))?;
    let verdict = Verdict::parse(verdict_text).ok_or_else(|| {
        Refusal::Run(format!(
            "the item `{id}` has verdict `{verdict_text}`; known verdicts are pass, fail, not-run"
        ))
    })?;

    let text = |key: &str| -> Result<Option<String>, Refusal> {
        match field(key) {
            None => Ok(None),
            Some(node) => {
                let value = node.as_str().ok_or_else(|| {
                    Refusal::Run(format!(
                        "the item `{id}`'s `{key}` is a {}, not a string",
                        node.kind_name()
                    ))
                })?;
                if value.trim().is_empty() {
                    return Err(Refusal::Run(format!(
                        "the item `{id}`'s `{key}` is empty; a blank is not a value this report will carry"
                    )));
                }
                Ok(Some(value.to_string()))
            }
        }
    };

    let evidence = text("evidence")?;
    // The rule: an item the run did not exercise is not a pass. A verdict of
    // pass or fail rests on evidence, so an observation that claims one without
    // evidence is refused rather than printed as a result.
    if verdict != Verdict::NotRun && evidence.is_none() {
        return Err(Refusal::Run(format!(
            "the item `{id}` is reported `{}` with no evidence; an item with no evidence is not a pass, and this emitter will not print one",
            verdict.as_str()
        )));
    }

    Ok(ItemResult {
        item,
        verdict,
        evidence,
        correction: text("correction")?,
        caveat: text("caveat")?,
    })
}

/// The counts per verdict, computed from the resolved items and from nothing
/// else. This is what a reader of the report reads back.
struct Counts {
    pass: usize,
    fail: usize,
    not_run: usize,
    total: usize,
}

impl Run {
    /// The log's own name, which is what the public row carries: the release
    /// notes are a published document and a tester's absolute path under their
    /// home is not something they carry (`docs/development.md` section 6, "No
    /// secrets, no hostnames from a private network, no personal paths"). The
    /// machine-readable report keeps the path the caller named, because a reader
    /// of the report is the person who has the log.
    fn log_name(&self) -> &str {
        std::path::Path::new(&self.log_path)
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or(&self.log_path)
    }

    fn counts(&self) -> Counts {
        let mut counts = Counts {
            pass: 0,
            fail: 0,
            not_run: 0,
            total: self.items.len(),
        };
        for item in &self.items {
            match item.verdict {
                Verdict::Pass => counts.pass += 1,
                Verdict::Fail => counts.fail += 1,
                Verdict::NotRun => counts.not_run += 1,
            }
        }
        counts
    }

    /// The machine-readable report, as a JSON document.
    fn report_json(&self) -> String {
        let counts = self.counts();
        let items: Vec<Node> = self
            .items
            .iter()
            .map(|item| {
                let mut fields: Vec<(String, Node)> = vec![
                    field("id", item.item.id),
                    field("what", item.item.what),
                    field("verdict", item.verdict.as_str()),
                ];
                if let Some(evidence) = &item.evidence {
                    fields.push(field("evidence", evidence));
                }
                if let Some(correction) = &item.correction {
                    fields.push(field("correction", correction));
                }
                if let Some(caveat) = &item.caveat {
                    fields.push(field("caveat", caveat));
                }
                object(fields)
            })
            .collect();

        let mut root: Vec<(String, Node)> = vec![
            ("report_schema".to_string(), number(REPORT_SCHEMA as usize)),
            ("checklist".to_string(), text(self.checklist.name)),
            ("checklist_source".to_string(), text(self.checklist.source)),
            ("platform".to_string(), text(self.platform)),
            ("platform_version".to_string(), text(&self.os_version)),
            ("machine".to_string(), text(&self.machine)),
            ("backend".to_string(), text(self.backend.as_str())),
            (
                "build".to_string(),
                object(vec![
                    ("product".to_string(), text(whirl_core::protocol::PRODUCT)),
                    ("version".to_string(), text(&self.build_version)),
                    ("commit".to_string(), text(&self.build_commit)),
                ]),
            ),
            ("run_by".to_string(), text(&self.by)),
            ("run_at".to_string(), text(&self.at)),
            (
                "log".to_string(),
                object(vec![
                    ("path".to_string(), text(&self.log_path)),
                    ("sha256".to_string(), text(&self.log_sha256)),
                ]),
            ),
            (
                "counts".to_string(),
                object(vec![
                    ("pass".to_string(), number(counts.pass)),
                    ("fail".to_string(), number(counts.fail)),
                    ("not_run".to_string(), number(counts.not_run)),
                    ("total".to_string(), number(counts.total)),
                ]),
            ),
            ("items".to_string(), array(items)),
        ];
        // The unsigned-artifact facts are the run's where the template needs
        // them, and absent where the platform's row has no such bullet.
        if let Some(signed) = &self.signed {
            root.push(("signed".to_string(), text(signed)));
            if let Some(first_run) = &self.first_run {
                root.push(("first_run".to_string(), text(first_run)));
            }
        }
        if let Some(smartscreen) = &self.smartscreen {
            root.push(("smartscreen".to_string(), text(smartscreen)));
        }

        let mut out = String::new();
        write_json(&mut out, &object(root), 0);
        out.push('\n');
        out
    }

    /// The platform's section of `docs/releases/vX.Y.Z.md`, exactly as
    /// `.github/release-notes-template.md` requires it: the heading, the
    /// checklist line, the platform's own bullet where it has one, the run line
    /// with every item and every reason, and nothing left for a human to edit.
    fn row_markdown(&self) -> String {
        let counts = self.counts();
        let mut out = String::new();
        out.push_str(&format!(
            "### {}\n\n",
            platform_heading(self.checklist.name)
        ));
        out.push_str(&format!("- checklist: {}\n", self.checklist.label));
        if let Some(signed) = &self.signed {
            out.push_str(&format!("- signed and notarized: {signed}."));
            if let Some(first_run) = &self.first_run {
                out.push_str(&format!(
                    " For an unsigned tarball, the first-run Gatekeeper step the user has to take: {first_run}"
                ));
            }
            out.push('\n');
        }
        out.push_str(&format!(
            "- run by {} on {}, {} on {}: {} of the checklist's items passed, {} failed, {} were not run. The {} backend was exercised; the build is {} {} at {}. Log: `{}` (sha256 `{}`).\n",
            self.by,
            self.machine,
            self.os_version,
            self.date,
            counts.pass,
            counts.fail,
            counts.not_run,
            self.backend.as_str(),
            whirl_core::protocol::PRODUCT,
            self.build_version,
            self.build_commit,
            self.log_name(),
            self.log_sha256,
        ));
        if let Some(smartscreen) = &self.smartscreen {
            out.push_str(&format!(
                "- the unsigned build's SmartScreen first-run warning: {smartscreen}\n"
            ));
        }

        for (verdict, heading) in [
            (Verdict::Pass, "passed"),
            (Verdict::Fail, "failed"),
            (Verdict::NotRun, "not run, with the reason for each"),
        ] {
            let matching: Vec<&ItemResult> = self
                .items
                .iter()
                .filter(|item| item.verdict == verdict)
                .collect();
            if matching.is_empty() {
                continue;
            }
            out.push_str(&format!("  - {heading}:\n"));
            for item in matching {
                out.push_str(&format!("    - [{}] {}", item.item.id, item.item.what));
                // The row names the item and, where the caller's evidence says
                // more than the item's own name, the evidence after it. Where
                // the two are the same string the row prints it once, which is
                // how the handwritten rows read.
                if let Some(evidence) = &item.evidence {
                    if evidence != item.item.what {
                        out.push_str(&format!(": {evidence}"));
                    }
                }
                if let Some(correction) = &item.correction {
                    out.push_str(&format!(". Correction to this item's probe: {correction}"));
                }
                if let Some(caveat) = &item.caveat {
                    out.push_str(&format!(". Caveat: {caveat}"));
                }
                out.push('\n');
            }
        }
        out
    }
}

fn platform_heading(name: &str) -> &'static str {
    match name {
        "macos" => "macOS",
        "windows" => "Windows",
        _ => "Linux",
    }
}

fn text(value: impl Into<String>) -> Node {
    Node {
        value: Value::Str(value.into()),
        line: 0,
    }
}

fn number(value: usize) -> Node {
    Node {
        value: Value::Num(value as f64),
        line: 0,
    }
}

fn field(key: &str, value: &str) -> (String, Node) {
    (key.to_string(), text(value))
}

fn object(fields: Vec<(String, Node)>) -> Node {
    Node {
        value: Value::Obj(fields),
        line: 0,
    }
}

fn array(items: Vec<Node>) -> Node {
    Node {
        value: Value::Arr(items),
        line: 0,
    }
}

/// A JSON writer for the tree `report_json` builds. The reader is
/// `whirl-core::config::json`; a second reader would be a defect
/// (docs/development.md section 1), but a writer has no second home.
fn write_json(out: &mut String, node: &Node, indent: usize) {
    match &node.value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Num(value) => {
            if value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 {
                out.push_str(&format!("{}", *value as i64));
            } else {
                out.push_str(&format!("{value}"));
            }
        }
        Value::Str(text) => write_json_string(out, text),
        Value::Arr(elements) => {
            if elements.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (index, element) in elements.iter().enumerate() {
                indent_into(out, indent + 1);
                write_json(out, element, indent + 1);
                if index + 1 < elements.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            indent_into(out, indent);
            out.push(']');
        }
        Value::Obj(entries) => {
            if entries.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (index, (key, entry)) in entries.iter().enumerate() {
                indent_into(out, indent + 1);
                write_json_string(out, key);
                out.push_str(": ");
                write_json(out, entry, indent + 1);
                if index + 1 < entries.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            indent_into(out, indent);
            out.push('}');
        }
    }
}

fn indent_into(out: &mut String, indent: usize) {
    for _ in 0..indent {
        out.push_str("  ");
    }
}

fn write_json_string(out: &mut String, text: &str) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", control as u32));
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

/// Write both documents, and prove neither path already existed before either
/// is written. The two outputs are one run's, so a refusal to write the second
/// removes the first rather than leaving half a report behind.
fn write(run: &Run) -> Result<(), String> {
    let report = run.report_json();
    let markdown = run.row_markdown();

    create_new(&run.row_path, &markdown)?;
    match create_new(&run.out_path, &report) {
        Ok(()) => Ok(()),
        Err(message) => {
            let _ = std::fs::remove_file(&run.row_path);
            Err(message)
        }
    }
}

/// Write `contents` to a path that must not exist. `create_new` is the
/// guarantee: it fails rather than truncate, so an existing report is never
/// overwritten even if it appeared between a check and a write.
fn create_new(path: &std::path::Path, contents: &str) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                format!(
                    "`{}` exists; a report is written once and never over another",
                    path.display()
                )
            } else {
                format!("`{}` cannot be written: {error}", path.display())
            }
        })?;
    file.write_all(contents.as_bytes())
        .map_err(|error| format!("`{}` cannot be written: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run with every required field, over a scratch directory this test owns.
    /// The values are synthetic on purpose: the tests below are about what the
    /// emitter does with a run, not about any machine's real results.
    struct Fixture {
        dir: PathBuf,
        log: String,
        observations: String,
    }

    fn fixture(name: &str, observations: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("whirl-report-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let log = dir.join("run.log");
        std::fs::write(&log, b"one probe's output\n").expect("a log file");
        let observations_path = dir.join("observations.json");
        std::fs::write(&observations_path, observations).expect("an observations file");
        Fixture {
            log: log.to_string_lossy().into_owned(),
            observations: observations_path.to_string_lossy().into_owned(),
            dir,
        }
    }

    fn args(fixture: &Fixture, extra: &[&str]) -> Vec<String> {
        let platform = platform_checklist().expect("a checklist for this platform");
        let mut args: Vec<String> = [
            "--checklist",
            platform.name,
            "--observations",
            &fixture.observations,
            "--log",
            &fixture.log,
            "--build-version",
            "9.9.9",
            "--build-commit",
            "0123456789abcdef0123456789abcdef01234567",
            "--backend",
            "noop",
            "--machine",
            "test-machine",
            "--os-version",
            "0.0",
            "--by",
            "test-runner",
            "--at",
            "2026-01-02T03:04:05Z",
            "--out",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        args.push(
            fixture
                .dir
                .join("report.json")
                .to_string_lossy()
                .into_owned(),
        );
        args.push("--row".to_string());
        args.push(fixture.dir.join("row.md").to_string_lossy().into_owned());
        for value in extra {
            args.push((*value).to_string());
        }
        // macOS and Windows both need their extra bullet's flag; this test's
        // platform is whichever one it is compiled on.
        match platform.extra {
            Extra::Macos => {
                args.push("--signed".to_string());
                args.push("no".to_string());
                args.push("--first-run".to_string());
                args.push("clear the quarantine attribute".to_string());
            }
            Extra::Windows => {
                args.push("--smartscreen".to_string());
                args.push("More info, Run anyway".to_string());
            }
            Extra::None => {}
        }
        args
    }

    fn build_from(fixture: &Fixture, extra: &[&str]) -> Result<Run, Refusal> {
        build(&args(fixture, extra))
    }

    #[test]
    fn a_run_with_no_observations_for_an_item_reports_it_not_run() {
        // One item observed and passing; the checklist's other items are
        // supplied by no observation at all, and the rule is that they are not
        // passes: they are not-run.
        let fixture = fixture(
            "not-run",
            r#"{ "items": [ { "item": "U6", "verdict": "pass", "evidence": "observed" } ] }"#,
        );
        let run = build_from(&fixture, &[]).expect("a run with one observation");
        let unobserved = run
            .items
            .iter()
            .find(|item| item.item.id == "V1")
            .expect("V1 is in the checklist");
        assert_eq!(unobserved.verdict, Verdict::NotRun);
        assert_eq!(unobserved.evidence, None);
        let observed = run
            .items
            .iter()
            .find(|item| item.item.id == "U6")
            .expect("U6 is in the checklist");
        assert_eq!(observed.verdict, Verdict::Pass);
        // The count is the checklist's size, not the observations' size.
        let counts = run.counts();
        assert_eq!(counts.total, run.checklist.items.len());
        assert_eq!(counts.pass, 1);
        assert_eq!(counts.not_run, run.checklist.items.len() - 1);
    }

    #[test]
    fn a_verdict_with_no_evidence_is_refused_rather_than_printed() {
        let fixture = fixture(
            "no-evidence",
            r#"{ "items": [ { "item": "V1", "verdict": "pass" } ] }"#,
        );
        match build_from(&fixture, &[]) {
            Err(Refusal::Run(message)) => {
                assert!(message.contains("V1"), "{message}");
                assert!(message.contains("no evidence"), "{message}");
            }
            other => panic!("expected a refusal naming the missing evidence: {other:?}"),
        }
    }

    #[test]
    fn a_missing_build_identity_is_refused_and_named() {
        let fixture = fixture("no-build", r#"{ "items": [] }"#);
        let mut args = args(&fixture, &[]);
        // Drop `--build-commit <sha>`, and its value with it.
        let position = args
            .iter()
            .position(|arg| arg == "--build-commit")
            .expect("the flag is in the fixture");
        args.drain(position..position + 2);
        match build(&args) {
            Err(Refusal::Run(message)) => {
                assert!(message.contains("--build-commit"), "{message}");
            }
            other => panic!("expected a refusal naming the missing build identity: {other:?}"),
        }
    }

    #[test]
    fn an_unknown_checklist_item_is_refused_and_named() {
        let fixture = fixture(
            "unknown-item",
            r#"{ "items": [ { "item": "V99", "verdict": "pass", "evidence": "x" } ] }"#,
        );
        match build_from(&fixture, &[]) {
            Err(Refusal::Run(message)) => {
                assert!(message.contains("V99"), "{message}");
                assert!(message.contains("not in the"), "{message}");
            }
            other => panic!("expected a refusal naming the unknown item: {other:?}"),
        }
    }

    #[test]
    fn the_other_platform_s_checklist_is_refused() {
        let fixture = fixture("other-platform", r#"{ "items": [] }"#);
        let other = CHECKLISTS
            .iter()
            .find(|list| list.name != platform_checklist().expect("a checklist").name)
            .expect("a checklist that is not this platform's");
        let mut args = args(&fixture, &[]);
        let position = args
            .iter()
            .position(|arg| arg == "--checklist")
            .expect("the flag is in the fixture");
        args[position + 1] = other.name.to_string();
        match build(&args) {
            Err(Refusal::Run(message)) => {
                assert!(message.contains(other.name), "{message}");
            }
            other => panic!("expected a refusal for the wrong platform's checklist: {other:?}"),
        }
    }

    #[test]
    fn a_correction_and_a_caveat_reach_both_outputs() {
        let fixture = fixture(
            "annotations",
            r#"{ "items": [
                { "item": "V1", "verdict": "pass", "evidence": "ran",
                  "correction": "the probe's template ended in the wrong place",
                  "caveat": "this proves the route, not the placement" }
            ] }"#,
        );
        let run = build_from(&fixture, &[]).expect("a run");
        let report = run.report_json();
        assert!(
            report.contains("the probe's template ended in the wrong place"),
            "{report}"
        );
        assert!(
            report.contains("this proves the route, not the placement"),
            "{report}"
        );
        let markdown = run.row_markdown();
        assert!(
            markdown.contains("Correction to this item's probe:"),
            "{markdown}"
        );
        assert!(
            markdown.contains("the probe's template ended in the wrong place"),
            "{markdown}"
        );
        assert!(
            markdown.contains("Caveat: this proves the route, not the placement"),
            "{markdown}"
        );
    }

    #[test]
    fn the_row_carries_the_platform_s_heading_and_no_placeholder() {
        let fixture = fixture(
            "row",
            r#"{ "items": [ { "item": "V1", "verdict": "pass", "evidence": "ran" } ] }"#,
        );
        let run = build_from(&fixture, &[]).expect("a run");
        let markdown = run.row_markdown();
        let platform = platform_checklist().expect("a checklist");
        assert!(
            markdown.starts_with(&format!("### {}\n", platform_heading(platform.name))),
            "{markdown}"
        );
        assert!(markdown.contains("- checklist: "), "{markdown}");
        assert!(markdown.contains(&run.by), "{markdown}");
        assert!(
            markdown.contains("not run, with the reason for each"),
            "{markdown}"
        );
        // The release workflow refuses a final release whose notes still hold a
        // `<...>` placeholder. The row introduces none.
        assert!(
            !markdown.contains('<'),
            "the row carries no placeholder: {markdown}"
        );
        assert!(
            !markdown.contains('>'),
            "the row carries no placeholder: {markdown}"
        );
    }

    #[test]
    fn an_existing_output_is_refused_rather_than_overwritten() {
        let fixture = fixture("exists", r#"{ "items": [] }"#);
        std::fs::write(fixture.dir.join("report.json"), b"an earlier report")
            .expect("a decoy report");
        let args = args(&fixture, &[]);
        match build(&args) {
            Ok(run) => match write(&run) {
                Err(message) => {
                    assert!(message.contains("report.json"), "{message}");
                    assert!(message.contains("exists"), "{message}");
                }
                Ok(()) => panic!("expected the existing report to be refused"),
            },
            Err(other) => panic!("expected the run to build: {other:?}"),
        }
        // The decoy is left exactly as it was, and no row was written.
        assert_eq!(
            std::fs::read_to_string(fixture.dir.join("report.json")).expect("the decoy"),
            "an earlier report"
        );
        assert!(!fixture.dir.join("row.md").exists(), "no row was written");
    }

    #[test]
    fn a_completed_run_writes_both_files_and_the_counts_read_back() {
        let fixture = fixture(
            "complete",
            r#"{ "items": [ { "item": "V1", "verdict": "pass", "evidence": "ran" } ] }"#,
        );
        let args = args(&fixture, &[]);
        let run = build(&args).expect("a run");
        write(&run).expect("both files are written");

        let report = std::fs::read_to_string(fixture.dir.join("report.json")).expect("the report");
        let parsed = json::parse(&report).expect("the report is JSON");
        let counts = parsed
            .as_object()
            .expect("an object")
            .iter()
            .find(|(key, _)| key == "counts")
            .map(|(_, node)| node)
            .expect("a counts object");
        let count_of = |key: &str| {
            counts
                .as_object()
                .expect("an object")
                .iter()
                .find(|(name, _)| name == key)
                .and_then(|(_, node)| node.as_num())
                .expect("a count")
        };
        assert_eq!(count_of("pass"), 1.0);
        assert_eq!(count_of("fail"), 0.0);
        assert_eq!(
            count_of("not_run") + count_of("pass") + count_of("fail"),
            count_of("total")
        );
        assert!(
            fixture.dir.join("row.md").exists(),
            "the row is written too"
        );
    }

    #[test]
    fn a_log_that_cannot_be_read_is_refused() {
        let fixture = fixture("no-log", r#"{ "items": [] }"#);
        let mut args = args(&fixture, &[]);
        let position = args
            .iter()
            .position(|arg| arg == "--log")
            .expect("the flag");
        args[position + 1] = fixture
            .dir
            .join("does-not-exist.log")
            .to_string_lossy()
            .into_owned();
        match build(&args) {
            Err(Refusal::Run(message)) => {
                assert!(message.contains("cannot be read"), "{message}");
            }
            other => panic!("expected the missing log to be refused: {other:?}"),
        }
    }

    #[test]
    fn the_digest_is_the_log_s_own_bytes() {
        let fixture = fixture("digest", r#"{ "items": [] }"#);
        let run = build_from(&fixture, &[]).expect("a run");
        let mut hasher = Sha256::new();
        hasher.update(b"one probe's output\n");
        assert_eq!(run.log_sha256, hasher.hex());
        assert_eq!(run.log_sha256.len(), 64);
    }

    #[test]
    fn evidence_that_is_the_item_s_own_name_is_printed_once() {
        // The existing handwritten rows name an item and say no more about it
        // where the run has nothing to add (`[V1] the filesystem probes`). An
        // evidence string that is the item's own name prints the same way.
        let fixture = fixture(
            "print-once",
            r#"{ "items": [ { "item": "V1", "verdict": "pass", "evidence": "the filesystem probes" } ] }"#,
        );
        let run = build_from(&fixture, &[]).expect("a run");
        let markdown = run.row_markdown();
        assert!(
            markdown.contains("    - [V1] the filesystem probes\n"),
            "the row names the item once: {markdown}"
        );
        assert!(
            !markdown.contains("the filesystem probes: the filesystem probes"),
            "{markdown}"
        );
        // The report still carries the evidence as its own field.
        assert!(
            run.report_json()
                .contains("\"evidence\": \"the filesystem probes\""),
            "the report keeps the evidence it was given"
        );
    }

    #[test]
    fn a_failed_item_is_named_in_the_row_with_its_evidence() {
        let fixture = fixture(
            "failed",
            r#"{ "items": [ { "item": "V1", "verdict": "fail", "evidence": "the write returned an error the probe could not explain" } ] }"#,
        );
        let run = build_from(&fixture, &[]).expect("a run");
        let markdown = run.row_markdown();
        assert!(markdown.contains("  - failed:\n"), "{markdown}");
        assert!(
            markdown.contains("[V1] the filesystem probes: the write returned an error the probe could not explain"),
            "{markdown}"
        );
        assert_eq!(run.counts().fail, 1);
    }

    #[test]
    fn the_row_names_the_log_and_the_report_carries_its_path() {
        let fixture = fixture("log-name", r#"{ "items": [] }"#);
        let run = build_from(&fixture, &[]).expect("a run");
        let markdown = run.row_markdown();
        assert!(
            markdown.contains(&format!("Log: `run.log` (sha256 `{}`)", run.log_sha256)),
            "the row names the log by name: {markdown}"
        );
        assert!(
            !markdown.contains(&fixture.dir.to_string_lossy().to_string()),
            "the public row carries no absolute path: {markdown}"
        );
        assert!(
            run.report_json().contains(&fixture.log),
            "the report keeps the path the caller named"
        );
    }
}
