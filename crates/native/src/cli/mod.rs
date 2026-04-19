pub mod args;
pub mod util;
pub mod load;
pub mod validate;
pub mod translate;
pub mod man;

// Ask + test depend on sumo-kb's prover API, which is only compiled
// under the `vampire` feature.  Without it, sumo still builds but
// provides only translate / validate / load / man.
#[cfg(feature = "ask")]
pub mod ask;
#[cfg(feature = "ask")]
pub mod test;

pub use args::{Cli, KbArgs, Cmd};
pub use load::run_load;
pub use validate::run_validate;
pub use translate::run_translate;
pub use man::run_man;

#[cfg(feature = "ask")]
pub use ask::run_ask;
#[cfg(feature = "ask")]
pub use test::run_test;
