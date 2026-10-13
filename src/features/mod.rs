//! The features. The whole bot is the list in [`all`].
//!
//! To add a feature: create a folder here with a struct that implements
//! [`crate::core::Feature`], then add one line to [`all`].

mod chat;
mod control_panel;
mod drawing;
mod memory;
mod reminders;
mod snails;
mod voice;
mod wheel;
mod who_said_it;

use std::sync::Arc;

use crate::core::Feature;

pub fn all() -> Vec<Arc<dyn Feature>> {
    vec![
        Arc::new(reminders::Reminders::default()),
        Arc::new(wheel::Wheel),
        Arc::new(control_panel::ControlPanel),
        Arc::new(memory::Memory),
        Arc::new(snails::Snails::default()),
        Arc::new(voice::Voice),
        Arc::new(drawing::Drawing),
        Arc::new(who_said_it::WhoSaidIt),
        Arc::new(chat::Chat::default()),
    ]
}
