//! whirl-core: everything the daemon, the worker and the CLI share.
//!
//! Four modules, and each one has exactly one implementation in the workspace
//! (docs/development.md section 1):
//!
//! - [`protocol`]: the line grammar of docs/architecture.md section 2, the verb
//!   set, the response records and the closed error-code set.
//! - [`config`]: the config schema of docs/spec/features.md Part 2 with the
//!   precedence and validation rules of docs/architecture.md 4.3, plus the
//!   platform default paths of docs/spec/state-and-cache.md section 1.
//! - [`state`]: the state records and the history ring.
//! - [`source`]: the source trait and the candidate type (docs/spec/features.md
//!   2.6). No source implementation lives here or anywhere else yet.
//!
//! Nothing in this crate does I/O and nothing in it links a platform backend: the
//! callers read the files and own the sockets. The two parsers (protocol and
//! config) and the hasher are written against the standard library alone, because
//! v0.1 has zero third-party dependencies (docs/development.md section 2).

pub mod config;
pub mod protocol;
pub mod source;
pub mod state;

/// Throwaway: the deliberate clippy failure of the gate proof in t_438e03b0.
/// Reverted in the commit after this one.
pub fn gate_proof_declares_itself(flag: bool) -> bool {
    let answer = if flag == true { true } else { false };
    answer
}
