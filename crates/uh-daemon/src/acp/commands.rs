//! The slash commands the session takes, advertised to the client as the
//! ACP `available_commands_update` once the session is admitted: the
//! session commands (`/compact`, `/goal`, `/autonomous`), the prompt
//! templates, and the skills as `/skill:<name>` while `enableSkillCommands`
//! is on, each with where it comes from (`builtin`, `user`, ...). Prompt admission expands a skill first, then a template, then
//! runs a session command (`prompt_with_images`), so a template named like
//! a command is what that name runs: it is listed once, as the template.

use serde_json::json;
use uh_core::session_engine::engine::SessionEngine;
use uh_types::slash_commands::{SlashCommandExecution, SlashCommandRegistry};

use super::meta::UNHARNESSED_META_NAMESPACE;
use super::types::{AvailableCommand, AvailableCommandInput};

/// The commands, the templates, then the skills; each group by name, the
/// commands in the builtin table's order.
pub fn available_commands(engine: &SessionEngine) -> Vec<AvailableCommand> {
    let mut templates: Vec<_> = engine.prompt_templates.iter().collect();
    templates.sort_by(|a, b| a.name.cmp(&b.name));
    let commands = SlashCommandRegistry::builtin_cached()
        .all()
        .iter()
        .filter(|command| command.execution == SlashCommandExecution::Session)
        .filter(|command| {
            !templates
                .iter()
                .any(|template| template.name == command.name)
        })
        .map(|command| {
            entry(
                command.name,
                command.description,
                command.argument_hint,
                "command",
            )
        });
    let prompts = templates.iter().map(|template| {
        entry(
            &template.name,
            &template.description,
            template.argument_hint.as_deref(),
            "prompt",
        )
    });
    let mut skills: Vec<_> = if engine.skill_commands {
        engine.skills.iter().collect()
    } else {
        Vec::new()
    };
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    let skills = skills.into_iter().map(|skill| {
        let mut command = entry(
            &format!("skill:{}", skill.name),
            &skill.description,
            None,
            "skill",
        );
        command.meta[UNHARNESSED_META_NAMESPACE]["source"] = json!(super::skills::source(skill));
        command
    });
    commands.chain(prompts).chain(skills).collect()
}

fn entry(name: &str, description: &str, hint: Option<&str>, kind: &str) -> AvailableCommand {
    AvailableCommand {
        name: name.to_string(),
        description: description.to_string(),
        input: hint
            .filter(|hint| !hint.is_empty())
            .map(|hint| AvailableCommandInput {
                hint: hint.to_string(),
            }),
        meta: json!({ UNHARNESSED_META_NAMESPACE: { "kind": kind } }),
    }
}
