//! Structured output types returned by SDK operations.
//!
//! Each `*Op::run` returns a typed report.  Reports never carry
//! errors — they carry *findings*: the things the operation observed
//! that the consumer might want to surface (semantic warnings, parse
//! errors on inline input, the final TPTP string, …).  Infrastructural
//! failures (KB-level bail-outs, config conflicts, prover spawn
//! errors) flow out as `Err(SdkError)` instead.
//!
//! The split keeps the consumer's match arms small: success means
//! "the operation completed; here's what it produced", and `Err` means
//! "the operation could not run".

#[cfg(feature = "ask")]
pub mod ask;
pub mod ingest;
#[cfg(feature = "persist")]
pub mod load;
#[cfg(feature = "ask")]
pub mod test;
pub mod translate;
pub mod validate;

#[cfg(feature = "ask")]
pub use ask::AskReport;
pub use ingest::{IngestReport, SourceIngestStatus};
#[cfg(feature = "persist")]
pub use load::{LoadFileStatus, LoadReport};
#[cfg(feature = "ask")]
pub use test::{TestCaseReport, TestOutcome, TestSuiteReport};
pub use translate::{TranslateReport, TranslatedSentence};
pub use validate::ValidationReport;
