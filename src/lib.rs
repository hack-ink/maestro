//! Maestro runtime bootstrap and CLI entrypoint.

#![deny(clippy::all, missing_docs, unused_crate_dependencies)]

mod agent;
mod cli;
/// Service configuration types and loaders.
pub mod config;
mod orchestrator;
mod prelude {
	pub use color_eyre::{Result, eyre};
}
/// Thin local persistence for active Maestro execution state.
pub mod state;
mod tracker;
/// Downstream `WORKFLOW.md` parsing and validation.
pub mod workflow;
mod workspace;

use std::{panic, process};

use clap::Parser;
use directories::ProjectDirs;
use tracing_appender::{
	non_blocking::WorkerGuard,
	rolling::{RollingFileAppender, Rotation},
};
use tracing_subscriber::EnvFilter;

use crate::{
	cli::Cli,
	prelude::{Result, eyre},
};

/// Run the Maestro CLI after initializing error reporting, logging, and the panic hook.
pub fn run() -> Result<()> {
	color_eyre::install()?;

	let _guard = init_tracing()?;

	install_panic_hook();

	Cli::parse().run()
}

fn init_tracing() -> Result<WorkerGuard> {
	let project_dirs = ProjectDirs::from("", "helixbox", env!("CARGO_PKG_NAME"))
		.ok_or_else(|| eyre::eyre!("Failed to resolve project directories."))?;
	let app_root = project_dirs.data_dir();
	let (non_blocking, guard) = tracing_appender::non_blocking(
		RollingFileAppender::builder()
			.rotation(Rotation::WEEKLY)
			.max_log_files(3)
			.filename_suffix("log")
			.build(app_root)?,
	);
	let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

	tracing_subscriber::fmt()
		.with_env_filter(filter)
		.with_ansi(false)
		.with_writer(non_blocking)
		.init();

	Ok(guard)
}

fn install_panic_hook() {
	let default_hook = panic::take_hook();

	panic::set_hook(Box::new(move |panic_info| {
		default_hook(panic_info);

		process::abort();
	}));
}
