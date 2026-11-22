//! Starters: one stored interaction per system prompt and tool list, that every new
//! conversation continues from, so Gemini's cache can reuse their tokens.
//!
//! Gemini only caches by itself (the Interactions API has no cache key or explicit cache),
//! and on Gemini 4 it caches a stored *previous request*: a follow-up with
//! `previous_interaction_id` reads everything but the last 2,050 tokens of it from the
//! cache. A brand-new conversation has no previous request, so it almost never hit the
//! cache (about 1 time in 15 in tests on 2026-11-21), although the system prompt and
//! tools (about 7,300 tokens) are the same every time.
//!
//! So the first request of a conversation continues a starter instead: an interaction
//! made once with the same system prompt and tools and a short fixed message. In tests,
//! 36 of 40 new questions then got about 5,300 tokens from the cache. The first one after
//! a new starter always misses (the cache is written a few seconds after a request uses
//! it), and the cache is gone after about 5 quiet minutes, so this helps busy stretches.
//!
//! Starters are kept in memory, by a hash of the model, system prompt and tools; a restart
//! or a changed prompt makes a new one (one small request). Gemini keeps them 55 days.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;

use serde_json::{Value, json};

/// The message a starter holds. The model reads it (and its "ok") at the start of every
/// conversation, so it says what it is.
pub const STARTER_MESSAGE: &str =
    "(This only opens the conversation; the real messages come next. Reply with just: ok)";

/// The starter of each system prompt and tool list, by [`key`].
#[derive(Default)]
pub struct Starters {
    pub(super) ids: Mutex<HashMap<u64, String>>,
}

impl Starters {
    pub fn get(&self, key: u64) -> Option<String> {
        self.ids.lock().unwrap().get(&key).cloned()
    }

    pub fn insert(&self, key: u64, id: String) {
        self.ids.lock().unwrap().insert(key, id);
    }

    /// Forgets a starter Gemini no longer has, so the next request makes a new one.
    pub fn forget(&self, key: u64) {
        self.ids.lock().unwrap().remove(&key);
    }
}

/// What a request's starter depends on: its model, system prompt and tools. Requests
/// that share these share a starter.
pub fn key(body: &Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    body["model"].to_string().hash(&mut hasher);
    body["system_instruction"].to_string().hash(&mut hasher);
    body["tools"].to_string().hash(&mut hasher);
    hasher.finish()
}

/// The request that makes a starter: the same model, system prompt and tools as `body`,
/// with [`STARTER_MESSAGE`], stored, not streamed, thinking little and calling no tools.
pub fn body(body: &Value) -> Value {
    json!({
        "model": body["model"],
        "system_instruction": body["system_instruction"],
        "tools": body["tools"],
        "input": STARTER_MESSAGE,
        "generation_config": {"thinking_level": "low", "tool_choice": "none"},
        "store": true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starters_follow_the_prompt_and_tools() {
        let chat = json!({"model": "m", "system_instruction": "Be nice.", "tools": [{"type": "google_search"}],
            "input": [{"type": "user_input"}], "stream": true});
        // The input and the rest don't matter; the model, prompt and tools do.
        let other_question = json!({"model": "m", "system_instruction": "Be nice.", "tools": [{"type": "google_search"}],
            "input": "hi"});
        assert_eq!(key(&chat), key(&other_question));
        let fewer_tools = json!({"model": "m", "system_instruction": "Be nice.", "tools": []});
        assert_ne!(key(&chat), key(&fewer_tools));

        let starter = body(&chat);
        assert_eq!(starter["input"], STARTER_MESSAGE);
        assert_eq!(starter["tools"], chat["tools"]);
        assert_eq!(starter["store"], true);
        assert!(starter.get("stream").is_none());
        assert_eq!(starter["generation_config"]["tool_choice"], "none");
    }
}
