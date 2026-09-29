
use crate::app::operating_mode::AgentOperatingMode;
use crate::trajectory::{TrajectoryEvent, TrajectoryEventKind, TrajectoryLog};

// ---------------------------------------------------------------------------
// Task Complexity Analyzer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskComplexity {
    Trivial,
    Moderate,
    Complex,
    Exploratory,
}

pub fn analyze_complexity(prompt: &str) -> TaskComplexity {
    let lower = prompt.to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();

    // Exploratory signals
    let exploratory_signals = [
        "explore", "what if", "alternatives", "options", "design",
        "brainstorm", "approach", "strategy", "architecture", "how should",
        "what's the best", "compare", "trade-offs", "pros and cons",
    ];
    for signal in &exploratory_signals {
        if lower.contains(signal) {
            return TaskComplexity::Exploratory;
        }
    }

    // Complex signals
    let complex_signals = [
        "refactor", "migrate", "redesign", "rewrite", "restructure",
        "across all", "every file", "throughout the", "systematic",
        "multi-file", "breaking change", "api change",
    ];
    for signal in &complex_signals {
        if lower.contains(signal) {
            return TaskComplexity::Complex;
        }
    }

    // Trivial signals
    if words.len() <= 5 || prompt.contains('?') && words.len() <= 15 {
        return TaskComplexity::Trivial;
    }

    // Multiple file paths suggest moderate+ complexity
    let path_count = prompt.matches('/').count() + prompt.matches('\\').count();
    if path_count >= 3 {
        return TaskComplexity::Complex;
    }

    TaskComplexity::Moderate
}

pub fn complexity_to_mode(complexity: TaskComplexity) -> AgentOperatingMode {
    match complexity {
        TaskComplexity::Trivial => AgentOperatingMode::Minimal,
        TaskComplexity::Moderate => AgentOperatingMode::Standard,
        TaskComplexity::Complex => AgentOperatingMode::PlanThenCode,
        TaskComplexity::Exploratory => AgentOperatingMode::Creative,
    }
}

pub fn recommend_mode(
    prompt: &str,
    current_mode: AgentOperatingMode,
) -> Option<AgentOperatingMode> {
    let complexity = analyze_complexity(prompt);
    let recommended = complexity_to_mode(complexity);
    if recommended == current_mode {
        None
    } else {
        Some(recommended)
    }
}

// ---------------------------------------------------------------------------
// Self-Reflection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ReflectionResult {
    pub appears_successful: bool,
    pub confidence: f32,
    pub suggested_follow_up: Option<String>,
    pub reasoning: String,
}

pub fn reflect_on_tool(
    tool_name: &str,
    input_preview: &str,
    output_preview: &str,
    _elapsed_ms: u64,
    success: bool,
) -> ReflectionResult {
    // Fast path: explicit failure
    if !success {
        return ReflectionResult {
            appears_successful: false,
            confidence: 1.0,
            suggested_follow_up: Some(format!("Retry or try alternative approach for {tool_name}")),
            reasoning: "Tool reported explicit failure".into(),
        };
    }

    // Check for common error patterns in output
    let error_patterns = [
        "error", "failed", "not found", "permission denied",
        "no such file", "undefined", "null", "exception",
        "timeout", "refused", "denied",
    ];
    let output_lower = output_preview.to_lowercase();
    let has_error_signals = error_patterns.iter().any(|p| output_lower.contains(p));

    // Check for empty/minimal output (might indicate a problem)
    let output_is_thin = output_preview.trim().len() < 10;

    // File read tools: check if output looks like file content
    if tool_name == "Read" || tool_name == "read" {
        if output_is_thin && !input_preview.contains("empty") {
            return ReflectionResult {
                appears_successful: false,
                confidence: 0.7,
                suggested_follow_up: Some("File may be empty or unreadable — verify path".into()),
                reasoning: "Read returned minimal output".into(),
            };
        }
        return ReflectionResult {
            appears_successful: true,
            confidence: 0.9,
            suggested_follow_up: None,
            reasoning: "File read completed".into(),
        };
    }

    // Execute tools: check exit code patterns
    if tool_name == "Execute" || tool_name == "Bash" {
        if output_lower.contains("exit code: 0") || output_lower.contains("exit status: 0") {
            return ReflectionResult {
                appears_successful: true,
                confidence: 0.95,
                suggested_follow_up: None,
                reasoning: "Command exited successfully".into(),
            };
        }
        if has_error_signals {
            return ReflectionResult {
                appears_successful: false,
                confidence: 0.6,
                suggested_follow_up: Some("Command may have failed — check output".into()),
                reasoning: "Error signals detected in command output".into(),
            };
        }
    }

    // Edit tools: verify diff was applied
    if tool_name == "Edit" || tool_name == "edit" {
        if output_lower.contains("applied") || output_lower.contains("+") && output_lower.contains("-") {
            return ReflectionResult {
                appears_successful: true,
                confidence: 0.85,
                suggested_follow_up: Some("Consider verifying the change compiles/passes tests".into()),
                reasoning: "Edit appears to have been applied".into(),
            };
        }
    }

    // Default: assume success if no error signals
    if has_error_signals {
        ReflectionResult {
            appears_successful: false,
            confidence: 0.5,
            suggested_follow_up: Some(format!("Verify {tool_name} output for errors")),
            reasoning: "Potential error signals in output".into(),
        }
    } else {
        ReflectionResult {
            appears_successful: true,
            confidence: 0.7,
            suggested_follow_up: None,
            reasoning: format!("{tool_name} completed without error signals"),
        }
    }
}

// ---------------------------------------------------------------------------
// Proactive Suggestion Engine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ProactiveSuggestion {
    pub action: String,
    pub reason: String,
    pub confidence: f32,
}

pub fn generate_suggestions(log: &TrajectoryLog) -> Vec<ProactiveSuggestion> {
    let mut suggestions = Vec::new();
    let events = log.snapshot();

    if events.is_empty() {
        return suggestions;
    }

    // Collect recent tool activity for pattern analysis.
    let recent_tools: Vec<&TrajectoryEvent> = events
        .iter()
        .filter(|e| matches!(&e.event, TrajectoryEventKind::ToolStart { .. }))
        .collect();

    let tool_failures: Vec<&TrajectoryEvent> = events
        .iter()
        .filter(|e| matches!(&e.event, TrajectoryEventKind::ToolEnd { success: false, .. }))
        .collect();

    let edit_count = recent_tools
        .iter()
        .filter(|e| {
            if let TrajectoryEventKind::ToolStart { tool, .. } = &e.event {
                let t = tool.to_lowercase();
                t.contains("edit") || t.contains("write") || t.contains("patch")
            } else {
                false
            }
        })
        .count();

    let test_count = recent_tools
        .iter()
        .filter(|e| {
            if let TrajectoryEventKind::ToolStart { tool, .. } = &e.event {
                let t = tool.to_lowercase();
                t.contains("test") || t.contains("cargo")
            } else {
                false
            }
        })
        .count();

    let exec_count = recent_tools
        .iter()
        .filter(|e| {
            if let TrajectoryEventKind::ToolStart { tool, .. } = &e.event {
                let t = tool.to_lowercase();
                t.contains("exec") || t.contains("bash") || t.contains("run") || t.contains("shell")
            } else {
                false
            }
        })
        .count();

    // Pattern 1: Edits made but no tests run → suggest testing
    if edit_count > 0 && test_count == 0 {
        suggestions.push(ProactiveSuggestion {
            action: "Run tests to verify changes".into(),
            reason: format!("{edit_count} edits made, no tests run yet"),
            confidence: 0.85,
        });
    }

    // Pattern 2: Repeated tool failures → suggest alternative
    if tool_failures.len() >= 2 {
        let last_tool = tool_failures.last().and_then(|e| {
            if let TrajectoryEventKind::ToolEnd { tool, .. } = &e.event {
                Some(tool.clone())
            } else {
                None
            }
        });
        if let Some(tool) = last_tool {
            suggestions.push(ProactiveSuggestion {
                action: format!("Try a different approach for {tool}"),
                reason: format!("{tool} failed {} times", tool_failures.len()),
                confidence: 0.8,
            });
        }
    }

    // Pattern 3: Many edits → suggest reviewing
    if edit_count >= 3 {
        suggestions.push(ProactiveSuggestion {
            action: "Review all changes made".into(),
            reason: format!("{edit_count} files edited in this session"),
            confidence: 0.7,
        });
    }

    // Pattern 4: Executions without edits — might be exploring
    if exec_count >= 3 && edit_count == 0 {
        suggestions.push(ProactiveSuggestion {
            action: "Summarize what you found".into(),
            reason: format!("{exec_count} commands run, no changes made yet"),
            confidence: 0.6,
        });
    }

    // Pattern 5: Auto-compact events — context getting heavy
    let compact_events: Vec<&TrajectoryEvent> = events
        .iter()
        .filter(|e| matches!(&e.event, TrajectoryEventKind::AutoCompact { .. }))
        .collect();

    if compact_events.len() >= 2 {
        suggestions.push(ProactiveSuggestion {
            action: "Start a fresh session".into(),
            reason: format!("{} auto-compacts — context is getting large", compact_events.len()),
            confidence: 0.5,
        });
    }

    // Pattern 6: Errors encountered
    let errors: Vec<&TrajectoryEvent> = events
        .iter()
        .filter(|e| matches!(&e.event, TrajectoryEventKind::Error { .. }))
        .collect();

    if errors.len() >= 2 {
        suggestions.push(ProactiveSuggestion {
            action: "Diagnose recent errors".into(),
            reason: format!("{} errors in this session", errors.len()),
            confidence: 0.65,
        });
    }

    // Pattern 7: Many tool calls with little user input → check in
    let user_msg_count = events
        .iter()
        .filter(|e| matches!(&e.event, TrajectoryEventKind::UserMessage { .. }))
        .count();

    if recent_tools.len() > 8 && user_msg_count <= 1 {
        suggestions.push(ProactiveSuggestion {
            action: "Explain what was done so far".into(),
            reason: format!("{} tool calls with minimal user interaction", recent_tools.len()),
            confidence: 0.6,
        });
    }

    // Sort by confidence descending, take top 4.
    suggestions.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal));
    suggestions.truncate(4);
    suggestions
}
