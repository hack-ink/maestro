use std::{
	collections::VecDeque,
	io::{BufRead, BufReader, Write},
	process::{Child, ChildStdin, Command, Stdio},
	sync::mpsc::{self, Receiver, RecvTimeoutError},
	thread,
	time::Duration,
};

use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use color_eyre::eyre::eyre;

use crate::prelude::Result;

pub(crate) struct JsonRpcConnection {
	child: Child,
	stdin: ChildStdin,
	stdout_rx: Receiver<String>,
	pending_messages: VecDeque<WireMessage>,
	next_request_id: i64,
}
impl JsonRpcConnection {
	pub(crate) fn spawn_app_server(listen: &str) -> Result<Self> {
		let mut child = Command::new("codex")
			.args(["app-server", "--listen", listen])
			.stdin(Stdio::piped())
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.spawn()?;
		let stdin =
			child.stdin.take().ok_or_else(|| eyre!("Failed to capture app-server stdin."))?;
		let stdout =
			child.stdout.take().ok_or_else(|| eyre!("Failed to capture app-server stdout."))?;
		let stderr =
			child.stderr.take().ok_or_else(|| eyre!("Failed to capture app-server stderr."))?;
		let (stdout_tx, stdout_rx) = mpsc::channel();

		let _stdout_task = thread::spawn(move || {
			let reader = BufReader::new(stdout);

			for line in reader.lines() {
				match line {
					Ok(line) => {
						let line: String = line;
						if line.trim().is_empty() {
							continue;
						}
						if stdout_tx.send(line).is_err() {
							break;
						}
					},
					Err(error) => {
						tracing::warn!(?error, "Failed to read app-server stdout.");
						break;
					},
				}
			}
		});

		let _stderr_task = thread::spawn(move || {
			let reader = BufReader::new(stderr);

			for line in reader.lines() {
				match line {
					Ok(line) => {
						let line: String = line;
						if line.trim().is_empty() {
							continue;
						}
						tracing::warn!(stderr = %line, "codex app-server stderr");
					},
					Err(error) => {
						tracing::warn!(?error, "Failed to read app-server stderr.");
						break;
					},
				}
			}
		});

		Ok(Self { child, stdin, stdout_rx, pending_messages: VecDeque::new(), next_request_id: 1 })
	}

	pub(crate) fn request<P, T>(&mut self, method: &str, params: &P, timeout: Duration) -> Result<T>
	where
		P: Serialize,
		T: DeserializeOwned,
	{
		let request_id = self.next_request_id;
		let expected_id = Value::from(request_id);
		self.next_request_id += 1;
		self.send_value(&json!({
			"id": request_id,
			"method": method,
			"params": params,
		}))?;

		loop {
			let wire_message = self.read_message(Some(timeout))?;

			match &wire_message.message {
				JsonRpcMessage::Notification(_) => self.pending_messages.push_back(wire_message),
				JsonRpcMessage::Response(response) if response.id == expected_id => {
					return Ok(serde_json::from_value(response.result.clone())?);
				},
				JsonRpcMessage::Error(error) if error.id == expected_id => {
					return Err(eyre!(
						"`{method}` failed with {}: {}",
						error.error.code,
						error.error.message
					));
				},
				JsonRpcMessage::Request(request) => {
					return Err(eyre!(
						"Unexpected inbound JSON-RPC request `{}` while waiting for `{method}`.",
						request.method
					));
				},
				JsonRpcMessage::Response(_) | JsonRpcMessage::Error(_) => {
					return Err(eyre!(
						"Received an unexpected JSON-RPC response while waiting for `{method}`."
					));
				},
			}
		}
	}

	pub(crate) fn notify<P>(&mut self, method: &str, params: Option<&P>) -> Result<()>
	where
		P: Serialize,
	{
		let value = match params {
			Some(params) => json!({
				"method": method,
				"params": params,
			}),
			None => json!({ "method": method }),
		};
		self.send_value(&value)
	}

	pub(crate) fn recv(&mut self, timeout: Option<Duration>) -> Result<WireMessage> {
		if let Some(message) = self.pending_messages.pop_front() {
			return Ok(message);
		}

		self.read_message(timeout)
	}

	pub(crate) fn respond<R>(&mut self, id: &Value, result: &R) -> Result<()>
	where
		R: Serialize,
	{
		self.send_value(&json!({
			"id": id,
			"result": result,
		}))
	}

	pub(crate) fn drain_pending(&mut self) -> Vec<WireMessage> {
		self.pending_messages.drain(..).collect()
	}

	fn send_value(&mut self, value: &Value) -> Result<()> {
		writeln!(self.stdin, "{}", serde_json::to_string(value)?)?;
		self.stdin.flush()?;

		Ok(())
	}

	fn read_message(&mut self, timeout: Option<Duration>) -> Result<WireMessage> {
		let raw = match timeout {
			Some(timeout) => match self.stdout_rx.recv_timeout(timeout) {
				Ok(raw) => raw,
				Err(RecvTimeoutError::Timeout) => {
					return Err(eyre!("Timed out while waiting for app-server output."));
				},
				Err(RecvTimeoutError::Disconnected) => {
					return Err(eyre!("App-server stdout disconnected unexpectedly."));
				},
			},
			None => self
				.stdout_rx
				.recv()
				.map_err(|_| eyre!("App-server stdout disconnected unexpectedly."))?,
		};

		WireMessage::parse(raw)
	}
}
impl Drop for JsonRpcConnection {
	fn drop(&mut self) {
		let _ = self.child.kill();
		let _ = self.child.wait();
	}
}

#[derive(Debug, Clone)]
pub(crate) struct WireMessage {
	pub(crate) raw: String,
	pub(crate) message: JsonRpcMessage,
}
impl WireMessage {
	fn parse(raw: String) -> Result<Self> {
		let value: Value = serde_json::from_str(&raw)?;
		let message = if value.get("method").is_some() && value.get("id").is_some() {
			JsonRpcMessage::Request(serde_json::from_value(value)?)
		} else if value.get("method").is_some() {
			JsonRpcMessage::Notification(serde_json::from_value(value)?)
		} else if value.get("error").is_some() {
			JsonRpcMessage::Error(serde_json::from_value(value)?)
		} else if value.get("result").is_some() {
			JsonRpcMessage::Response(serde_json::from_value(value)?)
		} else {
			return Err(eyre!("Received an unrecognized JSON-RPC payload: {raw}"));
		};

		Ok(Self { raw, message })
	}
}

#[derive(Debug, Clone)]
pub(crate) enum JsonRpcMessage {
	Request(JsonRpcRequest),
	Notification(JsonRpcNotification),
	Response(JsonRpcResponse),
	Error(JsonRpcError),
}

#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct JsonRpcRequest {
	pub(crate) id: Value,
	pub(crate) method: String,
	#[serde(default)]
	pub(crate) params: Value,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct JsonRpcNotification {
	pub(crate) method: String,
	#[serde(default)]
	pub(crate) params: Value,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct JsonRpcResponse {
	pub(crate) id: Value,
	pub(crate) result: Value,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct JsonRpcError {
	pub(crate) id: Value,
	pub(crate) error: JsonRpcErrorPayload,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct JsonRpcErrorPayload {
	pub(crate) code: i64,
	pub(crate) message: String,
}

#[cfg(test)]
mod tests {
	use serde_json::json;

	use crate::agent::json_rpc::{JsonRpcMessage, WireMessage};

	#[test]
	fn parses_notification_messages() {
		let message = WireMessage::parse(
			r#"{"method":"thread/status/changed","params":{"threadId":"thread-1"}}"#.to_owned(),
		)
		.expect("notification should parse");

		match message.message {
			JsonRpcMessage::Notification(notification) => {
				assert_eq!(notification.method, "thread/status/changed");
				assert_eq!(notification.params["threadId"], json!("thread-1"));
			},
			other => panic!("unexpected message: {other:?}"),
		}
	}

	#[test]
	fn parses_response_messages() {
		let message =
			WireMessage::parse(r#"{"id":1,"result":{"userAgent":"maestro-test"}}"#.to_owned())
				.expect("response should parse");

		match message.message {
			JsonRpcMessage::Response(response) => {
				assert_eq!(response.id, json!(1));
				assert_eq!(response.result["userAgent"], json!("maestro-test"));
			},
			other => panic!("unexpected message: {other:?}"),
		}
	}
}
