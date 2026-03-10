//! Developer-envelope model-visible fragments and helper functions used by
//! settings-update assembly (`build_settings_update_items`) and initial-context
//! assembly.
//!
//! This module owns the turn-context diffing logic for developer-role context
//! updates (permissions, collaboration mode, realtime, personality, and model
//! switch guidance).

use super::FragmentBuildPass;
use super::TurnStateFragmentRegistration;
use crate::codex::TurnContext;
use crate::features::Feature;
use crate::model_visible_context::DeveloperContextRole;
use crate::model_visible_context::DeveloperTextFragment;
use crate::model_visible_context::ModelVisibleContextFragment;
use crate::model_visible_context::TurnContextDiffFragment;
use crate::model_visible_context::TurnContextDiffParams;
use codex_protocol::config_types::Personality;
use codex_protocol::models::developer_collaboration_mode_text;
use codex_protocol::models::developer_model_switch_text;
use codex_protocol::models::developer_permissions_text;
use codex_protocol::models::developer_personality_spec_text;
use codex_protocol::models::developer_realtime_end_text;
use codex_protocol::models::developer_realtime_start_text;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::protocol::TurnContextItem;

// ---------------------------------------------------------------------------
// Model instructions fragment
// ---------------------------------------------------------------------------

fn model_instructions_update_text(
    previous_model: Option<&str>,
    turn_context: &TurnContext,
) -> Option<String> {
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

    Some(developer_model_switch_text(model_instructions))
}

pub(crate) struct ModelInstructionsUpdateFragment {
    text: String,
}

impl ModelVisibleContextFragment for ModelInstructionsUpdateFragment {
    type Role = DeveloperContextRole;

    fn render_text(&self) -> String {
        self.text.clone()
    }
}

impl TurnContextDiffFragment for ModelInstructionsUpdateFragment {
    fn from_turn_context(
        turn_context: &TurnContext,
        params: &TurnContextDiffParams<'_>,
    ) -> Option<Self> {
        model_instructions_update_text(
            params
                .previous_turn_settings
                .map(|settings| settings.model.as_str()),
            turn_context,
        )
        .map(|text| Self { text })
    }

    fn diff_from_turn_context_item(
        previous: &TurnContextItem,
        turn_context: &TurnContext,
        params: &TurnContextDiffParams<'_>,
    ) -> Option<Self> {
        let previous_model = params
            .previous_turn_settings
            .map(|settings| settings.model.as_str())
            .or(Some(previous.model.as_str()));
        model_instructions_update_text(previous_model, turn_context).map(|text| Self { text })
    }
}

struct PermissionsUpdateFragment {
    text: String,
}

impl ModelVisibleContextFragment for PermissionsUpdateFragment {
    type Role = DeveloperContextRole;

    fn render_text(&self) -> String {
        self.text.clone()
    }
}

impl TurnContextDiffFragment for PermissionsUpdateFragment {
    fn from_turn_context(
        turn_context: &TurnContext,
        params: &TurnContextDiffParams<'_>,
    ) -> Option<Self> {
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

    fn diff_from_turn_context_item(
        previous: &TurnContextItem,
        turn_context: &TurnContext,
        params: &TurnContextDiffParams<'_>,
    ) -> Option<Self> {
        if previous.sandbox_policy == *turn_context.sandbox_policy.get()
            && previous.approval_policy == turn_context.approval_policy.value()
        {
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

struct CustomDeveloperInstructionsUpdateFragment {
    text: String,
}

impl ModelVisibleContextFragment for CustomDeveloperInstructionsUpdateFragment {
    type Role = DeveloperContextRole;

    fn render_text(&self) -> String {
        self.text.clone()
    }
}

impl TurnContextDiffFragment for CustomDeveloperInstructionsUpdateFragment {
    fn from_turn_context(
        turn_context: &TurnContext,
        _params: &TurnContextDiffParams<'_>,
    ) -> Option<Self> {
        turn_context
            .developer_instructions
            .as_ref()
            .map(|text| Self { text: text.clone() })
    }

    fn diff_from_turn_context_item(
        previous: &TurnContextItem,
        turn_context: &TurnContext,
        _params: &TurnContextDiffParams<'_>,
    ) -> Option<Self> {
        if previous.developer_instructions == turn_context.developer_instructions {
            return None;
        }

        turn_context
            .developer_instructions
            .as_ref()
            .map(|text| Self { text: text.clone() })
    }
}

// ---------------------------------------------------------------------------
// Collaboration mode fragment
// ---------------------------------------------------------------------------

struct CollaborationModeUpdateFragment {
    text: String,
}

impl ModelVisibleContextFragment for CollaborationModeUpdateFragment {
    type Role = DeveloperContextRole;

    fn render_text(&self) -> String {
        self.text.clone()
    }
}

impl TurnContextDiffFragment for CollaborationModeUpdateFragment {
    fn from_turn_context(
        turn_context: &TurnContext,
        _params: &TurnContextDiffParams<'_>,
    ) -> Option<Self> {
        developer_collaboration_mode_text(&turn_context.collaboration_mode)
            .map(|text| Self { text })
    }

    fn diff_from_turn_context_item(
        previous: &TurnContextItem,
        turn_context: &TurnContext,
        _params: &TurnContextDiffParams<'_>,
    ) -> Option<Self> {
        if previous.collaboration_mode.as_ref() != Some(&turn_context.collaboration_mode) {
            // If the next mode has empty developer instructions, this returns None and we emit no
            // update, so prior collaboration instructions remain in the prompt history.
            Some(Self {
                text: developer_collaboration_mode_text(&turn_context.collaboration_mode)?,
            })
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Realtime fragment
// ---------------------------------------------------------------------------

fn realtime_update_text(
    previous_realtime_active: Option<bool>,
    current_realtime_active: bool,
    previous_turn_realtime_active: Option<bool>,
) -> Option<String> {
    match (previous_realtime_active, current_realtime_active) {
        (Some(true), false) => Some(developer_realtime_end_text("inactive")),
        (Some(false), true) | (None, true) => Some(developer_realtime_start_text()),
        (Some(true), true) | (Some(false), false) => None,
        (None, false) => previous_turn_realtime_active
            .filter(|realtime_active| *realtime_active)
            .map(|_| developer_realtime_end_text("inactive")),
    }
}

pub(crate) struct RealtimeUpdateFragment {
    text: String,
}

impl ModelVisibleContextFragment for RealtimeUpdateFragment {
    type Role = DeveloperContextRole;

    fn render_text(&self) -> String {
        self.text.clone()
    }
}

impl TurnContextDiffFragment for RealtimeUpdateFragment {
    fn from_turn_context(
        turn_context: &TurnContext,
        params: &TurnContextDiffParams<'_>,
    ) -> Option<Self> {
        realtime_update_text(
            None,
            turn_context.realtime_active,
            params
                .previous_turn_settings
                .and_then(|settings| settings.realtime_active),
        )
        .map(|text| Self { text })
    }

    fn diff_from_turn_context_item(
        previous: &TurnContextItem,
        turn_context: &TurnContext,
        params: &TurnContextDiffParams<'_>,
    ) -> Option<Self> {
        realtime_update_text(
            previous.realtime_active,
            turn_context.realtime_active,
            params
                .previous_turn_settings
                .and_then(|settings| settings.realtime_active),
        )
        .map(|text| Self { text })
    }
}

// ---------------------------------------------------------------------------
// Personality fragment
// ---------------------------------------------------------------------------

fn personality_message_for(model_info: &ModelInfo, personality: Personality) -> Option<String> {
    model_info
        .model_messages
        .as_ref()
        .and_then(|spec| spec.get_personality_message(Some(personality)))
        .filter(|message| !message.is_empty())
}

struct PersonalityUpdateFragment {
    text: String,
}

impl ModelVisibleContextFragment for PersonalityUpdateFragment {
    type Role = DeveloperContextRole;

    fn render_text(&self) -> String {
        self.text.clone()
    }
}

impl TurnContextDiffFragment for PersonalityUpdateFragment {
    fn from_turn_context(
        turn_context: &TurnContext,
        params: &TurnContextDiffParams<'_>,
    ) -> Option<Self> {
        if !params.personality_feature_enabled {
            return None;
        }

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
        let personality_message = personality_message_for(&turn_context.model_info, personality)?;
        Some(Self {
            text: developer_personality_spec_text(personality_message),
        })
    }

    fn diff_from_turn_context_item(
        previous: &TurnContextItem,
        turn_context: &TurnContext,
        params: &TurnContextDiffParams<'_>,
    ) -> Option<Self> {
        if !params.personality_feature_enabled {
            return None;
        }
        if turn_context.model_info.slug != previous.model {
            return None;
        }

        if let Some(personality) = turn_context.personality
            && turn_context.personality != previous.personality
        {
            let model_info = &turn_context.model_info;
            let personality_message = personality_message_for(model_info, personality)?;
            Some(Self {
                text: developer_personality_spec_text(personality_message),
            })
        } else {
            None
        }
    }
}

fn build_registered_developer_fragment<F>(
    pass: FragmentBuildPass,
    previous: Option<&TurnContextItem>,
    turn_context: &TurnContext,
    params: &TurnContextDiffParams<'_>,
) -> Option<DeveloperTextFragment>
where
    F: TurnContextDiffFragment<Role = DeveloperContextRole>,
{
    let fragment = match pass {
        FragmentBuildPass::InitialContext => F::from_turn_context(turn_context, params),
        FragmentBuildPass::TurnDiff => match previous {
            Some(previous) => F::diff_from_turn_context_item(previous, turn_context, params),
            None => F::from_turn_context(turn_context, params),
        },
    }?;
    Some(DeveloperTextFragment::new(fragment.render_text()))
}

struct DeveloperFragmentRegistration {
    build: fn(
        FragmentBuildPass,
        Option<&TurnContextItem>,
        &TurnContext,
        &TurnContextDiffParams<'_>,
    ) -> Option<DeveloperTextFragment>,
}

impl DeveloperFragmentRegistration {
    const fn of<F>() -> Self
    where
        F: TurnContextDiffFragment<Role = DeveloperContextRole>,
    {
        Self {
            build: build_registered_developer_fragment::<F>,
        }
    }
}

impl TurnStateFragmentRegistration for DeveloperFragmentRegistration {
    type Role = DeveloperContextRole;
    type Fragment = DeveloperTextFragment;

    fn build(
        &self,
        pass: FragmentBuildPass,
        previous: Option<&TurnContextItem>,
        turn_context: &TurnContext,
        params: &TurnContextDiffParams<'_>,
    ) -> Option<Self::Fragment> {
        (self.build)(pass, previous, turn_context, params)
    }
}

const REGISTERED_DEVELOPER_FRAGMENT_BUILDERS: &[DeveloperFragmentRegistration] = &[
    // Keep model-switch instructions first so model-specific guidance is read
    // before any other context diffs on this turn.
    DeveloperFragmentRegistration::of::<ModelInstructionsUpdateFragment>(),
    DeveloperFragmentRegistration::of::<PermissionsUpdateFragment>(),
    DeveloperFragmentRegistration::of::<CustomDeveloperInstructionsUpdateFragment>(),
    DeveloperFragmentRegistration::of::<CollaborationModeUpdateFragment>(),
    DeveloperFragmentRegistration::of::<RealtimeUpdateFragment>(),
    DeveloperFragmentRegistration::of::<PersonalityUpdateFragment>(),
];

pub(super) fn build_registered_developer_fragments(
    pass: FragmentBuildPass,
    previous: Option<&TurnContextItem>,
    next: &TurnContext,
    params: &TurnContextDiffParams<'_>,
) -> Vec<DeveloperTextFragment> {
    REGISTERED_DEVELOPER_FRAGMENT_BUILDERS
        .iter()
        .filter_map(|registration| {
            TurnStateFragmentRegistration::build(registration, pass, previous, next, params)
        })
        .collect()
}
