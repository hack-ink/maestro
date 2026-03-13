use std::{path::PathBuf, time::Duration};

use clap::{
	Args, Parser, Subcommand,
	builder::{
		Styles,
		styling::{AnsiColor, Effects},
	},
};

use crate::{
	agent, orchestrator,
	prelude::{Result, eyre},
};

/// Root CLI parser for the Maestro control plane.
#[derive(Debug, Parser)]
#[command(
	about = "Repo-native orchestration for autonomous coding agents.",
	version = concat!(
		env!("CARGO_PKG_VERSION"),
		"-",
		env!("VERGEN_GIT_SHA"),
		"-",
		env!("VERGEN_CARGO_TARGET_TRIPLE"),
	),
	arg_required_else_help = true,
	rename_all = "kebab",
	subcommand_required = true,
	styles = styles(),
)]
pub(crate) struct Cli {
	#[command(subcommand)]
	command: Command,
}
impl Cli {
	pub(crate) fn run(&self) -> Result<()> {
		match &self.command {
			Command::Run(args) => args.run(),
			Command::Daemon(args) => args.run(),
			Command::Protocol(args) => args.run(),
		}
	}
}

#[derive(Debug, Subcommand)]
enum Command {
	/// Run one orchestration pass or a bounded execution mode.
	Run(RunCommand),
	/// Start the long-running poll loop.
	Daemon(DaemonCommand),
	/// Inspect or validate the local app-server integration boundary.
	Protocol(ProtocolCommand),
}

#[derive(Debug, Args)]
struct RunCommand {
	/// Run only a single orchestration iteration.
	#[arg(long)]
	once: bool,
	/// Skip external side effects where the later implementation allows it.
	#[arg(long)]
	dry_run: bool,
	/// Override the service config path.
	#[arg(long, value_name = "PATH")]
	config: Option<PathBuf>,
}
impl RunCommand {
	fn run(&self) -> Result<()> {
		if !self.once {
			eyre::bail!("`run` currently requires `--once` for the MVP.");
		}

		orchestrator::run_once(self.config.as_deref(), self.dry_run)
	}
}

#[derive(Debug, Args)]
struct DaemonCommand {
	/// Poll interval in seconds for the long-running loop.
	#[arg(long, value_name = "SECONDS", default_value_t = 60)]
	poll_interval_s: u64,
	/// Override the service config path.
	#[arg(long, value_name = "PATH")]
	config: Option<PathBuf>,
}
impl DaemonCommand {
	fn run(&self) -> Result<()> {
		orchestrator::run_daemon(self.config.as_deref(), Duration::from_secs(self.poll_interval_s))
	}
}

#[derive(Debug, Args)]
struct ProtocolCommand {
	#[command(subcommand)]
	command: ProtocolSubcommand,
}
impl ProtocolCommand {
	fn run(&self) -> Result<()> {
		match &self.command {
			ProtocolSubcommand::Probe(args) => args.run(),
		}
	}
}

#[derive(Debug, Subcommand)]
enum ProtocolSubcommand {
	/// Validate the local app-server contract before orchestration depends on it.
	Probe(ProtocolProbeCommand),
}

#[derive(Debug, Args)]
struct ProtocolProbeCommand {
	/// Override the expected app-server transport during probing.
	#[arg(long, default_value = "stdio://")]
	listen: String,
}
impl ProtocolProbeCommand {
	fn run(&self) -> Result<()> {
		let report = agent::probe_app_server(&self.listen)?;

		println!(
			"protocol probe ok: thread={} turn={} events={} output={}",
			report.thread_id, report.turn_id, report.event_count, report.final_output
		);

		tracing::info!(
			user_agent = %report.user_agent,
			thread_id = %report.thread_id,
			turn_id = %report.turn_id,
			event_count = report.event_count,
			"Completed protocol probe."
		);

		Ok(())
	}
}

fn styles() -> Styles {
	Styles::styled()
		.header(AnsiColor::Red.on_default() | Effects::BOLD)
		.usage(AnsiColor::Red.on_default() | Effects::BOLD)
		.literal(AnsiColor::Blue.on_default() | Effects::BOLD)
		.placeholder(AnsiColor::Green.on_default())
}

#[cfg(test)]
mod tests {
	use clap::Parser;

	use crate::cli::{
		Cli, Command, DaemonCommand, ProtocolCommand, ProtocolProbeCommand, ProtocolSubcommand,
		RunCommand,
	};

	#[test]
	fn parses_run_once_dry_run() {
		let cli = Cli::parse_from(["maestro", "run", "--once", "--dry-run"]);

		assert!(matches!(
			cli.command,
			Command::Run(RunCommand { once: true, dry_run: true, config: None })
		));
	}

	#[test]
	fn parses_protocol_probe_with_custom_transport() {
		let cli =
			Cli::parse_from(["maestro", "protocol", "probe", "--listen", "ws://127.0.0.1:9000"]);

		assert!(matches!(
			cli.command,
			Command::Protocol(ProtocolCommand {
				command: ProtocolSubcommand::Probe(ProtocolProbeCommand { .. })
			})
		));
	}

	#[test]
	fn parses_daemon_with_poll_interval_and_config() {
		let cli = Cli::parse_from([
			"maestro",
			"daemon",
			"--poll-interval-s",
			"5",
			"--config",
			"./maestro.toml",
		]);

		assert!(matches!(
			cli.command,
			Command::Daemon(DaemonCommand { poll_interval_s: 5, config: Some(_) })
		));
	}
}
