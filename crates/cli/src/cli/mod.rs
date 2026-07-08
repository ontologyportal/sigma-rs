pub mod args;
pub mod update;
pub mod util;
pub mod load;
pub mod validate;
pub mod translate;
pub mod man;
pub mod search;
pub mod config_cmd;
pub mod args_project;

// Ask + test + debug depend on sigmakee-rs-core's prover API, which is only
// compiled under the `vampire` feature.  Without it, sumo still builds
// but provides only translate / validate / load / man.
#[cfg(feature = "ask")]
pub mod ask;
#[cfg(feature = "ask")]
pub mod test;
#[cfg(feature = "ask")]
pub mod audit;
#[cfg(feature = "ask")]
pub mod proof;
#[cfg(feature = "ask")]
pub mod casc;
#[cfg(feature = "ask")]
#[cfg(feature = "sweep")]
pub mod sweep;

#[cfg(feature = "server")]
pub mod serve;

pub use args::{Cli, KbArgs, Cmd};
pub use load::{run_load, run_load_warm, run_flush};
pub use update::run_update;
pub use validate::run_validate;
pub use translate::run_translate;
pub use man::run_man;
pub use search::run_search;
pub use config_cmd::run_config;

#[cfg(feature = "ask")]
pub use ask::run_ask;
#[cfg(feature = "ask")]
#[cfg(feature = "sweep")]
pub use sweep::run_sweep;
#[cfg(feature = "ask")]
pub use test::run_test;
#[cfg(feature = "ask")]
pub use audit::run_audit;
#[cfg(feature = "ask")]
pub use casc::run_casc;

#[cfg(feature = "server")]
pub use serve::run_serve;
