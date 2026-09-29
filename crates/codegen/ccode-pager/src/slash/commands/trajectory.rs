
use crate::app::actions::Action;
use crate::slash::command::{CommandExecCtx, CommandResult, SlashCommand};

pub struct TrajectoryCommand;

impl SlashCommand for TrajectoryCommand {
    fn name(&self) -> &str {
        "trajectory"
    }

    fn description(&self) -> &str {
        "Export trajectory log as JSON"
    }

    fn usage(&self) -> &str {
        "/trajectory"
    }

    fn run(&self, _ctx: &mut CommandExecCtx, _args: &str) -> CommandResult {
        CommandResult::Action(Action::ExportTrajectory)
    }
}
