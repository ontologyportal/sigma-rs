pub mod args;
pub mod args_project;
pub mod check;
pub mod config_cmd;
pub mod config_tui;
pub mod load;
pub mod man;
pub mod search;
pub mod translate;
pub mod update;
pub mod util;
pub mod validate;

// Ask + test + debug depend on sigmakee-rs-core's prover API, which is only
// compiled under the `vampire` feature.  Without it, sumo still builds
// but provides only translate / validate / load / man.
#[cfg(feature = "ask")]
pub mod ask;
#[cfg(feature = "ask")]
pub mod ask_tui;
#[cfg(feature = "ask")]
pub mod audit;
#[cfg(feature = "ask")]
pub mod casc;
#[cfg(feature = "ask")]
pub mod proof;
#[cfg(feature = "ask")]
#[cfg(feature = "sweep")]
pub mod sweep;
#[cfg(feature = "ask")]
pub mod test;

// #[cfg(feature = "server")]
// pub mod serve;

pub use args::{Cli, Cmd, KbArgs};
pub use check::{maybe_notify_stale_git, maybe_notify_stale_local, run_check};
pub use config_cmd::{run_config, run_config_write, ConstituentEdit};
pub use config_tui::run_config_tui;
pub use load::{run_flush, run_load, run_load_warm};
pub use man::run_man;
pub use search::run_search;
pub use translate::run_translate;
pub use update::{maybe_notify_update, run_update};
pub use validate::run_validate;

#[cfg(feature = "ask")]
pub use ask::run_ask;
#[cfg(feature = "ask")]
pub use ask_tui::run_ask_tui;
#[cfg(feature = "ask")]
pub use audit::run_audit;
#[cfg(feature = "ask")]
pub use casc::run_casc;
#[cfg(feature = "ask")]
#[cfg(feature = "sweep")]
pub use sweep::run_sweep;
#[cfg(feature = "ask")]
pub use test::run_test;

// #[cfg(feature = "server")]
// pub use serve::run_serve;
