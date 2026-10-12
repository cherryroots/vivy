//! What reminders look like in Discord: message texts, buttons, the delete menu, and the
//! button IDs.

use serenity::all::{
    ButtonStyle, CreateActionRow, CreateAllowedMentions, CreateAttachment, CreateButton,
    CreateInputText, CreateMessage, CreateModal, CreateSelectMenu, CreateSelectMenuKind,
    CreateSelectMenuOption, InputTextStyle, UserId,
};

use super::store::{Reminder, Summary};
use crate::util::{count, shorten};

/// The snooze buttons under a delivered reminder that add a fixed time: (label, minutes).
/// "Tomorrow 9:00" and "Pick a time…" come after them. The labels are short so all four
/// fit one row on a phone.
const SNOOZES: [(&str, i64); 2] = [("10 min", 10), ("1 hour", 60)];

/// Every button, menu and modal this feature makes. The ID text is built and parsed only
/// here.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// The `/reminders` delete menu. The reminder ID is the selected value.
    Delete,
    /// A snooze button on a delivered reminder: `minutes` from now. (Before "Tomorrow" had
    /// its own button, it was this with 1440 minutes; those buttons still work.)
    Snooze { id: i64, minutes: i64 },
    /// "Snooze until tomorrow": 09:00 the next morning in the owner's timezone.
    Tomorrow { id: i64 },
    /// "Pick a time…": opens the modal to type a time.
    AskSnooze { id: i64 },
    /// The snooze modal.
    SnoozeTo { id: i64 },
    /// The Cancel button under the "I'll remind you" reply.
    Cancel { id: i64 },
}

impl Action {
    pub fn custom_id(&self) -> String {
        match self {
            Action::Delete => "reminders:delete".to_string(),
            Action::Snooze { id, minutes } => format!("reminders:snooze:{id}:{minutes}"),
            Action::Tomorrow { id } => format!("reminders:tomorrow:{id}"),
            Action::AskSnooze { id } => format!("reminders:asksnooze:{id}"),
            Action::SnoozeTo { id } => format!("reminders:snoozeto:{id}"),
            Action::Cancel { id } => format!("reminders:cancel:{id}"),
        }
    }

    /// Parses what the dispatcher passes on: the ID without the `reminders:` prefix.
    pub fn parse(action: &str) -> Option<Action> {
        let parts: Vec<&str> = action.split(':').collect();
        match parts.as_slice() {
            ["delete"] => Some(Action::Delete),
            ["snooze", id, minutes] => Some(Action::Snooze {
                id: id.parse().ok()?,
                minutes: minutes.parse().ok()?,
            }),
            ["tomorrow", id] => Some(Action::Tomorrow {
                id: id.parse().ok()?,
            }),
            ["asksnooze", id] => Some(Action::AskSnooze {
                id: id.parse().ok()?,
            }),
            ["snoozeto", id] => Some(Action::SnoozeTo {
                id: id.parse().ok()?,
            }),
            ["cancel", id] => Some(Action::Cancel {
                id: id.parse().ok()?,
            }),
            _ => None,
        }
    }
}

/// The reply after setting a reminder. `repeat` is how a recurring one repeats.
pub fn confirmation(
    fire_at: i64,
    message: &str,
    repeat: Option<&str>,
    zone_hint: bool,
    images: usize,
    skipped: &[String],
) -> String {
    let mut text = match repeat {
        Some(repeat) => format!(
            "I'll remind you {repeat}, starting <t:{fire_at}:R> (<t:{fire_at}:f>): {}",
            shorten(message, 1400)
        ),
        None => format!(
            "I'll remind you <t:{fire_at}:R> (<t:{fire_at}:f>): {}",
            shorten(message, 1500)
        ),
    };
    if images > 0 {
        text.push_str(&format!(" (with {})", count(images, "image")));
    }
    if zone_hint {
        text.push_str("\n-# Times without a timezone are read as UTC. Set yours with `/timezone`.");
    }
    if !skipped.is_empty() {
        text.push_str(&format!(
            "\n-# Couldn't save {}, so the reminder will link to your message instead.",
            skipped.join(", ")
        ));
    }
    text
}

/// The Cancel button under the "I'll remind you" reply. Only the reminder's owner can use it.
pub fn cancel_button(id: i64) -> CreateActionRow {
    let button = CreateButton::new(Action::Cancel { id }.custom_id())
        .label("Cancel")
        .style(ButtonStyle::Secondary);
    CreateActionRow::Buttons(vec![button])
}

/// The delivered reminder: pings only its owner, links to the message that set it, and
/// carries its images and snooze buttons. With `with_images` false the images are left off
/// (used when Discord refused the upload) and counted in the "missing" hint instead.
pub fn fired_message(reminder: &Reminder, now: i64, with_images: bool) -> CreateMessage {
    let set = match reminder.source_message_id {
        Some(message) => {
            let guild = reminder
                .guild_id
                .map_or("@me".to_string(), |id| id.to_string());
            // The <> around the link stops Discord from showing a preview of it.
            format!(
                "[Reminder set](<https://discord.com/channels/{guild}/{}/{message}>)",
                reminder.channel_id
            )
        }
        None => "Reminder set".to_string(),
    };
    let mut content = format!(
        "<@{}> ⏰ {}\n-# {set} <t:{}:f>",
        reminder.user_id,
        shorten(&reminder.message, 1800),
        reminder.created_at
    );
    // Say so when it's late, for example after the bot was offline.
    if now - reminder.fire_at > 5 * 60 {
        content.push_str(&format!(", due <t:{}:R>", reminder.fire_at));
    }
    if let Some(repeat) = &reminder.repeat {
        content.push_str(&format!(" · 🔁 {repeat}"));
    }
    let mut missing = reminder.missing_images as usize;
    if !with_images {
        missing += reminder.images.len();
    }
    match missing {
        0 => {}
        1 => content.push_str(" · [image missing]"),
        n => content.push_str(&format!(" · [{n} images missing]")),
    }

    let id = reminder.id;
    let button = |action: Action, label: &str| {
        CreateButton::new(action.custom_id())
            .label(label)
            .style(ButtonStyle::Secondary)
    };
    let mut buttons: Vec<CreateButton> = SNOOZES
        .iter()
        .map(|&(label, minutes)| button(Action::Snooze { id, minutes }, label))
        .collect();
    buttons.push(button(Action::Tomorrow { id }, "Tomorrow 9:00"));
    buttons.push(button(Action::AskSnooze { id }, "Pick a time…"));

    let mut message = CreateMessage::new()
        .content(content)
        .allowed_mentions(CreateAllowedMentions::new().users([UserId::new(reminder.user_id)]))
        .components(vec![CreateActionRow::Buttons(buttons)]);
    if !with_images {
        return message;
    }
    for image in &reminder.images {
        message = message.add_file(CreateAttachment::bytes(
            image.data.clone(),
            image.filename.clone(),
        ));
    }
    message
}

/// The modal behind "Pick a time…".
pub fn snooze_modal(id: i64) -> CreateModal {
    let input = CreateInputText::new(InputTextStyle::Short, "When?", "when")
        .placeholder("in 30m, at 18:00, tomorrow at 9am, friday")
        .max_length(100)
        .required(true);
    CreateModal::new(Action::SnoozeTo { id }.custom_id(), "Snooze until…")
        .components(vec![CreateActionRow::InputText(input)])
}

/// The delivered reminder's text after a snooze button was pressed.
pub fn snoozed(original: &str, until: i64) -> String {
    with_line(original, &format!("😴 Snoozed until <t:{until}:f>"))
}

/// The "I'll remind you" reply after its Cancel button was pressed.
pub fn cancelled(original: &str) -> String {
    with_line(original, "❌ Cancelled")
}

/// `original` with a small line under it. Discord allows 2000 characters, so the original is
/// cut to leave room for the line.
fn with_line(original: &str, line: &str) -> String {
    let line = format!("\n-# {line}");
    let room = 2000 - line.chars().count();
    format!("{}{line}", shorten(original, room))
}

/// The `/reminders` list, with a menu to delete one. Discord menus hold at most 25 options.
/// `note` goes first, like "✅ Reminder deleted." after a delete.
pub fn list_message(reminders: &[Summary], note: Option<&str>) -> (String, Vec<CreateActionRow>) {
    let mut text = note.map(|note| format!("{note}\n")).unwrap_or_default();
    if reminders.is_empty() {
        text.push_str("You have no pending reminders.");
        return (text, Vec::new());
    }

    let shown = &reminders[..reminders.len().min(25)];
    text.push_str("**Your reminders**\n");
    for (n, reminder) in shown.iter().enumerate() {
        let images = match reminder.image_count {
            0 => String::new(),
            1 => " (1 image)".to_string(),
            n => format!(" ({n} images)"),
        };
        let repeat = match &reminder.repeat {
            Some(repeat) => format!(" 🔁 {repeat}"),
            None => String::new(),
        };
        text.push_str(&format!(
            "{}. <t:{}:R>{repeat}: {}{images}\n",
            n + 1,
            reminder.fire_at,
            shorten(&reminder.message, 60)
        ));
    }
    if reminders.len() > shown.len() {
        text.push_str(&format!("-# Showing the first 25 of {}.", reminders.len()));
    }

    let options = shown
        .iter()
        .enumerate()
        .map(|(n, reminder)| {
            let label = shorten(&format!("{}. {}", n + 1, reminder.message), 100);
            CreateSelectMenuOption::new(label, reminder.id.to_string())
        })
        .collect();
    let menu = CreateSelectMenu::new(
        Action::Delete.custom_id(),
        CreateSelectMenuKind::String { options },
    )
    .placeholder("Delete a reminder…");
    (text, vec![CreateActionRow::SelectMenu(menu)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::reminders::store::Image;

    fn reminder() -> Reminder {
        Reminder {
            id: 1,
            user_id: 2,
            channel_id: 3,
            guild_id: Some(4),
            message: "check the oven".into(),
            fire_at: 1000,
            created_at: 500,
            attempts: 0,
            source_message_id: Some(5),
            missing_images: 0,
            images: vec![Image {
                filename: "a.png".into(),
                data: vec![1],
            }],
            repeat: None,
        }
    }

    /// The text the message would send.
    fn content(message: &CreateMessage) -> String {
        let json = serde_json::to_value(message).unwrap();
        json["content"].as_str().unwrap().to_string()
    }

    #[test]
    fn fired_message_links_to_the_source() {
        let text = content(&fired_message(&reminder(), 1000, true));
        assert_eq!(
            text,
            "<@2> ⏰ check the oven\n-# [Reminder set](<https://discord.com/channels/4/3/5>) <t:500:f>"
        );

        let mut dm = reminder();
        dm.guild_id = None;
        assert!(content(&fired_message(&dm, 1000, true)).contains("/channels/@me/3/5"));

        let mut imported = reminder();
        imported.source_message_id = None;
        assert!(
            content(&fired_message(&imported, 1000, true)).contains("-# Reminder set <t:500:f>")
        );
    }

    #[test]
    fn fired_message_counts_missing_images() {
        let mut r = reminder();
        assert!(!content(&fired_message(&r, 1000, true)).contains("missing"));
        assert!(content(&fired_message(&r, 1000, false)).ends_with(" · [image missing]"));
        r.missing_images = 2;
        assert!(content(&fired_message(&r, 1000, true)).ends_with(" · [2 images missing]"));
        assert!(content(&fired_message(&r, 1000, false)).ends_with(" · [3 images missing]"));
    }

    #[test]
    fn snoozed_fits_in_a_message() {
        assert_eq!(snoozed("hi", 5), "hi\n-# 😴 Snoozed until <t:5:f>");
        let long = "a".repeat(2000);
        let text = snoozed(&long, 1_800_000_000);
        assert_eq!(text.chars().count(), 2000);
        assert!(text.ends_with("<t:1800000000:f>"));
    }

    #[test]
    fn recurring_reminders_say_so() {
        let mut r = reminder();
        r.repeat = Some("every monday at 9:00 UTC".into());
        assert!(
            content(&fired_message(&r, 1000, true)).ends_with(" · 🔁 every monday at 9:00 UTC")
        );
        let list = [Summary {
            id: 1,
            message: "trash".into(),
            fire_at: 1000,
            image_count: 0,
            repeat: r.repeat.clone(),
        }];
        let (text, rows) = list_message(&list, Some("✅ Reminder deleted."));
        assert_eq!(
            text,
            "✅ Reminder deleted.\n**Your reminders**\n1. <t:1000:R> 🔁 every monday at 9:00 UTC: trash\n"
        );
        assert_eq!(rows.len(), 1);
        let (text, rows) = list_message(&[], Some("✅ Reminder deleted."));
        assert_eq!(text, "✅ Reminder deleted.\nYou have no pending reminders.");
        assert!(rows.is_empty());
    }

    #[test]
    fn snooze_buttons() {
        let json = serde_json::to_value(fired_message(&reminder(), 1000, true)).unwrap();
        let labels: Vec<&str> = json["components"][0]["components"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["label"].as_str().unwrap())
            .collect();
        assert_eq!(
            labels,
            ["10 min", "1 hour", "Tomorrow 9:00", "Pick a time…"]
        );
    }

    #[test]
    fn action_round_trip() {
        for action in [
            Action::Delete,
            Action::Snooze {
                id: 12,
                minutes: 60,
            },
            Action::Tomorrow { id: 12 },
            Action::AskSnooze { id: 12 },
            Action::SnoozeTo { id: 12 },
            Action::Cancel { id: 12 },
        ] {
            let id = action.custom_id();
            let rest = id.strip_prefix("reminders:").unwrap();
            assert_eq!(Action::parse(rest), Some(action));
        }
        assert_eq!(Action::parse("snooze:x:10"), None);
        assert_eq!(Action::parse("explode"), None);
    }
}
