
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryEvent {
    pub seq: u64,
    pub timestamp: String,
    #[serde(skip)]
    pub elapsed: Option<Duration>,
    pub event: TrajectoryEventKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum TrajectoryEventKind {
    UserMessage {
        content_preview: String,
    },
    ModelResponseStart {
        model: String,
    },
    ModelResponseEnd {
        input_tokens: u64,
        output_tokens: u64,
    },
    ToolStart {
        tool: String,
        input_preview: String,
    },
    ToolEnd {
        tool: String,
        success: bool,
        elapsed_ms: u64,
        output_preview: String,
    },
    StateTransition {
        from: String,
        to: String,
        reason: Option<String>,
    },
    AutoCompact {
        usage_pct: u8,
        tokens_before: u64,
        tokens_after: u64,
    },
    Error {
        kind: String,
        message: String,
        recoverable: bool,
    },
    TurnComplete {
        total_turns: u64,
        total_tokens: u64,
    },
}

pub struct TrajectoryLog {
    events: std::sync::Mutex<Vec<TrajectoryEvent>>,
    started_at: Instant,
    session_id: String,
}

impl TrajectoryLog {
    pub fn new(session_id: String) -> Self {
        Self {
            events: std::sync::Mutex::new(Vec::new()),
            started_at: Instant::now(),
            session_id,
        }
    }

    pub fn append(&self, event: TrajectoryEventKind) -> u64 {
        let mut events = self.events.lock().unwrap();
        let seq = events.len() as u64;
        let elapsed = self.started_at.elapsed();
        let timestamp = chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        events.push(TrajectoryEvent {
            seq,
            timestamp,
            elapsed: Some(elapsed),
            event,
        });
        seq
    }

    pub fn len(&self) -> usize {
        self.events.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn snapshot(&self) -> Vec<TrajectoryEvent> {
        self.events.lock().unwrap().clone()
    }

    pub fn query<F>(&self, predicate: F) -> Vec<TrajectoryEvent>
    where
        F: Fn(&TrajectoryEventKind) -> bool,
    {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| predicate(&e.event))
            .cloned()
            .collect()
    }

    pub fn to_json(&self) -> String {
        let events = self.events.lock().unwrap();
        let wrapper = serde_json::json!({
            "session_id": self.session_id,
            "event_count": events.len(),
            "events": *events,
        });
        serde_json::to_string_pretty(&wrapper).unwrap_or_default()
    }

    pub fn replay<F>(&self, mut callback: F)
    where
        F: FnMut(&TrajectoryEvent),
    {
        let events = self.events.lock().unwrap();
        for event in events.iter() {
            callback(event);
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

pub fn record_tool_start(log: &TrajectoryLog, tool: &str, input_preview: &str) -> u64 {
    log.append(TrajectoryEventKind::ToolStart {
        tool: tool.to_string(),
        input_preview: input_preview.to_string(),
    })
}

pub fn record_tool_end(
    log: &TrajectoryLog,
    tool: &str,
    success: bool,
    elapsed_ms: u64,
    output_preview: &str,
) -> u64 {
    log.append(TrajectoryEventKind::ToolEnd {
        tool: tool.to_string(),
        success,
        elapsed_ms,
        output_preview: output_preview.to_string(),
    })
}

pub fn record_user_message(log: &TrajectoryLog, content: &str) -> u64 {
    let preview = if content.len() > 200 {
        format!("{}...", &content[..200])
    } else {
        content.to_string()
    };
    log.append(TrajectoryEventKind::UserMessage {
        content_preview: preview,
    })
}
