//! The ACP conversation policy: pure, with no effects.
//!
//! The agent session is a state machine over the messages that arrive on
//! the pipe: the policy answers what the agent asks and sends what comes
//! next. The transport feeds bytes in and writes bytes out; nothing here
//! touches a socket, a container or a clock, so the failure paths of the
//! conversation are observable offline.

use serde_json::{Value, json};

use opensandbox::acp;

/// The workspace the agent works in.
const PROBE_CWD: &str = "/workspace";

/// The task the probe asks the agent to fulfil.
const PROBE_PROMPT: &str =
    "Create the file /workspace/hodei-probe.txt with the content 'written-by-mock-agent'";

/// How much of the agent's visible text is kept for diagnostics.
const AGENT_TEXT_LIMIT: usize = 4000;

/// The permission option the client grants when the agent asks for one.
const PERMISSION_OPTION_KIND: &str = "allow_once";

/// What the transport has to do after a message from the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpAction {
    /// Write these bytes to the agent's standard input.
    Write(String),
}

impl AcpAction {
    /// The bytes to write to the agent.
    pub fn bytes(&self) -> &[u8] {
        match self {
            AcpAction::Write(text) => text.as_bytes(),
        }
    }
}

/// The conversation with the agent.
#[derive(Debug, Default)]
pub struct AcpDialogue {
    next_id: u64,
    init_id: Option<u64>,
    new_session_sent: bool,
    prompt_sent: bool,
    pending_prompt_id: Option<u64>,
    session_id: Option<String>,
    prompt_answer: Option<Value>,
    protocol_version: Option<Value>,
    updates: usize,
    methods: Vec<String>,
    agent_text: String,
}

impl AcpDialogue {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            ..Default::default()
        }
    }

    /// The handshake that opens the conversation.
    pub fn handshake(&mut self) -> AcpAction {
        let id = self.next_id;
        self.next_id += 1;
        self.init_id = Some(id);
        // No fs capabilities advertised: the agent uses its OWN tools inside
        // the sandbox instead of delegating writes to this client.
        AcpAction::Write(acp::encode_request(
            id,
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {}
            }),
        ))
    }

    /// Feed one message from the agent; the action to take, if any.
    pub fn on_message(&mut self, message: &Value) -> Option<AcpAction> {
        if let Some(method) = message.get("method").and_then(Value::as_str) {
            self.methods.push(method.to_string());
            match method {
                "session/update" => self.on_update(message),
                "session/request_permission" => return Some(self.answer_permission(message)),
                _ => {}
            }
        }
        let id = message.get("id").and_then(Value::as_u64);
        if id.is_none() || message.get("result").is_none() {
            return None;
        }
        if id == self.init_id {
            self.protocol_version = message["result"].get("protocolVersion").cloned();
            self.new_session_sent = true;
            return Some(self.request_session());
        }
        if self.new_session_sent
            && !self.prompt_sent
            && message["result"].get("sessionId").is_some()
        {
            self.session_id = message["result"]["sessionId"].as_str().map(str::to_string);
            return Some(self.request_prompt());
        }
        if self.pending_prompt_id == id {
            self.prompt_answer = Some(message["result"].clone());
        }
        None
    }

    /// The cancellation that ends the conversation with the agent.
    pub fn cancel(&mut self) -> AcpAction {
        let id = self.next_id;
        self.next_id += 1;
        AcpAction::Write(acp::encode_request(
            id,
            "session/cancel",
            json!({"sessionId": self.session_id.clone().unwrap_or_default()}),
        ))
    }

    /// The protocol version the agent answered the handshake with.
    pub fn protocol_version(&self) -> Option<&Value> {
        self.protocol_version.as_ref()
    }

    /// The result of the prompt, once the agent answered it.
    pub fn prompt_answer(&self) -> Option<&Value> {
        self.prompt_answer.as_ref()
    }

    /// How many structured updates the agent sent.
    pub fn updates(&self) -> usize {
        self.updates
    }

    /// The methods the agent used, in order.
    pub fn methods(&self) -> &[String] {
        &self.methods
    }

    /// The visible text of the agent, kept for diagnostics.
    pub fn agent_text(&self) -> &str {
        &self.agent_text
    }

    fn on_update(&mut self, message: &Value) {
        self.updates += 1;
        let chunk = message["params"]["update"]["content"]["text"]
            .as_str()
            .unwrap_or_default();
        if !chunk.is_empty() && self.agent_text.len() < AGENT_TEXT_LIMIT {
            self.agent_text.push_str(chunk);
        }
    }

    /// Answer the agent's permission request: the option that grants the
    /// permission once, or a refusal when the agent offers no such option.
    /// An empty option id is never sent: it is a lie the agent would
    /// refuse.
    fn answer_permission(&mut self, message: &Value) -> AcpAction {
        let grantable = message["params"]["options"]
            .as_array()
            .and_then(|options| {
                options
                    .iter()
                    .find(|option| option["kind"].as_str() == Some(PERMISSION_OPTION_KIND))
            })
            .and_then(|option| option["optionId"].as_str().map(str::to_string));
        let outcome = match grantable {
            Some(option_id) => json!({"outcome": "selected", "optionId": option_id}),
            None => json!({"outcome": "cancelled"}),
        };
        AcpAction::Write(acp::encode_response(
            message.get("id").cloned().unwrap_or(json!(0)),
            json!({"outcome": outcome}),
        ))
    }

    fn request_session(&mut self) -> AcpAction {
        let id = self.next_id;
        self.next_id += 1;
        AcpAction::Write(acp::encode_request(
            id,
            "session/new",
            json!({"cwd": PROBE_CWD, "mcpServers": []}),
        ))
    }

    fn request_prompt(&mut self) -> AcpAction {
        let id = self.next_id;
        self.next_id += 1;
        self.pending_prompt_id = Some(id);
        self.prompt_sent = true;
        AcpAction::Write(acp::encode_request(
            id,
            "session/prompt",
            json!({
                "sessionId": self.session_id.clone().unwrap_or_default(),
                "prompt": [{"type": "text", "text": PROBE_PROMPT}],
            }),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(value: Value) -> Value {
        value
    }

    fn written(action: Option<AcpAction>) -> Value {
        match action {
            Some(AcpAction::Write(text)) => {
                serde_json::from_str(text.trim_end()).expect("the written bytes are JSON")
            }
            None => panic!("no action was produced"),
        }
    }

    #[test]
    fn the_handshake_opens_the_conversation() {
        let mut dialogue = AcpDialogue::new();
        let request = written(Some(dialogue.handshake()));
        assert_eq!(request["method"], "initialize");
        assert_eq!(request["id"], 1);
        assert_eq!(request["params"]["protocolVersion"], 1);
    }

    #[test]
    fn a_session_is_requested_after_the_handshake_is_answered() {
        let mut dialogue = AcpDialogue::new();
        dialogue.handshake();
        let request = written(dialogue.on_message(&message(
            json!({"jsonrpc": "2.0", "id": 1, "result": {"protocolVersion": 1}}),
        )));
        assert_eq!(request["method"], "session/new");
        assert_eq!(request["params"]["cwd"], PROBE_CWD);
    }

    #[test]
    fn the_prompt_is_requested_with_the_session_the_agent_answered() {
        let mut dialogue = AcpDialogue::new();
        dialogue.handshake();
        dialogue.on_message(&message(json!({"id": 1, "result": {"protocolVersion": 1}})));
        let request = written(dialogue.on_message(&message(
            json!({"id": 2, "result": {"sessionId": "sess-42"}}),
        )));
        assert_eq!(request["method"], "session/prompt");
        assert_eq!(request["params"]["sessionId"], "sess-42");
        assert_eq!(request["params"]["prompt"][0]["text"], PROBE_PROMPT);
    }

    #[test]
    fn the_cancellation_carries_the_session_the_agent_answered() {
        let mut dialogue = AcpDialogue::new();
        dialogue.handshake();
        dialogue.on_message(&message(json!({"id": 1, "result": {"protocolVersion": 1}})));
        dialogue.on_message(&message(
            json!({"id": 2, "result": {"sessionId": "sess-42"}}),
        ));
        let request = written(Some(dialogue.cancel()));
        assert_eq!(request["method"], "session/cancel");
        assert_eq!(request["params"]["sessionId"], "sess-42");
    }

    #[test]
    fn the_permission_request_is_answered_with_the_grantable_option() {
        let mut dialogue = AcpDialogue::new();
        let response = written(dialogue.on_message(&message(json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "session/request_permission",
            "params": {"options": [
                {"kind": "allow_always", "optionId": "always-1"},
                {"kind": "allow_once", "optionId": "once-1"}
            ]}
        }))));
        assert_eq!(response["id"], 7);
        assert_eq!(response["result"]["outcome"]["outcome"], "selected");
        assert_eq!(response["result"]["outcome"]["optionId"], "once-1");
    }

    #[test]
    fn a_permission_request_without_a_grantable_option_is_refused_not_half_answered() {
        let mut dialogue = AcpDialogue::new();
        let response = written(dialogue.on_message(&message(json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "session/request_permission",
            "params": {"options": [{"kind": "reject_once", "optionId": "reject-1"}]}
        }))));
        assert_eq!(response["result"]["outcome"]["outcome"], "cancelled");
        assert!(
            response["result"]["outcome"].get("optionId").is_none(),
            "an empty option id is never sent: {response}"
        );
    }

    #[test]
    fn the_updates_of_the_agent_are_counted_and_kept_for_diagnostics() {
        let mut dialogue = AcpDialogue::new();
        dialogue.on_message(&message(json!({
            "method": "session/update",
            "params": {"update": {"content": {"text": "writing the file"}}}
        })));
        dialogue.on_message(&message(json!({
            "method": "session/update",
            "params": {"update": {"content": {"text": "done"}}}
        })));
        assert_eq!(dialogue.updates(), 2);
        assert_eq!(dialogue.agent_text(), "writing the filedone");
        assert_eq!(dialogue.methods(), ["session/update", "session/update"]);
    }

    #[test]
    fn the_prompt_answer_is_kept_once_the_agent_answers_it() {
        let mut dialogue = AcpDialogue::new();
        dialogue.handshake();
        dialogue.on_message(&message(json!({"id": 1, "result": {"protocolVersion": 1}})));
        dialogue.on_message(&message(
            json!({"id": 2, "result": {"sessionId": "sess-42"}}),
        ));
        assert!(dialogue.prompt_answer().is_none());
        dialogue.on_message(&message(
            json!({"id": 3, "result": {"stopReason": "end_turn"}}),
        ));
        assert_eq!(
            dialogue.prompt_answer().and_then(|a| a.get("stopReason")),
            Some(&json!("end_turn"))
        );
    }

    #[test]
    fn an_agent_that_never_answers_drives_no_further_request() {
        let mut dialogue = AcpDialogue::new();
        dialogue.handshake();
        // A notification is not an answer: the client keeps waiting.
        assert!(
            dialogue
                .on_message(&message(json!({"method": "session/update"})))
                .is_none()
        );
        assert!(dialogue.protocol_version().is_none());
        assert!(dialogue.prompt_answer().is_none());
    }
}
