use std::{
	env,
	path::{Path, PathBuf},
	time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
	agent::{
		json_rpc::{
			JsonRpcConnection, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, WireMessage,
		},
		tracker_tool_bridge::{
			DynamicToolCallResponse, DynamicToolContentItem, DynamicToolHandler, DynamicToolSpec,
		},
	},
	prelude::{Result, eyre},
	state::{self, StateStore},
};

pub(crate) const ACTIVE_RUN_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_RUN_ID: &str = "protocol-probe-run";
const PROBE_ISSUE_ID: &str = "protocol-probe";
const PROBE_EXPECTED_OUTPUT: &str = "PROBE_OK";
const DEFAULT_TURN_EFFORT: &str = "high";
const PROBE_DEVELOPER_INSTRUCTIONS: &str = "You are a protocol probe. You must call the dynamic tool `echo_probe` exactly once with the JSON argument `{\"text\":\"PROBE_OK\"}`. Do not use shell. Do not inspect files. After the tool response is returned, reply with the exact text PROBE_OK and nothing else.";
const PROBE_USER_INPUT: &str = "Call `echo_probe` with `{\\\"text\\\":\\\"PROBE_OK\\\"}`. After the tool succeeds, reply with the exact text PROBE_OK.";

#[derive(Clone)]
pub(crate) struct AppServerRunRequest<'a> {
	pub(crate) run_id: String,
	pub(crate) issue_id: String,
	pub(crate) attempt_number: i64,
	pub(crate) listen: String,
	pub(crate) cwd: String,
	pub(crate) approval_policy: String,
	pub(crate) sandbox: String,
	pub(crate) developer_instructions: String,
	pub(crate) user_input: String,
	pub(crate) model: Option<String>,
	pub(crate) personality: Option<String>,
	pub(crate) service_tier: Option<String>,
	pub(crate) timeout: Duration,
	pub(crate) activity_marker_path: Option<PathBuf>,
	pub(crate) dynamic_tool_handler: Option<&'a dyn DynamicToolHandler>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AppServerRunResult {
	pub(crate) user_agent: String,
	pub(crate) thread_id: String,
	pub(crate) turn_id: String,
	pub(crate) event_count: i64,
	pub(crate) final_output: String,
}

struct AppServerClient {
	connection: JsonRpcConnection,
}
impl AppServerClient {
	fn spawn(listen: &str) -> Result<Self> {
		Ok(Self { connection: JsonRpcConnection::spawn_app_server(listen)? })
	}

	fn initialize(&mut self, enable_experimental_api: bool) -> Result<InitializeResponse> {
		self.connection.request(
			"initialize",
			&InitializeParams {
				client_info: ClientInfo {
					name: env!("CARGO_PKG_NAME").to_owned(),
					version: env!("CARGO_PKG_VERSION").to_owned(),
				},
				capabilities: enable_experimental_api.then_some(InitializeCapabilities {
					experimental_api: Some(true),
					opt_out_notification_methods: Vec::new(),
				}),
			},
			REQUEST_TIMEOUT,
		)
	}

	fn mark_initialized(&mut self) -> Result<()> {
		self.connection.notify::<Value>("initialized", None)
	}

	fn start_thread(&mut self, params: ThreadStartRequest) -> Result<ThreadStartResponse> {
		self.connection.request("thread/start", &params, REQUEST_TIMEOUT)
	}

	fn start_turn(&mut self, params: TurnStartRequest) -> Result<TurnStartResponse> {
		self.connection.request("turn/start", &params, REQUEST_TIMEOUT)
	}

	fn recv(&mut self, timeout: Option<Duration>) -> Result<WireMessage> {
		self.connection.recv(timeout)
	}

	fn respond<R>(&mut self, id: &Value, result: &R) -> Result<()>
	where
		R: Serialize,
	{
		self.connection.respond(id, result)
	}

	fn drain_pending(&mut self) -> Vec<WireMessage> {
		self.connection.drain_pending()
	}
}

#[derive(Default)]
struct RunOutcome {
	final_output: String,
}

struct RunRecorder<'a> {
	state_store: &'a StateStore,
	run_id: &'a str,
	attempt_number: i64,
	activity_marker_path: Option<&'a PathBuf>,
	next_sequence: i64,
}
impl<'a> RunRecorder<'a> {
	fn new(
		state_store: &'a StateStore,
		run_id: &'a str,
		attempt_number: i64,
		activity_marker_path: Option<&'a PathBuf>,
	) -> Self {
		Self { state_store, run_id, attempt_number, activity_marker_path, next_sequence: 1 }
	}

	fn mark_activity(&self) -> Result<()> {
		if let Some(marker_path) = self.activity_marker_path {
			write_activity_marker_best_effort(marker_path, self.run_id, self.attempt_number);
		}

		Ok(())
	}

	fn record(&mut self, event_type: &str, payload: &str) -> Result<()> {
		self.state_store.append_event(self.run_id, self.next_sequence, event_type, payload)?;

		if let Some(marker_path) = self.activity_marker_path {
			write_protocol_activity_marker_best_effort(
				marker_path,
				self.run_id,
				self.attempt_number,
			);
		}

		self.next_sequence += 1;

		Ok(())
	}
}

#[derive(Debug, Serialize)]
struct InitializeParams {
	#[serde(rename = "clientInfo")]
	client_info: ClientInfo,
	#[serde(skip_serializing_if = "Option::is_none")]
	capabilities: Option<InitializeCapabilities>,
}

#[derive(Debug, Serialize)]
struct ClientInfo {
	name: String,
	version: String,
}

#[derive(Debug, Serialize)]
struct InitializeCapabilities {
	#[serde(rename = "experimentalApi", skip_serializing_if = "Option::is_none")]
	experimental_api: Option<bool>,
	#[serde(default, rename = "optOutNotificationMethods", skip_serializing_if = "Vec::is_empty")]
	opt_out_notification_methods: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct InitializeResponse {
	#[serde(rename = "userAgent")]
	user_agent: String,
}

#[derive(Debug, Default, Serialize)]
struct ThreadStartRequest {
	#[serde(rename = "approvalPolicy", skip_serializing_if = "Option::is_none")]
	approval_policy: Option<String>,
	#[serde(rename = "baseInstructions", skip_serializing_if = "Option::is_none")]
	base_instructions: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	config: Option<Value>,
	#[serde(skip_serializing_if = "Option::is_none")]
	cwd: Option<String>,
	#[serde(rename = "dynamicTools", skip_serializing_if = "Option::is_none")]
	dynamic_tools: Option<Vec<DynamicToolSpec>>,
	#[serde(rename = "developerInstructions", skip_serializing_if = "Option::is_none")]
	developer_instructions: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	ephemeral: Option<bool>,
	#[serde(skip_serializing_if = "Option::is_none")]
	model: Option<String>,
	#[serde(rename = "modelProvider", skip_serializing_if = "Option::is_none")]
	model_provider: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	personality: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	sandbox: Option<String>,
	#[serde(rename = "serviceName", skip_serializing_if = "Option::is_none")]
	service_name: Option<String>,
	#[serde(rename = "serviceTier", skip_serializing_if = "Option::is_none")]
	service_tier: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct ThreadStartResponse {
	thread: Thread,
}

#[derive(Debug, Deserialize)]
struct Thread {
	id: String,
}

#[derive(Debug, Default, Serialize)]
struct TurnStartRequest {
	#[serde(rename = "approvalPolicy", skip_serializing_if = "Option::is_none")]
	approval_policy: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	cwd: Option<String>,
	#[serde(skip_serializing_if = "Option::is_none")]
	effort: Option<String>,
	input: Vec<UserInput>,
	#[serde(skip_serializing_if = "Option::is_none")]
	model: Option<String>,
	#[serde(rename = "outputSchema", skip_serializing_if = "Option::is_none")]
	output_schema: Option<Value>,
	#[serde(skip_serializing_if = "Option::is_none")]
	personality: Option<String>,
	#[serde(rename = "sandboxPolicy", skip_serializing_if = "Option::is_none")]
	sandbox_policy: Option<Value>,
	#[serde(rename = "serviceTier", skip_serializing_if = "Option::is_none")]
	service_tier: Option<Value>,
	#[serde(skip_serializing_if = "Option::is_none")]
	summary: Option<String>,
	#[serde(rename = "threadId")]
	thread_id: String,
}

#[derive(Debug, Deserialize)]
struct TurnStartResponse {
	turn: TurnStatusPayload,
}

#[derive(Debug, Deserialize)]
struct TurnStatusPayload {
	id: String,
	status: String,
	error: Option<TurnError>,
}

#[derive(Debug, Deserialize)]
struct TurnError {
	message: String,
}

#[derive(Debug, Deserialize)]
struct ThreadStatusChangedNotification {
	#[serde(rename = "threadId")]
	thread_id: String,
	status: ThreadStatus,
}

#[derive(Debug, Deserialize)]
struct ThreadStatus {
	#[serde(rename = "type")]
	kind: String,
	#[serde(default, rename = "activeFlags")]
	active_flags: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AgentMessageDeltaNotification {
	delta: String,
}

#[derive(Debug, Deserialize)]
struct ItemCompletedNotification {
	item: CompletedItem,
}

#[derive(Debug, Deserialize)]
struct CompletedItem {
	#[serde(rename = "type")]
	kind: String,
	text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TurnCompletedNotification {
	turn: TurnStatusPayload,
}

#[derive(Debug, Deserialize)]
struct DynamicToolCallParams {
	arguments: Value,
	#[serde(rename = "callId")]
	_call_id: String,
	#[serde(rename = "threadId")]
	thread_id: String,
	tool: String,
	#[serde(rename = "turnId")]
	turn_id: String,
}

struct ProbeDynamicToolHandler;
impl DynamicToolHandler for ProbeDynamicToolHandler {
	fn tool_specs(&self) -> Vec<DynamicToolSpec> {
		vec![DynamicToolSpec {
			name: String::from("echo_probe"),
			description: String::from("Echo the provided text back to the model."),
			input_schema: json!({
				"type": "object",
				"properties": {
					"text": { "type": "string" }
				},
				"required": ["text"],
				"additionalProperties": false
			}),
		}]
	}

	fn handle_call(&self, tool_name: &str, arguments: Value) -> DynamicToolCallResponse {
		if tool_name != "echo_probe" {
			return DynamicToolCallResponse::failure(format!(
				"Unexpected probe tool `{tool_name}`."
			));
		}

		let Some(text) = arguments.get("text").and_then(Value::as_str) else {
			return DynamicToolCallResponse::failure(String::from(
				"`echo_probe` requires a string `text` argument.",
			));
		};

		DynamicToolCallResponse::success(text.to_owned())
	}
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum UserInput {
	#[serde(rename = "text")]
	Text { text: String },
}

pub(crate) fn execute_app_server_run(
	request: &AppServerRunRequest<'_>,
	state_store: &StateStore,
) -> Result<AppServerRunResult> {
	state_store.record_run_attempt(
		&request.run_id,
		&request.issue_id,
		request.attempt_number,
		"starting",
	)?;

	if let Some(marker_path) = request.activity_marker_path.as_ref() {
		write_activity_marker_best_effort(marker_path, &request.run_id, request.attempt_number);
	}

	let result = execute_app_server_run_inner(request, state_store);

	if result.is_err() {
		state_store.record_run_attempt(
			&request.run_id,
			&request.issue_id,
			request.attempt_number,
			"failed",
		)?;

		if let Some(marker_path) = request.activity_marker_path.as_ref() {
			write_activity_marker_best_effort(marker_path, &request.run_id, request.attempt_number);
		}
	}

	result
}

pub(crate) fn probe_app_server(listen: &str) -> Result<AppServerRunResult> {
	let state_store = StateStore::open_in_memory()?;
	let probe_tool_handler = ProbeDynamicToolHandler;
	let result = execute_app_server_run(
		&AppServerRunRequest {
			run_id: PROBE_RUN_ID.to_owned(),
			issue_id: PROBE_ISSUE_ID.to_owned(),
			attempt_number: 1,
			listen: listen.to_owned(),
			cwd: env::current_dir()?.display().to_string(),
			approval_policy: String::from("never"),
			sandbox: String::from("workspace-write"),
			developer_instructions: PROBE_DEVELOPER_INSTRUCTIONS.to_owned(),
			user_input: PROBE_USER_INPUT.to_owned(),
			model: None,
			personality: None,
			service_tier: None,
			timeout: PROBE_TIMEOUT,
			activity_marker_path: None,
			dynamic_tool_handler: Some(&probe_tool_handler),
		},
		&state_store,
	)?;

	if result.final_output.trim() != PROBE_EXPECTED_OUTPUT {
		eyre::bail!(
			"Protocol probe completed, but the final output was `{}` instead of `{PROBE_EXPECTED_OUTPUT}`.",
			result.final_output.trim()
		);
	}

	Ok(result)
}

fn write_activity_marker_best_effort(marker_path: &Path, run_id: &str, attempt_number: i64) {
	if let Err(error) = state::write_run_activity_marker(marker_path, run_id, attempt_number) {
		tracing::warn!(
			?error,
			run_id,
			attempt_number,
			marker_path = %marker_path.display(),
			"Failed to update workspace activity marker."
		);
	}
}

fn write_protocol_activity_marker_best_effort(
	marker_path: &Path,
	run_id: &str,
	attempt_number: i64,
) {
	if let Err(error) =
		state::write_run_protocol_activity_marker(marker_path, run_id, attempt_number)
	{
		tracing::warn!(
			?error,
			run_id,
			attempt_number,
			marker_path = %marker_path.display(),
			"Failed to update workspace protocol-activity marker."
		);
	}
}

fn execute_app_server_run_inner(
	request: &AppServerRunRequest<'_>,
	state_store: &StateStore,
) -> Result<AppServerRunResult> {
	let mut recorder = RunRecorder::new(
		state_store,
		&request.run_id,
		request.attempt_number,
		request.activity_marker_path.as_ref(),
	);
	let mut client = AppServerClient::spawn(&request.listen)?;
	let initialize_response = client.initialize(request.dynamic_tool_handler.is_some())?;

	client.mark_initialized()?;

	flush_pending_messages(&mut client, &mut recorder, None)?;

	let thread_response = client.start_thread(ThreadStartRequest {
		cwd: Some(request.cwd.clone()),
		dynamic_tools: request.dynamic_tool_handler.map(|handler| handler.tool_specs()),
		approval_policy: Some(request.approval_policy.clone()),
		developer_instructions: Some(request.developer_instructions.clone()),
		model: request.model.clone(),
		personality: request.personality.clone(),
		sandbox: Some(request.sandbox.clone()),
		service_tier: request.service_tier.clone().map(Value::String),
		..ThreadStartRequest::default()
	})?;
	let thread_id = thread_response.thread.id.clone();

	state_store.update_run_thread(&request.run_id, &thread_id)?;
	recorder.mark_activity()?;

	flush_pending_messages(&mut client, &mut recorder, Some(&thread_id))?;

	let turn_response =
		client.start_turn(build_turn_start_request(&thread_id, &request.user_input))?;
	let turn_id = turn_response.turn.id.clone();

	state_store.record_run_attempt(
		&request.run_id,
		&request.issue_id,
		request.attempt_number,
		"running",
	)?;
	recorder.mark_activity()?;

	flush_pending_messages(&mut client, &mut recorder, Some(&thread_id))?;

	let run_outcome = wait_for_turn_completion(
		&mut client,
		&mut recorder,
		&thread_id,
		&turn_id,
		request.timeout,
		request.dynamic_tool_handler,
	)?;

	validate_turn_completion(request.dynamic_tool_handler, &run_outcome.final_output)?;

	state_store.record_run_attempt(
		&request.run_id,
		&request.issue_id,
		request.attempt_number,
		"succeeded",
	)?;
	recorder.mark_activity()?;

	Ok(AppServerRunResult {
		user_agent: initialize_response.user_agent,
		thread_id,
		turn_id,
		event_count: state_store.event_count(&request.run_id)?,
		final_output: run_outcome.final_output,
	})
}

fn validate_turn_completion(
	dynamic_tool_handler: Option<&dyn DynamicToolHandler>,
	final_output: &str,
) -> Result<()> {
	if let Some(dynamic_tool_handler) = dynamic_tool_handler {
		dynamic_tool_handler.validate_turn_completion(final_output)?;
	}

	Ok(())
}

fn build_turn_start_request(thread_id: &str, user_input: &str) -> TurnStartRequest {
	TurnStartRequest {
		thread_id: thread_id.to_owned(),
		effort: Some(String::from(DEFAULT_TURN_EFFORT)),
		input: vec![UserInput::Text { text: user_input.to_owned() }],
		..TurnStartRequest::default()
	}
}

fn flush_pending_messages(
	client: &mut AppServerClient,
	recorder: &mut RunRecorder<'_>,
	target_thread_id: Option<&str>,
) -> Result<()> {
	for message in client.drain_pending() {
		if targets_thread(&message, target_thread_id) {
			recorder.record(message_type(&message), &message.raw)?;
		}
	}

	Ok(())
}

fn wait_for_turn_completion(
	client: &mut AppServerClient,
	recorder: &mut RunRecorder<'_>,
	target_thread_id: &str,
	target_turn_id: &str,
	timeout: Duration,
	dynamic_tool_handler: Option<&dyn DynamicToolHandler>,
) -> Result<RunOutcome> {
	let mut last_activity_at = Instant::now();
	let mut final_output = String::new();

	loop {
		let now = Instant::now();
		let Some(wait_timeout) = remaining_idle_budget(last_activity_at, now, timeout) else {
			eyre::bail!(
				"Timed out while waiting for turn `{target_turn_id}` on thread `{target_thread_id}`."
			);
		};
		let wire_message = client.recv(Some(wait_timeout))?;

		if !targets_thread(&wire_message, Some(target_thread_id)) {
			tracing::debug!(raw = %wire_message.raw, "Ignoring app-server message for another thread.");

			continue;
		}

		last_activity_at = Instant::now();

		recorder.record(message_type(&wire_message), &wire_message.raw)?;

		match &wire_message.message {
			JsonRpcMessage::Notification(notification) => match notification.method.as_str() {
				"thread/status/changed" => {
					let payload: ThreadStatusChangedNotification =
						serde_json::from_value(notification.params.clone())?;

					if payload.status.kind == "systemError" {
						eyre::bail!("Thread `{}` entered `systemError` status.", payload.thread_id);
					}
					if payload.status.kind == "active"
						&& payload
							.status
							.active_flags
							.iter()
							.any(|flag| flag == "waitingOnApproval" || flag == "waitingOnUserInput")
					{
						eyre::bail!(
							"Thread `{}` requested interactive input, which is unsupported for Maestro.",
							payload.thread_id
						);
					}
				},
				"item/agentMessage/delta" => {
					let payload: AgentMessageDeltaNotification =
						serde_json::from_value(notification.params.clone())?;

					final_output.push_str(&payload.delta);
				},
				"item/completed" => {
					let payload: ItemCompletedNotification =
						serde_json::from_value(notification.params.clone())?;

					if payload.item.kind == "agentMessage"
						&& let Some(text) = payload.item.text
					{
						final_output = text;
					}
				},
				"turn/completed" => {
					let payload: TurnCompletedNotification =
						serde_json::from_value(notification.params.clone())?;

					if payload.turn.id != target_turn_id {
						continue;
					}
					if payload.turn.status == "completed" {
						return Ok(RunOutcome { final_output });
					}

					let error_message = payload
						.turn
						.error
						.as_ref()
						.map(|error| error.message.as_str())
						.unwrap_or("turn completed without an explicit error payload");

					eyre::bail!(
						"Turn `{}` ended with status `{}`: {}",
						payload.turn.id,
						payload.turn.status,
						error_message
					);
				},
				_ => {},
			},
			JsonRpcMessage::Request(request) => {
				if request.method == "item/tool/call" {
					let response = handle_dynamic_tool_call(
						dynamic_tool_handler,
						request,
						target_thread_id,
						target_turn_id,
					);

					client.respond(&request.id, &response)?;
					recorder
						.record("item/tool/call/response", &serde_json::to_string(&response)?)?;

					continue;
				}

				eyre::bail!(
					"Received unexpected server request `{}` during non-interactive execution.",
					request.method
				);
			},
			JsonRpcMessage::Response(_) | JsonRpcMessage::Error(_) => {
				eyre::bail!(
					"Received an unexpected JSON-RPC response while waiting for turn completion."
				);
			},
		}
	}
}

fn remaining_idle_budget(
	last_activity_at: Instant,
	now: Instant,
	timeout: Duration,
) -> Option<Duration> {
	timeout.checked_sub(now.saturating_duration_since(last_activity_at))
}

fn handle_dynamic_tool_call(
	dynamic_tool_handler: Option<&dyn DynamicToolHandler>,
	request: &JsonRpcRequest,
	target_thread_id: &str,
	target_turn_id: &str,
) -> DynamicToolCallResponse {
	let payload = match serde_json::from_value::<DynamicToolCallParams>(request.params.clone()) {
		Ok(payload) => payload,
		Err(error) => {
			return DynamicToolCallResponse {
				content_items: vec![DynamicToolContentItem::InputText {
					text: format!("Invalid `item/tool/call` payload: {error}"),
				}],
				success: false,
			};
		},
	};

	if payload.thread_id != target_thread_id {
		return DynamicToolCallResponse {
			content_items: vec![DynamicToolContentItem::InputText {
				text: format!(
					"Dynamic tool call targeted thread `{}`, but the active thread is `{target_thread_id}`.",
					payload.thread_id
				),
			}],
			success: false,
		};
	}
	if payload.turn_id != target_turn_id {
		return DynamicToolCallResponse {
			content_items: vec![DynamicToolContentItem::InputText {
				text: format!(
					"Dynamic tool call targeted turn `{}`, but the active turn is `{target_turn_id}`.",
					payload.turn_id
				),
			}],
			success: false,
		};
	}

	let Some(dynamic_tool_handler) = dynamic_tool_handler else {
		return DynamicToolCallResponse {
			content_items: vec![DynamicToolContentItem::InputText {
				text: String::from("Dynamic tool bridge is unavailable for this run attempt."),
			}],
			success: false,
		};
	};

	dynamic_tool_handler.handle_call(&payload.tool, payload.arguments)
}

fn message_type(message: &WireMessage) -> &str {
	match &message.message {
		JsonRpcMessage::Notification(notification) => notification.method.as_str(),
		JsonRpcMessage::Request(request) => request.method.as_str(),
		JsonRpcMessage::Response(_) => "json-rpc/response",
		JsonRpcMessage::Error(_) => "json-rpc/error",
	}
}

fn targets_thread(message: &WireMessage, target_thread_id: Option<&str>) -> bool {
	let Some(target_thread_id) = target_thread_id else {
		return true;
	};

	match &message.message {
		JsonRpcMessage::Notification(notification) => thread_id_from_notification(notification)
			.is_none_or(|thread_id| thread_id == target_thread_id),
		JsonRpcMessage::Request(request) => thread_id_from_value(&request.params)
			.is_none_or(|thread_id| thread_id == target_thread_id),
		JsonRpcMessage::Response(_) | JsonRpcMessage::Error(_) => true,
	}
}

fn thread_id_from_notification(notification: &JsonRpcNotification) -> Option<&str> {
	thread_id_from_value(&notification.params)
}

fn thread_id_from_value(value: &Value) -> Option<&str> {
	value
		.get("threadId")
		.and_then(Value::as_str)
		.or_else(|| value.get("thread").and_then(|thread| thread.get("id")).and_then(Value::as_str))
}

#[cfg(test)]
mod tests {
	use std::{
		path::PathBuf,
		time::{Duration, Instant},
	};

	use tempfile::TempDir;

	use crate::{
		agent::{
			app_server::{AppServerRunResult, ProbeDynamicToolHandler},
			json_rpc::{JsonRpcMessage, JsonRpcNotification, WireMessage},
			tracker_tool_bridge::{DynamicToolCallResponse, DynamicToolHandler, DynamicToolSpec},
		},
		state::StateStore,
	};

	struct RejectingCompletionHandler;
	impl DynamicToolHandler for RejectingCompletionHandler {
		fn tool_specs(&self) -> Vec<DynamicToolSpec> {
			Vec::new()
		}

		fn handle_call(
			&self,
			_tool_name: &str,
			_arguments: serde_json::Value,
		) -> DynamicToolCallResponse {
			DynamicToolCallResponse::failure(String::from("unused"))
		}

		fn validate_turn_completion(&self, _final_output: &str) -> crate::prelude::Result<()> {
			Err(crate::prelude::eyre::eyre!("terminal finalization missing"))
		}
	}

	fn notification_message(method: &str, params: serde_json::Value) -> WireMessage {
		WireMessage {
			raw: params.to_string(),
			message: JsonRpcMessage::Notification(JsonRpcNotification {
				method: method.to_owned(),
				params,
			}),
		}
	}

	#[test]
	fn matches_thread_id_from_thread_started_notification() {
		let message = notification_message(
			"thread/started",
			serde_json::json!({
				"thread": {
					"id": "thread-1",
				}
			}),
		);

		assert!(super::targets_thread(&message, Some("thread-1")));
		assert!(!super::targets_thread(&message, Some("thread-2")));
	}

	#[test]
	fn matches_thread_id_from_thread_id_field() {
		let message = notification_message(
			"turn/completed",
			serde_json::json!({
				"threadId": "thread-1",
				"turn": {
					"id": "turn-1",
					"status": "completed",
					"error": null,
				}
			}),
		);

		assert!(super::targets_thread(&message, Some("thread-1")));
		assert!(!super::targets_thread(&message, Some("thread-2")));
	}

	#[test]
	fn probe_result_shape_is_stable() {
		let result = AppServerRunResult {
			user_agent: String::from("ua"),
			thread_id: String::from("thread"),
			turn_id: String::from("turn"),
			event_count: 3,
			final_output: String::from("PROBE_OK"),
		};

		assert_eq!(result.final_output, "PROBE_OK");
	}

	#[test]
	fn turn_start_request_uses_supported_effort() {
		let request = super::build_turn_start_request("thread-1", "hello");

		assert_eq!(request.thread_id, "thread-1");
		assert_eq!(request.effort.as_deref(), Some(super::DEFAULT_TURN_EFFORT));
		assert!(matches!(
			request.input.as_slice(),
			[super::UserInput::Text { text }] if text == "hello"
		));
	}

	#[test]
	fn remaining_idle_budget_resets_from_latest_activity() {
		let now = Instant::now();
		let timeout = Duration::from_secs(300);
		let last_activity_at = now.checked_sub(Duration::from_secs(12)).expect("instant math");
		let remaining = super::remaining_idle_budget(last_activity_at, now, timeout)
			.expect("budget should remain");

		assert!(remaining <= timeout);
		assert!(remaining >= Duration::from_secs(287));
	}

	#[test]
	fn remaining_idle_budget_expires_after_idle_timeout() {
		let now = Instant::now();
		let timeout = Duration::from_secs(300);
		let last_activity_at = now.checked_sub(Duration::from_secs(301)).expect("instant math");

		assert!(super::remaining_idle_budget(last_activity_at, now, timeout).is_none());
	}

	#[test]
	fn run_recorder_keeps_events_when_marker_write_fails() {
		let temp_dir = TempDir::new().expect("tempdir should create");
		let missing_workspace = PathBuf::from(temp_dir.path()).join("missing-workspace");
		let state_store = StateStore::open_in_memory().expect("state store should open");
		let mut recorder =
			super::RunRecorder::new(&state_store, "run-1", 1, Some(&missing_workspace));

		recorder.mark_activity().expect("marker failures should be non-fatal");
		recorder.record("turn/started", "{\"turn\":\"1\"}").expect("event should record");

		assert_eq!(state_store.event_count("run-1").expect("event count should load"), 1);
	}

	#[test]
	fn completion_validation_uses_dynamic_tool_handler() {
		let error = super::validate_turn_completion(Some(&RejectingCompletionHandler), "finished")
			.expect_err("completion validator should be consulted");

		assert!(error.to_string().contains("terminal finalization missing"));
	}

	#[test]
	fn completion_validation_defaults_to_noop_without_handler() {
		super::validate_turn_completion(None, "finished")
			.expect("missing dynamic handler should not fail completion");
	}

	#[test]
	fn probe_handler_allows_completion_validation() {
		super::validate_turn_completion(Some(&ProbeDynamicToolHandler), "PROBE_OK")
			.expect("probe handler should not override completion validation");
	}
}
