//! The `wallhaven` source of docs/spec/features.md 2.3, with the key rule of
//! 2.4. One source, two modes: a **search** (`/api/v1/search`) and a
//! **collection** (`/api/v1/collections/<user>/<id>`).
//!
//! **What this source does not do: filter by resolution.** features.md 2.3 gives
//! every key a documented query parameter, and `atleast` is one — but `atleast`
//! is silently ignored on the collection endpoint, which was measured against
//! the live API (identical entries and an identical `meta.total` for
//! `atleast=2560x1440` and `atleast=3840x2160`, on both pages). So the floor is
//! the pipeline's, from `min_width`/`min_height` and `filters.*`
//! ([`crate::pipeline::stage_resolution`]), and this source yields **every**
//! entry with the API's own `dimension_x`/`dimension_y` so that the pipeline's
//! `rejected_resolution` counts what was really rejected instead of reporting a
//! floor the API never applied. `atleast` is still sent on `/search`, where it
//! works, which is what 2.5 means by "push down what the source can answer":
//! [`Wallhaven::capabilities`] declares `resolution` for a search and not for a
//! collection.
//!
//! **Two more things the live API does that features.md 2.4 does not say.** Both
//! were measured while writing this file, and both make the spec's own rule
//! (- a key is required, and its absence is an error -) more necessary rather
//! than less:
//!
//! - A keyless `purity=001` (NSFW only) request answered **200 with an empty
//!   `data[]`**, and `purity=111` answered 200 with SFW and sketchy entries and
//!   no NSFW ones. The documented `401 Unauthorized` did not appear. A silent
//!   downgrade is exactly the failure 2.4 calls out, so the source is refused at
//!   load when a key is needed and none resolves rather than trusting the API to
//!   say so.
//! - A request with **no `User-Agent` at all** answered 200, not the documented
//!   403. The client still sends a browser user agent (a bare programmatic
//!   client is what a WAF blocks, and the block is per-request and not per-code
//!   path), and any 403 that does arrive is the named
//!   [`SourceErrorKind::Forbidden`].
//!
//! **The one configuration ambiguity.** features.md 2.3 says `collection` "Uses
//! `/api/v1/collections/USERNAME/ID` instead of search", so when a config sets
//! both `collection` and `query` the collection wins and `query` is not sent.
//! That is a line printed at every enumeration rather than a silent choice:
//! silence about a filter that is not being applied is the failure mode 2.4
//! refuses for the key, and it is the same failure.
//!
//! **Rate limiting is not implemented, and this is the one place the number
//! lives.** [`REQUESTS_PER_MINUTE`] is the spec's documented limit, not a
//! measured one. One rotation asks for at most `pages` (hard cap 5) listing
//! requests plus one download per admitted candidate, and a default rotation is
//! half an hour apart, so pacing would be dead code; a 429 is the named
//! [`SourceErrorKind::RateLimited`] and is never retried.

use std::process::Command;

use whirl_core::config::{ConfigError, SourceConfig, json};
use whirl_core::source::{
    Candidate, Capability, EnumContext, Enumerated, FilterSet, Source, SourceError, SourceErrorKind,
};

use crate::http::{Curl, Fetch, Request};

/// The user agent every request carries.
///
/// features.md 2.3 does not name one; a bare programmatic client is what the
/// API's edge blocks, so this is an ordinary desktop-browser string. It carries
/// no version of this project's own, because a UA that says `whirl/0.1` is the
/// fingerprint a blocklist is built from.
pub const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                             (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// The documented rate limit: 45 requests per minute, 429 on exceed
/// (features.md 2.3, `[1]`). **Not measured** — the research card that
/// established it could not reach the response headers, and this session did not
/// probe for them either (the probe would itself spend the budget it measures).
/// It lives here, in one named place, so that a later measurement has one line
/// to correct.
pub const REQUESTS_PER_MINUTE: u32 = 45;

/// The listing endpoints of features.md 2.3's table.
const SEARCH_ENDPOINT: &str = "https://wallhaven.cc/api/v1/search";
const COLLECTIONS_ENDPOINT: &str = "https://wallhaven.cc/api/v1/collections";

/// The environment variable 2.4 reads first.
pub const API_KEY_ENV: &str = "WHIRL_WALLHAVEN_API_KEY";

/// The label 2.4's second source is asked for when `api_key_ref` names none:
/// "macOS: Keychain, item `whirl-wallhaven`".
const DEFAULT_KEY_LABEL: &str = "whirl-wallhaven";

/// 2.3's hard cap on `pages`: "default 1, hard cap 5".
const MAX_PAGES: u32 = 5;

/// Where an API key comes from, behind a trait because a test must never read
/// the machine's real environment or its keychain.
pub trait Secrets {
    /// The key for `reference`, or `None` when nothing resolvable holds it.
    ///
    /// The value goes into one request header and nowhere else: never a log
    /// line, never an error message, never `whirl status` (features.md 2.4's
    /// logging rule, docs/architecture.md 6.3).
    fn resolve(&self, reference: Option<&str>) -> Option<String>;

    /// Where the key was looked, as 2.4's refusal message words it. The label
    /// only: never the value.
    fn where_looked(&self, reference: Option<&str>) -> String;
}

/// The production resolution order of features.md 2.4, first hit wins: the
/// environment variable, then the OS secret store by label.
///
/// features.md 2.4 is taken as authoritative over docs/architecture.md 6.3,
/// which adds a third source ("a `0600` file the config points at") that 2.4's
/// own resolution list does not have, that `WallhavenSource` has no key to point
/// at (its twelve fields are the schema, and `api_key_ref` is documented as a
/// *label*), and that would put a key on disk where 2.4 puts it in a process's
/// environment. The divergence is reported rather than papered over.
#[derive(Debug, Clone, Copy, Default)]
pub struct Platform;

impl Secrets for Platform {
    fn resolve(&self, reference: Option<&str>) -> Option<String> {
        if let Some(value) = std::env::var(API_KEY_ENV)
            .ok()
            .filter(|value| !value.is_empty())
        {
            return Some(value);
        }
        store_lookup(&label_of(reference))
    }

    fn where_looked(&self, reference: Option<&str>) -> String {
        format!(
            "checked env {API_KEY_ENV}, {} '{}'",
            store_name(),
            label_of(reference)
        )
    }
}

/// The label inside an `api_key_ref`. 2.4's example is
/// `keychain:whirl-wallhaven`, and its bare form is the same label, so a
/// `keychain:`/`secret:` prefix is dropped and a plain label is used as it is.
fn label_of(reference: Option<&str>) -> String {
    match reference {
        Some(reference) => reference
            .rsplit_once(':')
            .map(|(_, label)| label)
            .unwrap_or(reference)
            .to_string(),
        None => DEFAULT_KEY_LABEL.to_string(),
    }
}

/// What 2.4 calls the platform's own store.
fn store_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "keychain label"
    } else if cfg!(target_os = "windows") {
        "Credential Manager credential"
    } else {
        "secret store label"
    }
}

/// The key out of the platform's own store, per 2.4's three rows.
///
/// macOS is the row that is implemented against a label
/// (`security find-generic-password -s <label> -w`). Linux's row is the fixed
/// `secret-tool lookup service whirl key wallhaven`, which has no place for a
/// label, and Windows' row needs the Credential Manager API rather than a
/// program; both answer `None` here, so a key-requiring source is refused by
/// name rather than run without one. No failure is printed: the caller's refusal
/// already names where it looked.
fn store_lookup(label: &str) -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let output = Command::new("security")
        .arg("find-generic-password")
        .arg("-s")
        .arg(label)
        .arg("-w")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

/// The dispatch arm of features.md 2.6, called from
/// [`crate::sources::build`]: one line per kind, and no logic in the table.
pub fn source(config: &SourceConfig) -> Box<dyn Source> {
    Box::new(Wallhaven::of(config, Box::new(Curl), Box::new(Platform)))
}

/// The `wallhaven` source of docs/spec/features.md 2.3. Every field is a key of
/// that schema, named as the config names it.
pub struct Wallhaven {
    /// The config's `id`, the prefix of every `origin_key` this source produces
    /// and the name every warning carries.
    id: String,
    query: Option<String>,
    categories: String,
    purity: String,
    sorting: String,
    order: String,
    ratios: Option<String>,
    atleast: Option<String>,
    colors: Option<String>,
    top_range: Option<String>,
    collection: Option<String>,
    pages: u32,
    /// A name, never a value (2.4).
    api_key_ref: Option<String>,
    fetch: Box<dyn Fetch>,
    secrets: Box<dyn Secrets>,
}

impl Wallhaven {
    /// The schema of 2.3, taken from the config this source was built for. A
    /// `wallhaven` kind with no `wallhaven` section is a hand-built config that
    /// was never parsed; the defaults here are 2.3's so that the refusal is the
    /// only thing that happens.
    pub fn of(
        config: &SourceConfig,
        fetch: Box<dyn Fetch>,
        secrets: Box<dyn Secrets>,
    ) -> Wallhaven {
        let wallhaven = config.wallhaven.clone().unwrap_or_default();
        Wallhaven {
            id: config.id.clone(),
            query: wallhaven.query,
            categories: wallhaven.categories,
            purity: wallhaven.purity,
            sorting: wallhaven.sorting,
            order: wallhaven.order,
            ratios: wallhaven.ratios,
            atleast: wallhaven.atleast,
            colors: wallhaven.colors,
            top_range: wallhaven.top_range,
            collection: wallhaven.collection,
            pages: wallhaven.pages,
            api_key_ref: wallhaven.api_key_ref,
            fetch,
            secrets,
        }
    }

    /// The offending key, as far as this method can name it, in the shape
    /// `sources/local.rs` established: the parser's own field paths are not
    /// reproducible from one source (the trait hands `validate` a single entry),
    /// and the source `id` is unique and is what the operator searches for.
    fn field(&self, key: &str) -> String {
        format!("sources[id={}].{key}", self.id)
    }

    /// Whether 2.4 says a key is required: "`purity` includes `110` (sketchy) or
    /// `111` (nsfw)". The flags are positional, so the SFW flag being set is not
    /// enough.
    fn needs_key(&self) -> bool {
        let flags: Vec<char> = self.purity.chars().collect();
        flags.get(1) == Some(&'1') || flags.get(2) == Some(&'1')
    }

    /// `collection` as the two path segments 2.3's endpoint takes, or `None`
    /// when the source is a search.
    fn collection_parts(&self) -> Result<Option<(String, String)>, ConfigError> {
        let Some(collection) = self.collection.as_deref() else {
            return Ok(None);
        };
        let mut segments = collection.split('/');
        let (Some(user), Some(id), None) = (segments.next(), segments.next(), segments.next())
        else {
            return Err(self.bad_collection(collection));
        };
        if user.is_empty() || id.is_empty() {
            return Err(self.bad_collection(collection));
        }
        Ok(Some((user.to_string(), id.to_string())))
    }

    fn bad_collection(&self, collection: &str) -> ConfigError {
        ConfigError::new(
            self.field("collection"),
            0,
            format!(
                "{collection:?} is not <username>/<id>, which is the path \
                 /api/v1/collections/<username>/<id> takes (features.md 2.3)"
            ),
        )
    }

    /// The listing URL for one page, per 2.3's parameter table.
    ///
    /// The search carries every parameter the schema has. The collection carries
    /// `purity` and `page` only: 2.3 says that endpoint "exposes only the
    /// `purity` filter, so every other filter is applied locally by the shared
    /// pipeline", and `atleast` in particular is ignored there, so sending it
    /// would suggest a floor that is not being applied.
    fn listing_url(&self, page: u32, parts: &Option<(String, String)>) -> String {
        match parts {
            Some((user, id)) => query_string(
                &format!("{COLLECTIONS_ENDPOINT}/{}/{}", encoded(user), encoded(id)),
                &[
                    ("purity".to_string(), self.purity.clone()),
                    ("page".to_string(), page.to_string()),
                ],
            ),
            None => {
                let mut parameters: Vec<(String, String)> = Vec::new();
                if let Some(query) = self.query.as_deref().filter(|query| !query.is_empty()) {
                    parameters.push(("q".to_string(), query.to_string()));
                }
                parameters.push(("categories".to_string(), self.categories.clone()));
                parameters.push(("purity".to_string(), self.purity.clone()));
                parameters.push(("sorting".to_string(), self.sorting.clone()));
                parameters.push(("order".to_string(), self.order.clone()));
                for (key, value) in [
                    ("ratios", &self.ratios),
                    ("atleast", &self.atleast),
                    ("colors", &self.colors),
                    ("topRange", &self.top_range),
                ] {
                    if let Some(value) = value.as_deref().filter(|value| !value.is_empty()) {
                        parameters.push((key.to_string(), value.to_string()));
                    }
                }
                parameters.push(("page".to_string(), page.to_string()));
                query_string(SEARCH_ENDPOINT, &parameters)
            }
        }
    }

    /// One page of the listing, or the named failure.
    fn page(
        &self,
        page: u32,
        parts: &Option<(String, String)>,
        key: Option<&str>,
    ) -> Result<Listing, SourceError> {
        let url = self.listing_url(page, parts);
        let response = self
            .fetch
            .get(&Request { url: &url, key })
            .map_err(|error| SourceError::new(SourceErrorKind::Unavailable, error))?;
        match response.status {
            200 => listing_of(&response.body).map_err(|error| {
                SourceError::new(
                    SourceErrorKind::Malformed,
                    format!(
                        "{} answered 200 with something that is not a listing: {error}",
                        self.endpoint(parts)
                    ),
                )
            }),
            status => Err(SourceError::new(
                status_kind(status),
                format!(
                    "{} answered {status}{}",
                    self.endpoint(parts),
                    api_error(&response.body)
                ),
            )),
        }
    }

    /// The endpoint as a human names it, for an error message. Never the whole
    /// URL: a URL is where a key ends up when it is not in a header (2.4), and
    /// this message goes to the daemon's log.
    fn endpoint(&self, parts: &Option<(String, String)>) -> String {
        match parts {
            Some((user, id)) => format!("{COLLECTIONS_ENDPOINT}/{user}/{id}"),
            None => SEARCH_ENDPOINT.to_string(),
        }
    }

    /// The refusal rules of 2.4 and 2.3. See the note on `refuse`'s counterpart
    /// in `sources/local.rs` for why a fact about the environment
    /// (`store_lookup`, one program) belongs here and not in `enumerate`.
    fn refuse(&self) -> Result<(), ConfigError> {
        if !(1..=MAX_PAGES).contains(&self.pages) {
            return Err(ConfigError::new(
                self.field("pages"),
                0,
                format!(
                    "pages {} is outside 1..={MAX_PAGES}; one page is 24 results against a \
                     documented {REQUESTS_PER_MINUTE} requests per minute (features.md 2.3)",
                    self.pages
                ),
            ));
        }
        self.collection_parts()?;
        if self.needs_key() && self.secrets.resolve(self.api_key_ref.as_deref()).is_none() {
            // 2.4's own wording, because it is the message an operator greps
            // for: "purity=111 requires an API key, none resolvable (checked
            // env WHIRL_WALLHAVEN_API_KEY, keychain label 'whirl-wallhaven')".
            return Err(ConfigError::new(
                self.field("purity"),
                0,
                format!(
                    "purity={} requires an API key, none resolvable ({})",
                    self.purity,
                    self.secrets.where_looked(self.api_key_ref.as_deref())
                ),
            ));
        }
        Ok(())
    }
}

impl Source for Wallhaven {
    fn validate(&self, _config: &SourceConfig) -> Result<(), ConfigError> {
        self.refuse()
    }

    /// Every candidate the configured pages hold, in the order the API returned
    /// them, or the named failure of the page that could not be read.
    ///
    /// A listing that half-arrived is not a listing: when page 2 of 3 fails the
    /// whole enumeration fails by name, so a rotation skips this source with the
    /// reason printed and tries the next one (features.md 1.4) instead of
    /// silently rotating from a shorter list than the config asked for.
    fn enumerate(&self, _ctx: &EnumContext) -> Result<Enumerated, SourceError> {
        let parts = self
            .collection_parts()
            .map_err(|error| SourceError::new(SourceErrorKind::Malformed, error.to_string()))?;
        if parts.is_some() && self.query.is_some() {
            eprintln!(
                "warning: source {}: `collection` is set, so `query` is not sent (features.md \
                 2.3: the collection endpoint is used \"instead of search\")",
                self.id
            );
        }
        let key = self.secrets.resolve(self.api_key_ref.as_deref());
        if self.needs_key() && key.is_none() {
            // 2.4's own rule is that the source is disabled at load (`refuse`
            // above), and that is what an operator reads. This is the belt for a
            // caller that skips `validate`: the failure is the named 401 shape
            // rather than a keyless request whose answer is an empty `data[]`
            // (measured) and a rotation that silently found nothing.
            return Err(SourceError::new(
                SourceErrorKind::Unauthorized,
                format!(
                    "purity={} requires an API key, none resolvable ({})",
                    self.purity,
                    self.secrets.where_looked(self.api_key_ref.as_deref())
                ),
            ));
        }
        // `validate` is the guard and runs before every enumeration the pipeline
        // makes; this is the belt for a caller that skips it.
        let configured = self.pages.clamp(1, MAX_PAGES);
        let mut candidates: Vec<Candidate> = Vec::new();
        let mut walked = 0u64;
        let mut page = 1u32;
        while page <= configured {
            let listing = self.page(page, &parts, key.as_deref())?;
            walked += 1;
            let empty = listing.entries.is_empty();
            // Every page that arrived contributes, including the one that ends
            // the walk; a walk never asks for a page it then discards.
            candidates.extend(listing.entries);
            // And the walk ends when the listing has no further page to ask for:
            // `last_page` reached, or a page that came back with nothing, which
            // is `pages` asking for more than the listing holds.
            if empty || listing.last_page <= page {
                break;
            }
            page += 1;
        }
        let skipped = u64::from(configured).saturating_sub(walked);
        if skipped > 0 {
            eprintln!(
                "warning: source {}: pages walked {walked} of {configured} configured; the \
                 listing ended at page {walked}",
                self.id
            );
        }
        Ok(Enumerated {
            candidates,
            pages_walked: walked,
            pages_skipped: skipped,
        })
    }

    /// What 2.5 says this source can push into its own request.
    ///
    /// A search answers all five of 2.3's filters. A collection answers
    /// `purity` alone, because that endpoint "exposes only the `purity` filter"
    /// — including not `resolution`: `atleast` is ignored there, so declaring it
    /// would be a promise the API does not keep, and the shared pipeline applies
    /// the floor instead (see the module docs).
    fn capabilities(&self) -> FilterSet {
        if self.collection.is_some() {
            FilterSet::of(&[Capability::Purity])
        } else {
            FilterSet::of(&[
                Capability::Resolution,
                Capability::Ratio,
                Capability::Purity,
                Capability::Colors,
                Capability::Category,
            ])
        }
    }
}

/// One parsed listing page.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Listing {
    entries: Vec<Candidate>,
    current_page: u32,
    last_page: u32,
}

/// Read one `data[]`/`meta` response.
///
/// The two shapes features.md 2.3 documents (`/search` and
/// `/api/v1/collections/<user>/<id>`) carry the same `data` array and the same
/// `meta` object, which is why one reader covers both and why a collection is
/// not a second implementation of this source.
fn listing_of(body: &str) -> Result<Listing, String> {
    let root = json::parse(body).map_err(|error| error.to_string())?;
    let mut entries: Vec<Candidate> = Vec::new();
    for node in array(&root, "data").ok_or("`data` is missing or is not an array")? {
        entries.push(candidate_of(node)?);
    }
    let meta = field(&root, "meta").ok_or("`meta` is missing")?;
    Ok(Listing {
        entries,
        current_page: integer(meta, "current_page").unwrap_or(0) as u32,
        last_page: integer(meta, "last_page").unwrap_or(0) as u32,
    })
}

/// One `data[]` entry as a pipeline candidate.
///
/// `id` and `path` are required: they are the secondary identity and the
/// re-materialisation hint of docs/spec/state-and-cache.md 4.1, and an entry
/// without them cannot be fetched or deduped. The three numbers are optional for
/// the reason 2.5 step 1 gives: a source that did not answer is not a source
/// that answered "too small", so a candidate with no dimensions goes to the
/// download and the header read decides.
fn candidate_of(node: &json::Node) -> Result<Candidate, String> {
    let id = text(node, "id").ok_or("an entry has no `id`")?;
    let origin = text(node, "path").ok_or("an entry has no `path`")?;
    Ok(Candidate {
        id: id.to_string(),
        origin: origin.to_string(),
        width: dimension(node, "dimension_x"),
        height: dimension(node, "dimension_y"),
        bytes: integer(node, "file_size"),
    })
}

/// The API's own word for what went wrong, when it sent one: the body of a
/// failed request is `{"error": "Nothing here"}`.
fn api_error(body: &str) -> String {
    match json::parse(body)
        .ok()
        .and_then(|root| text(&root, "error").map(str::to_string))
    {
        Some(message) => format!(": {message}"),
        None => String::new(),
    }
}

/// The kind a status code maps to. The four 2.3 and 2.4 name by number are
/// named here too; anything else is the source failing to answer.
fn status_kind(status: u16) -> SourceErrorKind {
    match status {
        401 => SourceErrorKind::Unauthorized,
        403 => SourceErrorKind::Forbidden,
        429 => SourceErrorKind::RateLimited,
        404 => SourceErrorKind::NotFound,
        _ => SourceErrorKind::Unavailable,
    }
}

/// The member of a JSON object, or `None`.
fn field<'a>(node: &'a json::Node, key: &str) -> Option<&'a json::Node> {
    let entries = node.as_object()?;
    entries
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
}

/// A string member of a JSON object, or `None`.
fn text<'a>(node: &'a json::Node, key: &str) -> Option<&'a str> {
    field(node, key)?.as_str()
}

/// A whole-number member: the API sends counts as JSON numbers, and one that
/// arrives as a string or a float is not a number this reads.
fn integer(node: &json::Node, key: &str) -> Option<u64> {
    let number = field(node, key)?.as_num()?;
    if number.is_finite() && number >= 0.0 {
        Some(number as u64)
    } else {
        None
    }
}

/// A pixel count, or `None` when the entry did not carry one. Zero is not a
/// dimension any display has, and treating it as one would reject the entry at
/// the resolution stage for a reason the API did not state.
fn dimension(node: &json::Node, key: &str) -> Option<u32> {
    match integer(node, key) {
        Some(value) if value > 0 && value <= u64::from(u32::MAX) => Some(value as u32),
        _ => None,
    }
}

/// The array member of a JSON object, or `None`.
fn array<'a>(node: &'a json::Node, key: &str) -> Option<&'a [json::Node]> {
    field(node, key)?.as_array()
}

/// A base URL and its parameters, percent-encoded.
///
/// Everything outside the unreserved set is encoded, which is what makes a
/// `query` holding the site's own tag syntax (`+tag`, `-tag`, `@user`, `id:`,
/// `like:`, features.md 2.3) arrive as the user wrote it instead of as a second
/// query string.
fn query_string(base: &str, parameters: &[(String, String)]) -> String {
    let mut out = String::from(base);
    for (index, (key, value)) in parameters.iter().enumerate() {
        out.push(if index == 0 { '?' } else { '&' });
        out.push_str(&encoded(key));
        out.push('=');
        out.push_str(&encoded(value));
    }
    out
}

fn encoded(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use whirl_core::config::Config;
    use whirl_core::source::Source;

    use super::*;
    use crate::http::Response;
    use crate::pipeline::{Platform as HostPlatform, Seeking, Window, filter_pipeline};

    // -- the two seams, both answering with something a test wrote ----------

    /// A `Fetch` that answers with recorded statuses and bodies, and remembers
    /// what it was asked. It never opens a socket.
    struct Recorded {
        answers: Vec<(u16, String)>,
        seen: Rc<RefCell<Vec<(String, Option<String>)>>>,
    }

    impl Recorded {
        fn answering(answers: Vec<(u16, String)>) -> Recorded {
            Recorded {
                answers,
                seen: Rc::new(RefCell::new(Vec::new())),
            }
        }

        fn handle(&self) -> Rc<RefCell<Vec<(String, Option<String>)>>> {
            Rc::clone(&self.seen)
        }
    }

    impl Fetch for Recorded {
        fn get(&self, request: &Request<'_>) -> Result<Response, String> {
            let index = {
                let mut seen = self.seen.borrow_mut();
                seen.push((request.url.to_string(), request.key.map(str::to_string)));
                seen.len() - 1
            };
            match self.answers.get(index) {
                Some((status, body)) => Ok(Response {
                    status: *status,
                    body: body.clone(),
                }),
                // A page the test did not record is an empty listing, which is
                // how a walk that runs past the fixture ends.
                None => Ok(Response {
                    status: 200,
                    body: r#"{"data":[],"meta":{"current_page":9,"last_page":9}}"#.to_string(),
                }),
            }
        }
    }

    /// A `Fetch` that never made a request, for the program-not-installed case.
    struct Offline;

    impl Fetch for Offline {
        fn get(&self, _request: &Request<'_>) -> Result<Response, String> {
            Err("`curl` is not on PATH, and this build makes HTTP requests with it".to_string())
        }
    }

    /// A `Secrets` that answers with a value a test wrote, and never reads the
    /// environment or a keychain.
    struct Key(Option<&'static str>);

    impl Secrets for Key {
        fn resolve(&self, _reference: Option<&str>) -> Option<String> {
            self.0.map(str::to_string)
        }

        fn where_looked(&self, _reference: Option<&str>) -> String {
            "checked env WHIRL_WALLHAVEN_API_KEY, keychain label 'whirl-wallhaven'".to_string()
        }
    }

    // -- fixtures: the API's shape, nobody's contents ----------------------

    /// One `data[]` entry of the form `/search` and a collection both return.
    fn entry(id: &str, width: u32, height: u32, bytes: u64) -> String {
        format!(
            "{{\"id\":\"{id}\",\"url\":\"https://wallhaven.cc/w/{id}\",\"purity\":\"sfw\",\
             \"category\":\"general\",\"dimension_x\":{width},\"dimension_y\":{height},\
             \"ratio\":\"1.78\",\"file_size\":{bytes},\"file_type\":\"image/png\",\
             \"colors\":[\"#000000\"],\"path\":\"https://w.wallhaven.cc/full/aa/wallhaven-{id}.png\",\
             \"thumbs\":{{\"large\":\"https://th.wallhaven.cc/lg/aa/{id}.jpg\"}}}}"
        )
    }

    fn page(entries: &[String], current: u32, last: u32) -> String {
        format!(
            "{{\"data\":[{}],\"meta\":{{\"current_page\":{current},\"last_page\":{last},\
             \"per_page\":24,\"total\":55}}}}",
            entries.join(",")
        )
    }

    fn config_of(text: &str) -> Config {
        Config::parse(text)
            .expect("the fixture config parses")
            .config
    }

    /// What the recorded fake was asked, in order: a URL and the key header.
    type Asked = Rc<RefCell<Vec<(String, Option<String>)>>>;

    /// A source built the way the dispatch table builds one, except for the two
    /// seams. The handle it returns is where the fake wrote down what it was
    /// asked, so a test can assert on the request as well as on the answer.
    fn wallhaven(text: &str, recorded: Recorded, secrets: Box<dyn Secrets>) -> (Wallhaven, Asked) {
        let asked = recorded.handle();
        let config = config_of(text);
        (
            Wallhaven::of(&config.sources[0], Box::new(recorded), secrets),
            asked,
        )
    }

    /// The same, for a `Fetch` that is not the recorded one.
    fn wallhaven_with(text: &str, fetch: Box<dyn Fetch>, secrets: Box<dyn Secrets>) -> Wallhaven {
        let config = config_of(text);
        Wallhaven::of(&config.sources[0], fetch, secrets)
    }

    fn context() -> EnumContext {
        EnumContext::default()
    }

    /// The ids of the candidates an enumeration produced.
    fn ids(enumerated: &Enumerated) -> Vec<String> {
        enumerated
            .candidates
            .iter()
            .map(|candidate| candidate.id.clone())
            .collect()
    }

    const SEARCH: &str = "{\n  \"config_schema\": 1,\n  \"sources\": [ { \"id\": \"space\", \
                          \"kind\": \"wallhaven\", \"weight\": 3, \"query\": \"space nebula\", \
                          \"ratios\": \"16x9\", \"atleast\": \"2560x1440\" } ]\n}\n";

    const COLLECTION: &str = "{\n  \"config_schema\": 1,\n  \"sources\": [ { \"id\": \"space\", \
                              \"kind\": \"wallhaven\", \"weight\": 3, \"collection\": \
                              \"example-user/12345\", \"atleast\": \"2560x1440\" } ]\n}\n";

    // -- the listing, both modes -------------------------------------------

    #[test]
    fn a_search_listing_becomes_candidates_carrying_the_apis_own_numbers() {
        let recorded = Recorded::answering(vec![(
            200,
            page(&[entry("aaaaaa", 3840, 2160, 10_783_225)], 1, 1),
        )]);
        let (source, _) = wallhaven(SEARCH, recorded, Box::new(Key(None)));
        let enumerated = source.enumerate(&context()).expect("the listing is read");
        assert_eq!(ids(&enumerated), vec!["aaaaaa".to_string()]);
        let candidate = &enumerated.candidates[0];
        assert_eq!(
            candidate.origin,
            "https://w.wallhaven.cc/full/aa/wallhaven-aaaaaa.png"
        );
        assert_eq!(candidate.width, Some(3840));
        assert_eq!(candidate.height, Some(2160));
        assert_eq!(candidate.bytes, Some(10_783_225));
        assert_eq!(enumerated.pages_walked, 1);
        assert_eq!(enumerated.pages_skipped, 0);
    }

    #[test]
    fn a_collection_listing_is_read_by_the_same_reader() {
        let recorded = Recorded::answering(vec![(
            200,
            page(
                &[
                    entry("bbbbbb", 5120, 2880, 20),
                    entry("cccccc", 1920, 1080, 30),
                ],
                1,
                1,
            ),
        )]);
        let (source, seen) = wallhaven(COLLECTION, recorded, Box::new(Key(None)));
        let enumerated = source.enumerate(&context()).expect("the listing is read");
        assert_eq!(
            ids(&enumerated),
            vec!["bbbbbb".to_string(), "cccccc".to_string()]
        );
        let asked = seen.borrow();
        assert_eq!(asked.len(), 1);
        assert!(
            asked[0]
                .0
                .starts_with("https://wallhaven.cc/api/v1/collections/example-user/12345?"),
            "{}",
            asked[0].0
        );
    }

    #[test]
    fn the_collection_request_does_not_send_the_atleast_that_endpoint_ignores() {
        let recorded = Recorded::answering(vec![(200, page(&[], 1, 1))]);
        let (source, seen) = wallhaven(COLLECTION, recorded, Box::new(Key(None)));
        source
            .enumerate(&context())
            .expect("an empty listing is a listing");
        let url = seen.borrow()[0].0.clone();
        assert!(!url.contains("atleast"), "{url}");
        assert!(!url.contains("ratios"), "{url}");
        assert!(url.contains("purity=100"), "{url}");
    }

    #[test]
    fn the_search_request_sends_every_parameter_the_schema_has() {
        let text = "{\n  \"config_schema\": 1,\n  \"sources\": [ { \"id\": \"space\", \"kind\": \
                    \"wallhaven\", \"query\": \"space nebula+sky\", \"categories\": \"101\", \
                    \"purity\": \"100\", \"sorting\": \"toplist\", \"order\": \"asc\", \
                    \"ratios\": \"16x9,16x10\", \"atleast\": \"2560x1440\", \"colors\": \
                    \"#00ff00\", \"top_range\": \"1M\" } ]\n}\n";
        let recorded = Recorded::answering(vec![(200, page(&[], 1, 1))]);
        let (source, seen) = wallhaven(text, recorded, Box::new(Key(None)));
        source.enumerate(&context()).expect("a listing");
        let url = seen.borrow()[0].0.clone();
        for expected in [
            "q=space%20nebula%2Bsky",
            "categories=101",
            "purity=100",
            "sorting=toplist",
            "order=asc",
            "ratios=16x9%2C16x10",
            "atleast=2560x1440",
            "colors=%2300ff00",
            "topRange=1M",
            "page=1",
        ] {
            assert!(url.contains(expected), "{expected} is missing from {url}");
        }
    }

    // -- the floor is the pipeline's ---------------------------------------

    #[test]
    fn an_entry_below_the_floor_is_still_a_candidate_with_its_real_dimensions() {
        // The local source's own rule (features.md 2.5 step 1): the source
        // reports what the API said and the pipeline decides.
        let recorded =
            Recorded::answering(vec![(200, page(&[entry("small1", 1024, 600, 10)], 1, 1))]);
        let (source, _) = wallhaven(COLLECTION, recorded, Box::new(Key(None)));
        let enumerated = source.enumerate(&context()).expect("the listing is read");
        assert_eq!(enumerated.candidates.len(), 1);
        assert_eq!(enumerated.candidates[0].width, Some(1024));
    }

    #[test]
    fn the_pipelines_rejected_resolution_counter_counts_the_floor_the_api_ignored() {
        // `atleast=2560x1440` is in the config and this is a collection, so the
        // API ignored it. The floor below is the pipeline's, from
        // min_width/min_height, and the counter has to carry it.
        let text = "{\n  \"config_schema\": 1,\n  \"min_width\": 2560,\n  \"min_height\": 1440,\n  \
                    \"sources\": [ { \"id\": \"space\", \"kind\": \"wallhaven\", \"collection\": \
                    \"example-user/12345\", \"atleast\": \"2560x1440\" } ]\n}\n";
        let recorded = Recorded::answering(vec![(
            200,
            page(
                &[
                    entry("bigone", 3840, 2160, 10),
                    entry("small1", 1920, 1080, 11),
                    entry("small2", 1024, 600, 12),
                ],
                1,
                1,
            ),
        )]);
        let config = config_of(text);
        let source = Wallhaven::of(&config.sources[0], Box::new(recorded), Box::new(Key(None)));
        let enumerated = source.enumerate(&context()).expect("the listing is read");
        assert_eq!(
            enumerated.candidates.len(),
            3,
            "the source yields every entry and filters none"
        );
        let seeking = enumerated
            .candidates
            .into_iter()
            .map(|candidate| Seeking {
                source: "space".to_string(),
                candidate,
            })
            .collect();
        let filtered = filter_pipeline(&config, HostPlatform::Macos, &Window::empty(), seeking);
        assert_eq!(filtered.counters.rejected_resolution, 2);
        assert_eq!(filtered.counters.admitted, 1);
    }

    // -- pagination ---------------------------------------------------------

    #[test]
    fn a_walk_stops_at_last_page_and_counts_what_it_did_not_walk() {
        let text = "{\n  \"config_schema\": 1,\n  \"sources\": [ { \"id\": \"space\", \"kind\": \
                    \"wallhaven\", \"collection\": \"example-user/12345\", \"pages\": 3 } ]\n}\n";
        let recorded = Recorded::answering(vec![
            (200, page(&[entry("aaaaaa", 3840, 2160, 10)], 1, 2)),
            (200, page(&[entry("bbbbbb", 3840, 2160, 11)], 2, 2)),
        ]);
        let (source, seen) = wallhaven(text, recorded, Box::new(Key(None)));
        let enumerated = source.enumerate(&context()).expect("the listing is read");
        assert_eq!(
            ids(&enumerated),
            vec!["aaaaaa".to_string(), "bbbbbb".to_string()]
        );
        assert_eq!(enumerated.pages_walked, 2, "page 2 was the last one");
        assert_eq!(enumerated.pages_skipped, 1, "page 3 does not exist");
        assert_eq!(seen.borrow().len(), 2, "and it was never requested");
    }

    #[test]
    fn a_walk_makes_every_configured_page_when_the_listing_is_longer() {
        let text = "{\n  \"config_schema\": 1,\n  \"sources\": [ { \"id\": \"space\", \"kind\": \
                    \"wallhaven\", \"collection\": \"example-user/12345\", \"pages\": 2 } ]\n}\n";
        let recorded = Recorded::answering(vec![
            (200, page(&[entry("aaaaaa", 3840, 2160, 10)], 1, 20873)),
            (200, page(&[entry("bbbbbb", 3840, 2160, 11)], 2, 20873)),
        ]);
        let (source, seen) = wallhaven(text, recorded, Box::new(Key(None)));
        let enumerated = source.enumerate(&context()).expect("the listing is read");
        assert_eq!(enumerated.pages_walked, 2);
        assert_eq!(
            enumerated.pages_skipped, 0,
            "a listing with more pages than `pages` asked for is not a skip"
        );
        assert_eq!(seen.borrow().len(), 2);
    }

    #[test]
    fn an_empty_page_ends_the_walk_and_the_rest_is_counted_as_skipped() {
        let text = "{\n  \"config_schema\": 1,\n  \"sources\": [ { \"id\": \"space\", \"kind\": \
                    \"wallhaven\", \"collection\": \"example-user/12345\", \"pages\": 3 } ]\n}\n";
        let recorded = Recorded::answering(vec![(200, page(&[], 1, 9))]);
        let (source, seen) = wallhaven(text, recorded, Box::new(Key(None)));
        let enumerated = source
            .enumerate(&context())
            .expect("an empty listing is a listing");
        assert!(enumerated.candidates.is_empty());
        assert_eq!(enumerated.pages_walked, 1);
        assert_eq!(enumerated.pages_skipped, 2);
        assert_eq!(seen.borrow().len(), 1);
    }

    // -- the named error paths ---------------------------------------------

    #[test]
    fn an_api_refusal_is_a_named_error_and_not_an_empty_list() {
        for (status, expected) in [
            (401, SourceErrorKind::Unauthorized),
            (403, SourceErrorKind::Forbidden),
            (429, SourceErrorKind::RateLimited),
            (404, SourceErrorKind::NotFound),
            (500, SourceErrorKind::Unavailable),
        ] {
            let recorded =
                Recorded::answering(vec![(status, "{\"error\":\"Nothing here\"}".to_string())]);
            let (source, _) = wallhaven(SEARCH, recorded, Box::new(Key(None)));
            let error = source
                .enumerate(&context())
                .expect_err(&format!("{status} is not a listing"));
            assert_eq!(error.kind, expected, "{status}");
            assert!(error.to_string().contains(&status.to_string()), "{error}");
            assert!(
                error.to_string().contains("Nothing here"),
                "the API's own word travels: {error}"
            );
        }
    }

    #[test]
    fn a_program_that_is_not_there_is_unavailable_and_says_which_one() {
        let source = wallhaven_with(SEARCH, Box::new(Offline), Box::new(Key(None)));
        let error = source.enumerate(&context()).expect_err("no client");
        assert_eq!(error.kind, SourceErrorKind::Unavailable);
        assert!(error.to_string().contains("curl"), "{error}");
    }

    #[test]
    fn a_body_that_is_not_a_listing_is_malformed() {
        let recorded = Recorded::answering(vec![(200, "<html>not json</html>".to_string())]);
        let (source, _) = wallhaven(SEARCH, recorded, Box::new(Key(None)));
        let error = source.enumerate(&context()).expect_err("no listing");
        assert_eq!(error.kind, SourceErrorKind::Malformed);
    }

    #[test]
    fn a_failed_second_page_fails_the_whole_listing_by_name() {
        let text = "{\n  \"config_schema\": 1,\n  \"sources\": [ { \"id\": \"space\", \"kind\": \
                    \"wallhaven\", \"collection\": \"example-user/12345\", \"pages\": 2 } ]\n}\n";
        let recorded = Recorded::answering(vec![
            (200, page(&[entry("aaaaaa", 3840, 2160, 10)], 1, 9)),
            (429, "{\"error\":\"too many requests\"}".to_string()),
        ]);
        let (source, _) = wallhaven(text, recorded, Box::new(Key(None)));
        let error = source.enumerate(&context()).expect_err("page 2 failed");
        assert_eq!(error.kind, SourceErrorKind::RateLimited);
    }

    // -- the key ------------------------------------------------------------

    #[test]
    fn a_purity_that_needs_a_key_is_refused_at_validation_with_the_specs_wording() {
        for purity in ["110", "111"] {
            let text = format!(
                "{{\n  \"config_schema\": 1,\n  \"sources\": [ {{ \"id\": \"space\", \"kind\": \
                 \"wallhaven\", \"purity\": \"{purity}\" }} ]\n}}\n"
            );
            let recorded = Recorded::answering(vec![]);
            let (source, seen) = wallhaven(&text, recorded, Box::new(Key(None)));
            let error = source
                .validate(&config_of(&text).sources[0])
                .expect_err("no key, no sketchy");
            let message = error.to_string();
            assert!(
                message.contains(&format!("purity={purity} requires an API key")),
                "{message}"
            );
            assert!(message.contains("none resolvable"), "{message}");
            assert!(message.contains(API_KEY_ENV), "{message}");
            assert!(message.contains(DEFAULT_KEY_LABEL), "{message}");
            assert!(
                seen.borrow().is_empty(),
                "a refusal is not a retry storm: nothing was requested"
            );
        }
    }

    #[test]
    fn a_key_requiring_purity_enumerated_without_a_key_is_the_named_unauthorized_error() {
        // Two lines of defence for one rule. 2.4 disables the source at load,
        // which the test above pins; this is the second, for a caller that skips
        // `validate`: the named 401 rather than a keyless request the API answers
        // with an empty page (measured) and a rotation that found nothing.
        let text = "{\n  \"config_schema\": 1,\n  \"sources\": [ { \"id\": \"space\", \"kind\": \
                    \"wallhaven\", \"purity\": \"111\" } ]\n}\n";
        let recorded = Recorded::answering(vec![]);
        let (source, seen) = wallhaven(text, recorded, Box::new(Key(None)));
        let error = source
            .enumerate(&context())
            .expect_err("no key, no sketchy");
        assert_eq!(error.kind, SourceErrorKind::Unauthorized);
        assert!(error.to_string().contains(API_KEY_ENV), "{error}");
        assert!(
            seen.borrow().is_empty(),
            "a refusal is not a retry storm: nothing was requested"
        );
    }

    #[test]
    fn an_sfw_purity_needs_no_key_at_all() {
        let text = "{\n  \"config_schema\": 1,\n  \"sources\": [ { \"id\": \"space\", \"kind\": \
                    \"wallhaven\", \"purity\": \"100\" } ]\n}\n";
        let recorded = Recorded::answering(vec![]);
        let (source, _) = wallhaven(text, recorded, Box::new(Key(None)));
        assert!(source.validate(&config_of(text).sources[0]).is_ok());
    }

    #[test]
    fn a_resolvable_key_is_sent_as_a_header_and_never_printed() {
        let text = "{\n  \"config_schema\": 1,\n  \"sources\": [ { \"id\": \"space\", \"kind\": \
                    \"wallhaven\", \"purity\": \"111\", \"api_key_ref\": \
                    \"keychain:whirl-wallhaven\" } ]\n}\n";
        let recorded = Recorded::answering(vec![(200, page(&[], 1, 1))]);
        let (source, seen) = wallhaven(text, recorded, Box::new(Key(Some("test-key"))));
        assert!(source.validate(&config_of(text).sources[0]).is_ok());
        source.enumerate(&context()).expect("a listing");
        let asked = seen.borrow();
        assert_eq!(asked[0].1.as_deref(), Some("test-key"));
        assert!(
            !asked[0].0.contains("test-key"),
            "never in the URL: {}",
            asked[0].0
        );
    }

    #[test]
    fn the_label_is_the_one_api_key_ref_names() {
        assert_eq!(label_of(None), DEFAULT_KEY_LABEL);
        assert_eq!(
            label_of(Some("keychain:whirl-wallhaven")),
            "whirl-wallhaven"
        );
        assert_eq!(label_of(Some("other-label")), "other-label");
    }

    // -- the config refusals ------------------------------------------------

    #[test]
    fn a_collection_that_is_not_user_and_id_is_refused_naming_the_key() {
        let text = "{\n  \"config_schema\": 1,\n  \"sources\": [ { \"id\": \"space\", \"kind\": \
                    \"wallhaven\", \"collection\": \"just-a-name\" } ]\n}\n";
        let recorded = Recorded::answering(vec![]);
        let (source, _) = wallhaven(text, recorded, Box::new(Key(None)));
        let error = source
            .validate(&config_of(text).sources[0])
            .expect_err("a malformed handle");
        assert!(error.to_string().contains("collection"), "{error}");
    }

    #[test]
    fn pages_outside_the_specs_range_is_refused() {
        for pages in [0, 6] {
            let text = format!(
                "{{\n  \"config_schema\": 1,\n  \"sources\": [ {{ \"id\": \"space\", \"kind\": \
                 \"wallhaven\", \"pages\": {pages} }} ]\n}}\n"
            );
            // The parser refuses these before a source ever sees them (2.3's
            // hard cap), so the fixture is built by hand: this is the rule for a
            // config assembled in code.
            let config = config_of(
                "{\n  \"config_schema\": 1,\n  \"sources\": [ { \"id\": \"space\", \"kind\": \
                 \"wallhaven\" } ]\n}\n",
            );
            let mut wallhaven = config.sources[0].clone();
            wallhaven.wallhaven.as_mut().expect("the section").pages = pages;
            let source = Wallhaven::of(
                &wallhaven,
                Box::new(Recorded::answering(vec![])),
                Box::new(Key(None)),
            );
            let error = source.validate(&wallhaven).expect_err("outside 1..=5");
            assert!(error.to_string().contains("pages"), "{error}");
            assert!(!text.is_empty());
        }
    }

    #[test]
    fn the_capabilities_are_five_for_a_search_and_purity_only_for_a_collection() {
        let (search, _) = wallhaven(SEARCH, Recorded::answering(vec![]), Box::new(Key(None)));
        assert!(search.capabilities().contains(Capability::Resolution));
        assert!(search.capabilities().contains(Capability::Ratio));
        let (collection, _) =
            wallhaven(COLLECTION, Recorded::answering(vec![]), Box::new(Key(None)));
        assert_eq!(
            collection.capabilities(),
            FilterSet::of(&[Capability::Purity])
        );
        assert!(
            !collection.capabilities().contains(Capability::Resolution),
            "atleast is ignored on that endpoint, so the source does not claim it (features.md 2.3)"
        );
    }

    #[test]
    fn a_collection_is_the_mode_when_the_config_sets_both_keys() {
        let text = "{\n  \"config_schema\": 1,\n  \"sources\": [ { \"id\": \"space\", \"kind\": \
                    \"wallhaven\", \"query\": \"space nebula\", \"collection\": \
                    \"example-user/12345\" } ]\n}\n";
        let recorded = Recorded::answering(vec![(200, page(&[], 1, 1))]);
        let (source, seen) = wallhaven(text, recorded, Box::new(Key(None)));
        source.enumerate(&context()).expect("a listing");
        let url = seen.borrow()[0].0.clone();
        assert!(url.contains("/collections/"), "{url}");
        assert!(!url.contains("q="), "{url}");
    }

    #[test]
    fn the_user_agent_is_an_ordinary_browser_one() {
        assert!(USER_AGENT.starts_with("Mozilla/5.0"), "{USER_AGENT}");
        assert!(USER_AGENT.contains("AppleWebKit"), "{USER_AGENT}");
        assert!(
            !USER_AGENT.contains("whirl"),
            "a UA that names this project is the fingerprint a blocklist is built from"
        );
    }
}
