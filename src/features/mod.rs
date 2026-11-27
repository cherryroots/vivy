//! The features. The whole bot is the list in [`all`].
//!
//! To add a feature: create a folder here with a struct that implements
//! [`crate::core::Feature`], then add one line to [`all`].

mod chat;
mod control_panel;
mod drawing;
mod games;
mod memory;
mod reminders;
mod snails;
mod tinyfish;
mod voice;
mod wheel;

use std::sync::Arc;

use crate::core::Feature;

pub fn all() -> Vec<Arc<dyn Feature>> {
    vec![
        Arc::new(reminders::Reminders::default()),
        Arc::new(wheel::Wheel),
        Arc::new(control_panel::ControlPanel),
        Arc::new(memory::Memory),
        Arc::new(snails::Snails::default()),
        Arc::new(voice::Voice::default()),
        Arc::new(drawing::Drawing::default()),
        Arc::new(games::Games),
        Arc::new(tinyfish::TinyFish),
        Arc::new(chat::Chat::default()),
    ]
}

/// Deserializes every feature's `[features.<name>]` settings so unknown keys are recorded
/// on `config`. Used by the whole-repo config smoke test in `main.rs`.
#[cfg(test)]
pub fn check_all_settings(config: &crate::core::config::Config) -> anyhow::Result<()> {
    #[derive(Default, serde::Deserialize)]
    struct GateOnly {}

    let checked = [
        "chat",
        "control_panel",
        "drawing",
        "games",
        "memory",
        "reminders",
        "snails",
        "tinyfish",
        "voice",
        "wheel",
    ];
    let mut actual: Vec<&str> = all().iter().map(|f| f.name()).collect();
    actual.sort();
    assert_eq!(
        actual, checked,
        "update features::check_all_settings when adding or removing a feature"
    );

    config.feature::<chat::chime::Settings>("chat")?;
    config.feature::<control_panel::Settings>("control_panel")?;
    config.feature::<drawing::Settings>("drawing")?;
    config.feature::<games::Settings>("games")?;
    memory::check_settings(config)?;
    config.feature::<GateOnly>("reminders")?;
    config.feature::<snails::Settings>("snails")?;
    config.feature::<tinyfish::Settings>("tinyfish")?;
    config.feature::<voice::Settings>("voice")?;
    config.feature::<GateOnly>("wheel")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Discord refuses the whole list when one command breaks its rules (a description too
    /// long, an option name with a capital), so every command is checked here.
    #[test]
    fn every_command_fits_discords_rules() {
        let mut names = std::collections::HashSet::new();
        for feature in all() {
            for command in feature.commands() {
                let definition = &command.definition;
                if let Err(err) = twilight_validate::command::command(definition) {
                    panic!("/{} doesn't fit: {err}", definition.name);
                }
                assert!(
                    names.insert(definition.name.clone()),
                    "two commands are called {}",
                    definition.name
                );
                let wants_suggestions =
                    format!("{:?}", definition.options).contains("autocomplete: Some(true)");
                assert_eq!(
                    wants_suggestions,
                    command.autocomplete.is_some(),
                    "/{} has autocomplete options but no suggester, or the other way round",
                    definition.name
                );
            }
        }
    }
}
