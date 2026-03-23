use std::{path::PathBuf, time::Duration};

use clap::{
	Args, Parser, Subcommand, ValueEnum,
	builder::{
		Styles,
		styling::{AnsiColor, Effects},
	},
};

use crate::{
	agent,
	orchestrator::{self, IssueDispatchMode},
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
			Command::Status(args) => args.run(),
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
	/// Inspect the current local runtime state for one configured project.
	Status(StatusCommand),
	/// Inspect or validate the local app-server integration boundary.
	Protocol(ProtocolCommand),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum RunIssueDispatchMode {
	Normal,
	Retry,
	ReviewRepair,
}
impl From<RunIssueDispatchMode> for IssueDispatchMode {
	fn from(value: RunIssueDispatchMode) -> Self {
		match value {
			RunIssueDispatchMode::Normal => Self::Normal,
			RunIssueDispatchMode::Retry => Self::Retry,
			RunIssueDispatchMode::ReviewRepair => Self::ReviewRepair,
		}
	}
}

#[derive(Debug, Args)]
struct RunCommand {
	/// Run only a single orchestration iteration.
	#[arg(long)]
	once: bool,
	/// Skip external side effects where the later implementation allows it.
	#[arg(long)]
	dry_run: bool,
	/// Run a specific leased or queued issue instead of normal candidate selection.
	#[arg(long, value_name = "ISSUE_ID")]
	issue_id: Option<String>,
	/// Reuse a daemon-planned issue state for an issue-targeted child run.
	#[arg(long, value_name = "STATE", hide = true, requires = "issue_id")]
	issue_state: Option<String>,
	/// Reuse the original tracker state captured when the daemon planned the run.
	#[arg(long, value_name = "STATE", hide = true, requires = "issue_id")]
	initial_issue_state: Option<String>,
	/// Override the dispatch policy for an issue-targeted run.
	#[arg(long, value_name = "MODE", value_enum, hide = true, requires = "issue_id")]
	dispatch_mode: Option<RunIssueDispatchMode>,
	/// Reuse a daemon-held lease for an issue-targeted child run.
	#[arg(long, hide = true, requires = "issue_id")]
	lease_preacquired: bool,
	/// Reuse the daemon-held issue-claim fd for an issue-targeted child run.
	#[arg(long, value_name = "FD", hide = true, requires = "issue_id")]
	issue_claim_fd: Option<i32>,
	/// Reuse the daemon-held dispatch-slot fd for an issue-targeted child run.
	#[arg(long, value_name = "FD", hide = true, requires = "issue_id")]
	dispatch_slot_fd: Option<i32>,
	/// Reuse the daemon-held dispatch-slot index for an issue-targeted child run.
	#[arg(long, value_name = "INDEX", hide = true, requires = "issue_id")]
	dispatch_slot_index: Option<usize>,
	/// Reuse a daemon-planned run identifier for an issue-targeted run.
	#[arg(long, value_name = "RUN_ID", hide = true, requires = "issue_id")]
	run_id: Option<String>,
	/// Reuse a daemon-planned attempt number for an issue-targeted run.
	#[arg(long, value_name = "ATTEMPT", hide = true, requires = "issue_id")]
	attempt_number: Option<i64>,
	/// Reuse the daemon-known retry-budget count before this issue-targeted run starts.
	#[arg(long, value_name = "COUNT", hide = true, requires = "issue_id")]
	retry_budget_base: Option<i64>,
	/// Reuse a daemon-planned workflow snapshot for an issue-targeted run.
	#[arg(long, value_name = "MARKDOWN", hide = true, requires = "issue_id")]
	workflow_snapshot: Option<String>,
	/// Override the service config path.
	#[arg(long, value_name = "PATH")]
	config: Option<PathBuf>,
}
impl RunCommand {
	fn run(&self) -> Result<()> {
		if !self.once {
			eyre::bail!("`run` currently requires `--once` for the MVP.");
		}
		if self.run_id.is_some() != self.attempt_number.is_some() {
			eyre::bail!(
				"`run --once --issue-id` requires `--run-id` and `--attempt-number` together."
			);
		}

		orchestrator::run_once(orchestrator::RunOnceRequest {
			config_path: self.config.as_deref(),
			dry_run: self.dry_run,
			preferred_issue_id: self.issue_id.as_deref(),
			preferred_issue_state: self.issue_state.as_deref(),
			preferred_initial_issue_state: self.initial_issue_state.as_deref(),
			preferred_lease_acquired: self.lease_preacquired,
			preferred_issue_claim_fd: self.issue_claim_fd,
			preferred_dispatch_slot_fd: self.dispatch_slot_fd,
			preferred_dispatch_slot_index: self.dispatch_slot_index,
			preferred_dispatch_mode: self.dispatch_mode.map(Into::into),
			preferred_run_id: self.run_id.as_deref(),
			preferred_attempt_number: self.attempt_number,
			preferred_retry_budget_base: self.retry_budget_base,
			preferred_workflow_snapshot: self.workflow_snapshot.as_deref(),
		})
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
struct StatusCommand {
	/// Override the service config path.
	#[arg(long, value_name = "PATH")]
	config: Option<PathBuf>,
	/// Emit structured JSON instead of human-readable text.
	#[arg(long)]
	json: bool,
	/// Maximum number of recent runs to display.
	#[arg(long, value_name = "COUNT", default_value_t = orchestrator::DEFAULT_STATUS_RUN_LIMIT)]
	limit: usize,
}
impl StatusCommand {
	fn run(&self) -> Result<()> {
		orchestrator::print_status(self.config.as_deref(), self.json, self.limit)
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
		RunCommand, RunIssueDispatchMode, StatusCommand,
	};

	#[test]
	fn parses_run_once_dry_run() {
		let cli = Cli::parse_from(["maestro", "run", "--once", "--dry-run"]);

		assert!(matches!(
			cli.command,
			Command::Run(RunCommand {
				once: true,
				dry_run: true,
				issue_id: None,
				issue_state: None,
				initial_issue_state: None,
				lease_preacquired: false,
				issue_claim_fd: None,
				dispatch_slot_fd: None,
				dispatch_slot_index: None,
				dispatch_mode: None,
				run_id: None,
				attempt_number: None,
				retry_budget_base: None,
				workflow_snapshot: None,
				config: None
			})
		));
	}

	#[test]
	fn parses_run_once_with_issue_override() {
		let cli = Cli::parse_from(["maestro", "run", "--once", "--issue-id", "issue-1"]);

		assert!(matches!(
			cli.command,
			Command::Run(RunCommand {
				once: true,
				dry_run: false,
				issue_id: Some(_),
				issue_state: None,
				initial_issue_state: None,
				lease_preacquired: false,
				issue_claim_fd: None,
				dispatch_slot_fd: None,
				dispatch_slot_index: None,
				dispatch_mode: None,
				run_id: None,
				attempt_number: None,
				retry_budget_base: None,
				workflow_snapshot: None,
				config: None
			})
		));
	}

	#[test]
	fn parses_run_once_with_hidden_daemon_identity_override() {
		let cli = Cli::parse_from([
			"maestro",
			"run",
			"--once",
			"--issue-id",
			"issue-1",
			"--dispatch-mode",
			"normal",
			"--issue-state",
			"Todo",
			"--initial-issue-state",
			"Todo",
			"--run-id",
			"mae-1-attempt-2-123",
			"--attempt-number",
			"2",
			"--retry-budget-base",
			"1",
			"--workflow-snapshot",
			"workflow-snapshot",
		]);

		assert!(matches!(
			cli.command,
			Command::Run(RunCommand {
				once: true,
				dry_run: false,
				issue_id: Some(_),
				issue_state: Some(_),
				initial_issue_state: Some(_),
				lease_preacquired: false,
				issue_claim_fd: None,
				dispatch_slot_fd: None,
				dispatch_slot_index: None,
				dispatch_mode: Some(RunIssueDispatchMode::Normal),
				run_id: Some(_),
				attempt_number: Some(2),
				retry_budget_base: Some(1),
				workflow_snapshot: Some(_),
				config: None
			})
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
			"./tmp/maestro.toml",
		]);

		assert!(matches!(
			cli.command,
			Command::Daemon(DaemonCommand { poll_interval_s: 5, config: Some(_) })
		));
	}

	#[test]
	fn parses_status_with_json_limit_and_config() {
		let cli = Cli::parse_from([
			"maestro",
			"status",
			"--json",
			"--limit",
			"5",
			"--config",
			"./tmp/maestro.toml",
		]);

		assert!(matches!(
			cli.command,
			Command::Status(StatusCommand { json: true, limit: 5, config: Some(_) })
		));
	}
}
