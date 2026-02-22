use async_trait::async_trait;
use serenity::all::{CommandInteraction, Context, CreateCommand, CreateCommandOption};

use crate::i18n::I18n;

pub mod abort;
pub mod agent;
pub mod clear;
pub mod compact;
pub mod config;
pub mod cron;
pub mod language;
pub mod mention_only;
pub mod model;
pub mod skill;
pub mod thinking;

#[async_trait]
pub trait SlashCommand: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self, i18n: &I18n) -> String;
    fn options(&self, _i18n: &I18n) -> Vec<CreateCommandOption> {
        vec![]
    }

    fn create_command(&self, i18n: &I18n) -> CreateCommand {
        let mut cmd = CreateCommand::new(self.name()).description(self.description(i18n));
        for opt in self.options(i18n) {
            cmd = cmd.add_option(opt);
        }
        cmd
    }

    async fn execute(
        &self,
        ctx: &Context,
        command: &CommandInteraction,
        state: &crate::AppState,
    ) -> anyhow::Result<()>;
}

pub fn get_all_commands() -> Vec<Box<dyn SlashCommand>> {
    vec![
        Box::new(agent::AgentCommand),
        Box::new(model::ModelCommand),
        Box::new(thinking::ThinkingCommand),
        Box::new(compact::CompactCommand),
        Box::new(config::ConfigCommand),
        Box::new(clear::ClearCommand),
        Box::new(abort::AbortCommand),
        Box::new(skill::SkillCommand),
        Box::new(mention_only::MentionOnlyCommand),
        Box::new(language::LanguageCommand),
        Box::new(cron::CronCommand),
        Box::new(cron::CronListCommand),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_commands_have_name_desc_and_buildable_options() {
        let i18n = crate::i18n::I18n::new("en");
        for cmd in get_all_commands() {
            let name = cmd.name();
            assert!(!name.trim().is_empty());
            let desc = cmd.description(&i18n);
            assert!(!desc.trim().is_empty());
            let _opts = cmd.options(&i18n);
            let _create = cmd.create_command(&i18n);
        }
    }
}
