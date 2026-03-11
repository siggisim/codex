//! Developer-envelope model-visible fragments used by turn-state context
//! assembly.
//!
//! This module owns the turn-context diffing logic for developer-role context
//! updates (permissions, collaboration mode, realtime, personality, and model
//! switch guidance).

use crate::codex::TurnContext;
use crate::features::Feature;
use crate::model_visible_context::DeveloperContextRole;
use crate::model_visible_context::ModelVisibleContextFragment;
use crate::model_visible_context::TurnContextDiffFragment;
use crate::model_visible_context::TurnContextDiffParams;
use codex_protocol::models::developer_collaboration_mode_text;
use codex_protocol::models::developer_model_switch_text;
use codex_protocol::models::developer_permissions_text;
use codex_protocol::models::developer_personality_spec_text;
use codex_protocol::models::developer_realtime_end_text;
use codex_protocol::models::developer_realtime_start_text;
use codex_protocol::protocol::TurnContextItem;

// ---------------------------------------------------------------------------
// Model instructions fragment
// ---------------------------------------------------------------------------

pub(super) struct ModelInstructionsUpdateFragment {
    text: String,
}

impl ModelVisibleContextFragment for ModelInstructionsUpdateFragment {
    type Role = DeveloperContextRole;

    fn render_text(&self) -> String {
        self.text.clone()
    }
}

impl TurnContextDiffFragment for ModelInstructionsUpdateFragment {
    fn build(
        turn_context: &TurnContext,
        _reference_context_item: Option<&TurnContextItem>,
        params: &TurnContextDiffParams<'_>,
    ) -> Option<Self> {
        let previous_model = params
            .previous_turn_settings
            .map(|settings| settings.model.as_str());
        let previous_model = previous_model?;
        if previous_model == turn_context.model_info.slug.as_str() {
            return None;
        }

        let model_instructions = turn_context
            .model_info
            .get_model_instructions(turn_context.personality);
        if model_instructions.is_empty() {
            return None;
        }

        Some(Self {
            text: developer_model_switch_text(model_instructions),
        })
    }
}

// ---------------------------------------------------------------------------
// Permissions fragment
// ---------------------------------------------------------------------------

pub(super) struct PermissionsUpdateFragment {
    text: String,
}

impl ModelVisibleContextFragment for PermissionsUpdateFragment {
    type Role = DeveloperContextRole;

    fn render_text(&self) -> String {
        self.text.clone()
    }
}

impl TurnContextDiffFragment for PermissionsUpdateFragment {
    fn build(
        turn_context: &TurnContext,
        reference_context_item: Option<&TurnContextItem>,
        params: &TurnContextDiffParams<'_>,
    ) -> Option<Self> {
        if reference_context_item.is_some_and(|previous| {
            previous.sandbox_policy == *turn_context.sandbox_policy.get()
                && previous.approval_policy == turn_context.approval_policy.value()
        }) {
            return None;
        }

        Some(Self {
            text: developer_permissions_text(
                turn_context.sandbox_policy.get(),
                turn_context.approval_policy.value(),
                turn_context.features.enabled(Feature::GuardianApproval),
                params.exec_policy,
                &turn_context.cwd,
                turn_context.features.enabled(Feature::RequestPermissions),
            ),
        })
    }
}

// ---------------------------------------------------------------------------
// Custom developer instructions fragment
// ---------------------------------------------------------------------------

pub(super) struct CustomDeveloperInstructionsUpdateFragment {
    text: String,
}

impl ModelVisibleContextFragment for CustomDeveloperInstructionsUpdateFragment {
    type Role = DeveloperContextRole;

    fn render_text(&self) -> String {
        self.text.clone()
    }
}

impl TurnContextDiffFragment for CustomDeveloperInstructionsUpdateFragment {
    fn build(
        turn_context: &TurnContext,
        reference_context_item: Option<&TurnContextItem>,
        _params: &TurnContextDiffParams<'_>,
    ) -> Option<Self> {
        if reference_context_item.is_some_and(|previous| {
            previous.developer_instructions == turn_context.developer_instructions
        }) {
            return None;
        }

        Some(Self {
            text: turn_context.developer_instructions.as_ref()?.clone(),
        })
    }
}

// ---------------------------------------------------------------------------
// Collaboration mode fragment
// ---------------------------------------------------------------------------

pub(super) struct CollaborationModeUpdateFragment {
    text: String,
}

impl ModelVisibleContextFragment for CollaborationModeUpdateFragment {
    type Role = DeveloperContextRole;

    fn render_text(&self) -> String {
        self.text.clone()
    }
}

impl TurnContextDiffFragment for CollaborationModeUpdateFragment {
    fn build(
        turn_context: &TurnContext,
        reference_context_item: Option<&TurnContextItem>,
        _params: &TurnContextDiffParams<'_>,
    ) -> Option<Self> {
        if let Some(previous) = reference_context_item {
            if previous.collaboration_mode.as_ref() != Some(&turn_context.collaboration_mode) {
                // If the next mode has empty developer instructions, this returns None and we emit no
                // update, so prior collaboration instructions remain in the prompt history.
                return Some(Self {
                    text: developer_collaboration_mode_text(&turn_context.collaboration_mode)?,
                });
            }
            return None;
        }

        developer_collaboration_mode_text(&turn_context.collaboration_mode)
            .map(|text| Self { text })
    }
}

// ---------------------------------------------------------------------------
// Realtime fragment
// ---------------------------------------------------------------------------

pub(super) struct RealtimeUpdateFragment {
    text: String,
}

impl ModelVisibleContextFragment for RealtimeUpdateFragment {
    type Role = DeveloperContextRole;

    fn render_text(&self) -> String {
        self.text.clone()
    }
}

impl TurnContextDiffFragment for RealtimeUpdateFragment {
    fn build(
        turn_context: &TurnContext,
        reference_context_item: Option<&TurnContextItem>,
        params: &TurnContextDiffParams<'_>,
    ) -> Option<Self> {
        let text = match (
            reference_context_item.and_then(|previous| previous.realtime_active),
            turn_context.realtime_active,
        ) {
            (Some(true), false) => Some(developer_realtime_end_text("inactive")),
            (Some(false), true) | (None, true) => Some(developer_realtime_start_text()),
            (Some(true), true) | (Some(false), false) => None,
            (None, false) => params
                .previous_turn_settings
                .and_then(|settings| settings.realtime_active)
                .filter(|realtime_active| *realtime_active)
                .map(|_| developer_realtime_end_text("inactive")),
        }?;

        Some(Self { text })
    }
}

// ---------------------------------------------------------------------------
// Personality fragment
// ---------------------------------------------------------------------------

pub(super) struct PersonalityUpdateFragment {
    text: String,
}

impl ModelVisibleContextFragment for PersonalityUpdateFragment {
    type Role = DeveloperContextRole;

    fn render_text(&self) -> String {
        self.text.clone()
    }
}

impl TurnContextDiffFragment for PersonalityUpdateFragment {
    fn build(
        turn_context: &TurnContext,
        reference_context_item: Option<&TurnContextItem>,
        params: &TurnContextDiffParams<'_>,
    ) -> Option<Self> {
        if !params.personality_feature_enabled {
            return None;
        }

        let Some(previous) = reference_context_item else {
            let personality = turn_context.personality?;
            let has_baked_personality = params.base_instructions.is_some_and(|base_instructions| {
                turn_context.model_info.supports_personality()
                    && base_instructions
                        == turn_context
                            .model_info
                            .get_model_instructions(Some(personality))
            });
            if has_baked_personality {
                return None;
            }
            let personality_message = turn_context
                .model_info
                .model_messages
                .as_ref()
                .and_then(|spec| spec.get_personality_message(Some(personality)))
                .filter(|message| !message.is_empty())?;
            return Some(Self {
                text: developer_personality_spec_text(personality_message),
            });
        };

        if turn_context.model_info.slug != previous.model {
            return None;
        }
        if let Some(personality) = turn_context.personality
            && turn_context.personality != previous.personality
        {
            let personality_message = turn_context
                .model_info
                .model_messages
                .as_ref()
                .and_then(|spec| spec.get_personality_message(Some(personality)))
                .filter(|message| !message.is_empty())?;
            return Some(Self {
                text: developer_personality_spec_text(personality_message),
            });
        }

        None
    }
}
