pub mod args;
pub mod profile;
pub mod update;
pub mod util;
pub mod load;
pub mod validate;
pub mod translate;
pub mod man;

// Ask + test + debug depend on sigmakee-rs-core's prover API, which is only
// compiled under the `vampire` feature.  Without it, sumo still builds
// but provides only translate / validate / load / man.
#[cfg(feature = "ask")]
pub mod ask;
#[cfg(feature = "ask")]
pub mod test;
#[cfg(feature = "ask")]
pub mod debug;
#[cfg(feature = "ask")]
pub mod proof;

#[cfg(feature = "server")]
pub mod serve;

pub use args::{Cli, KbArgs, Cmd};
pub use load::run_load;
pub use update::run_update;
pub use validate::run_validate;
pub use translate::run_translate;
pub use man::run_man;

#[cfg(feature = "ask")]
pub use ask::run_ask;
#[cfg(feature = "ask")]
pub use test::run_test;
#[cfg(feature = "ask")]
pub use debug::run_debug;

#[cfg(feature = "server")]
pub use serve::run_serve;
