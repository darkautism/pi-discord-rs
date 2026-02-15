use async_trait::async_trait;
use serenity::all::{
    CommandInteraction, Context, CreateCommand,
    CreateCommandOption,
};
use std::sync::Arc;

use crate::agent::AiAgent;

pub mod abort;
pub mod agent;
pub mod clear;
pub mod compact;
pub mod mention_only;
pub mod model;
pub mod skill;
pub mod thinking;

#[async_trait]
pub trait SlashCommand: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn options(&self) -> Vec<CreateCommandOption> {
        vec![]
    }

    fn create_command(&self) -> CreateCommand {
        let mut cmd = CreateCommand::new(self.name()).description(self.description());
        for opt in self.options() {
            cmd = cmd.add_option(opt);
        }
        cmd
    }

    async fn execute(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
        agent: Arc<dyn AiAgent>,
    ) -> anyhow::Result<()>;
}

pub fn get_all_commands() -> Vec<Box<dyn SlashCommand>> {
    vec![
        Box::new(agent::AgentCommand),
        Box::new(model::ModelCommand),
        Box::new(thinking::ThinkingCommand),
        Box::new(compact::CompactCommand),
        Box::new(clear::ClearCommand),
        Box::new(abort::AbortCommand),
        Box::new(skill::SkillCommand),
        Box::new(mention_only::MentionOnlyCommand),
    ]
}
