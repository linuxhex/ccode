
use crate::app::actions::Action;
use crate::app::operating_mode::AgentOperatingMode;
use crate::slash::command::{AppCtx, ArgItem, CommandExecCtx, CommandResult, SlashCommand};

pub struct ModeCommand;

impl SlashCommand for ModeCommand {
    fn name(&self) -> &str {
        "mode"
    }

    fn description(&self) -> &str {
        "View or change operating mode (standard/ptc/minimal/creative)"
    }

    fn usage(&self) -> &str {
        "/mode [standard|ptc|minimal|creative]"
    }

    fn takes_args(&self) -> bool {
        true
    }

    fn args_required(&self) -> bool {
        false
    }

    fn suggest_args(&self, _ctx: &AppCtx, _query: &str) -> Option<Vec<ArgItem>> {
        let items = AgentOperatingMode::ALL
            .iter()
            .map(|m| ArgItem {
                display: format!("{} — {}", m.slug(), m.label()),
                match_text: m.slug().to_string(),
                insert_text: m.slug().to_string(),
                description: mode_description(m),
            })
            .collect();
        Some(items)
    }

    fn run(&self, _ctx: &mut CommandExecCtx, args: &str) -> CommandResult {
        let trimmed = args.trim();
        if trimmed.is_empty() {
            return CommandResult::Message(
                "Current mode: use /mode <name> to switch. \
                 Options: standard, ptc, minimal, creative"
                    .into(),
            );
        }
        match AgentOperatingMode::from_slug(trimmed) {
            Some(mode) => CommandResult::Action(Action::SetOperatingMode(mode)),
            None => CommandResult::Error(format!(
                "Unknown mode: '{trimmed}'. Options: standard, ptc, minimal, creative"
            )),
        }
    }
}

fn mode_description(mode: &AgentOperatingMode) -> String {
    match mode {
        AgentOperatingMode::Standard => "Default balanced operation".into(),
        AgentOperatingMode::PlanThenCode => "Plan first, then implement".into(),
        AgentOperatingMode::Minimal => "Concise, essentials only".into(),
        AgentOperatingMode::Creative => "Explore alternatives, favor innovation".into(),
    }
}

