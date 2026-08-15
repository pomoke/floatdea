use serde::{Deserialize, Serialize};

macro_rules! stable_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new() -> Self {
                let ulid = ulid::Ulid::new().to_string().to_ascii_lowercase();
                Self(ulid[6..].to_owned())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

stable_id!(EntityId);
stable_id!(ContainerId);
stable_id!(ReferenceId);
stable_id!(TextId);
stable_id! {
    /// Identity of an AI conversation inside an AI workspace. Conversations are
    /// sidecar state (not Markdown entities): the ID only appears in the AI
    /// box's member list (as a `Conversation` card) and in the AI sidecar store.
    ConversationId
}
stable_id! {
    /// Identity of a single AI turn task (one request—streamed-response round).
    TurnTaskId
}

impl EntityId {
    /// Rebuilds an id from its string form (e.g. parsed out of a
    /// `{title}--{id}.md` filename in a markdown link).
    pub fn from_string(value: &str) -> Self {
        Self(value.to_owned())
    }
}

/// A Markdown note with an identity that does not change when its title changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snippet {
    pub id: EntityId,
    pub title: String,
    pub content: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_short_lowercase_alphanumeric_strings() {
        for id in [
            EntityId::new().0,
            ContainerId::new().0,
            ReferenceId::new().0,
            TextId::new().0,
        ] {
            assert_eq!(id.len(), 20);
            assert!(
                id.bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            );
        }
    }
}
