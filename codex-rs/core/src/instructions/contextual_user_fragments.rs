//! Contextual-user model-visible fragments used by initial-context assembly.
//!
//! These fragments represent injected user-role context (for example AGENTS.md,
//! skills, and plugin guidance) and include turn-context extraction/diffing for
//! AGENTS.md instructions.

use serde::Deserialize;
use serde::Serialize;

use crate::codex::TurnContext;
use crate::model_visible_context::ContextualUserContextRole;
use crate::model_visible_context::ContextualUserFragmentDetector;
use crate::model_visible_context::ContextualUserFragmentMarkers;
use crate::model_visible_context::ModelVisibleContextFragment;
use crate::model_visible_context::PLUGINS_CLOSE_TAG;
use crate::model_visible_context::PLUGINS_OPEN_TAG;
use crate::model_visible_context::SKILL_CLOSE_TAG;
use crate::model_visible_context::SKILL_OPEN_TAG;
use crate::model_visible_context::TaggedContextualUserFragment;
use crate::model_visible_context::TurnContextDiffFragment;
use crate::model_visible_context::TurnContextDiffParams;
use codex_protocol::protocol::TurnContextItem;

// ---------------------------------------------------------------------------
// AGENTS instructions fragment
// ---------------------------------------------------------------------------

const AGENTS_MD_START_MARKER: &str = "# AGENTS.md instructions for ";
const AGENTS_MD_END_MARKER: &str = "</INSTRUCTIONS>";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename = "user_instructions", rename_all = "snake_case")]
pub(crate) struct AgentsMdInstructions {
    pub directory: String,
    pub text: String,
}

impl ModelVisibleContextFragment for AgentsMdInstructions {
    type Role = ContextualUserContextRole;

    fn render_text(&self) -> String {
        // TODO(ccunningham): Switch AGENTS.md rendering/detection to
        // `<AGENTS.md INSTRUCTIONS FOR {dirname}>...` for consistency with the
        // other contextual-user fragments.
        format!(
            "{prefix}{directory}\n\n<INSTRUCTIONS>\n{contents}\n{suffix}",
            prefix = AGENTS_MD_START_MARKER,
            directory = self.directory,
            contents = self.text,
            suffix = AGENTS_MD_END_MARKER,
        )
    }
}

impl TurnContextDiffFragment for AgentsMdInstructions {
    fn build(
        turn_context: &TurnContext,
        reference_context_item: Option<&TurnContextItem>,
        _params: &TurnContextDiffParams<'_>,
    ) -> Option<Self> {
        let text = turn_context.user_instructions.as_ref()?.clone();
        let current = Self {
            directory: turn_context.cwd.to_string_lossy().into_owned(),
            text,
        };
        if let Some(previous) = reference_context_item {
            let previous_directory = previous.cwd.to_string_lossy().into_owned();
            if previous_directory == current.directory
                && previous.user_instructions.as_deref() == Some(current.text.as_str())
            {
                return None;
            }
        }

        Some(current)
    }
}

impl ContextualUserFragmentDetector for AgentsMdInstructions {
    fn matches_contextual_user_text(text: &str) -> bool {
        let trimmed = text.trim_start();
        // TODO(ccunningham): Switch detection to the XML-ish wrapper once we
        // intentionally change the shipped AGENTS.md fragment format.
        trimmed.starts_with(AGENTS_MD_START_MARKER)
            && trimmed.trim_end().ends_with(AGENTS_MD_END_MARKER)
    }
}

// ---------------------------------------------------------------------------
// Skills fragment
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename = "skill_instructions", rename_all = "snake_case")]
pub(crate) struct SkillInstructions {
    pub name: String,
    pub path: String,
    pub contents: String,
}

impl ModelVisibleContextFragment for SkillInstructions {
    type Role = ContextualUserContextRole;

    fn render_text(&self) -> String {
        Self::wrap_contextual_user_body(format!(
            "<name>{}</name>\n<path>{}</path>\n{}",
            self.name, self.path, self.contents
        ))
    }
}

impl TaggedContextualUserFragment for SkillInstructions {
    const MARKERS: ContextualUserFragmentMarkers =
        ContextualUserFragmentMarkers::new(SKILL_OPEN_TAG, SKILL_CLOSE_TAG);
}

// ---------------------------------------------------------------------------
// Plugins fragment
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename = "plugin_instructions", rename_all = "snake_case")]
pub(crate) struct PluginInstructions {
    pub text: String,
}

impl ModelVisibleContextFragment for PluginInstructions {
    type Role = ContextualUserContextRole;

    fn render_text(&self) -> String {
        Self::wrap_contextual_user_body(self.text.clone())
    }
}

impl TaggedContextualUserFragment for PluginInstructions {
    const MARKERS: ContextualUserFragmentMarkers =
        ContextualUserFragmentMarkers::new(PLUGINS_OPEN_TAG, PLUGINS_CLOSE_TAG);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::ResponseItem;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_user_instructions() {
        let user_instructions = AgentsMdInstructions {
            directory: "test_directory".to_string(),
            text: "test_text".to_string(),
        };
        let response_item = user_instructions.into_message();

        let ResponseItem::Message { role, content, .. } = response_item else {
            panic!("expected ResponseItem::Message");
        };

        assert_eq!(role, "user");

        let [ContentItem::InputText { text }] = content.as_slice() else {
            panic!("expected one InputText content item");
        };

        assert_eq!(
            text,
            "# AGENTS.md instructions for test_directory\n\n<INSTRUCTIONS>\ntest_text\n</INSTRUCTIONS>",
        );
    }

    #[test]
    fn test_is_user_instructions() {
        assert!(crate::model_visible_context::is_contextual_user_fragment(
            &ContentItem::InputText {
                text: "# AGENTS.md instructions for test_directory\n\n<INSTRUCTIONS>\ntest_text\n</INSTRUCTIONS>"
                    .to_string(),
            }
        ));
        assert!(
            <AgentsMdInstructions as ContextualUserFragmentDetector>::matches_contextual_user_text(
                "# AGENTS.md instructions for test_directory\n\n<INSTRUCTIONS>\ntest_text\n</INSTRUCTIONS>"
            )
        );
    }

    #[test]
    fn test_skill_instructions() {
        let skill_instructions = SkillInstructions {
            name: "demo-skill".to_string(),
            path: "skills/demo/SKILL.md".to_string(),
            contents: "body".to_string(),
        };
        let response_item = skill_instructions.into_message();

        let ResponseItem::Message { role, content, .. } = response_item else {
            panic!("expected ResponseItem::Message");
        };

        assert_eq!(role, "user");

        let [ContentItem::InputText { text }] = content.as_slice() else {
            panic!("expected one InputText content item");
        };

        assert_eq!(
            text,
            "<skill>\n<name>demo-skill</name>\n<path>skills/demo/SKILL.md</path>\nbody\n</skill>",
        );
    }

    #[test]
    fn test_is_skill_instructions() {
        assert!(
            <SkillInstructions as ContextualUserFragmentDetector>::matches_contextual_user_text(
                "<skill>\n<name>demo-skill</name>\n<path>skills/demo/SKILL.md</path>\nbody\n</skill>"
            )
        );
        assert!(
            !<SkillInstructions as ContextualUserFragmentDetector>::matches_contextual_user_text(
                "regular text"
            )
        );
    }

    #[test]
    fn test_plugin_instructions() {
        let plugin_instructions = PluginInstructions {
            text: "## Plugins\n- `sample`".to_string(),
        };
        let response_item = plugin_instructions.into_message();

        let ResponseItem::Message { role, content, .. } = response_item else {
            panic!("expected ResponseItem::Message");
        };

        assert_eq!(role, "user");

        let [ContentItem::InputText { text }] = content.as_slice() else {
            panic!("expected one InputText content item");
        };

        assert_eq!(text, "<plugins>\n## Plugins\n- `sample`\n</plugins>");
    }
}
