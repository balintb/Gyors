//! Gyors core: query, candidate, provider trait, ranker, frecency
//!
//! This crate has no macOS dependencies - it's a pure pipeline for turning
//! user input into ranked candidates. The platform layer lives in `gyors-ipc`

pub mod candidate;
pub mod frecency;
pub mod provider;
pub mod query;
pub mod ranker;

pub use candidate::{Action, Candidate, CandidateId, CandidateKind, Effect, Icon};
pub use frecency::Frecency;
pub use provider::{KeywordSpec, Provider};
pub use query::{parse_mode, Query, QueryMode};
pub use ranker::{precision_bonus, NucleoRanker, Ranker, ScoredCandidate, MAX_FRECENCY_BOOST};
