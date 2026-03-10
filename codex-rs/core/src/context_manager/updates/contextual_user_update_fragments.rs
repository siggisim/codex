//! Contextual-user-envelope model-visible fragments used by initial-context and
//! settings-update assembly.
//!
//! This module owns the registered turn-state contextual-user fragments so both
//! `build_initial_context` and `build_settings_update_items` can iterate one
//! canonical list.

use super::FragmentBuildPass;
use crate::codex::TurnContext;
use crate::environment_context::EnvironmentContext;
use crate::instructions::AgentsMdInstructions;
use crate::model_visible_context::ContextualUserTextFragment;
use crate::model_visible_context::ModelVisibleContextFragment;
use crate::model_visible_context::TurnContextDiffFragment;
use crate::model_visible_context::TurnContextDiffParams;
use codex_protocol::protocol::TurnContextItem;

// We use function pointers here (instead of a trait-object registry) because
// turn-state fragment construction/diffing is modeled as static constructors on
// concrete fragment types (`from_turn_context` / `diff_from_turn_context_item`)
// that return `Self`. That pattern is not object-safe for `dyn` dispatch.
type ContextualUserFragmentBuilder = fn(
    FragmentBuildPass,
    Option<&TurnContextItem>,
    &TurnContext,
    &TurnContextDiffParams<'_>,
) -> Option<ContextualUserTextFragment>;

/// Canonical registry for turn-state contextual-user fragments.
///
/// To add a new turn-state contextual-user model-visible fragment:
/// 1. Define a typed fragment struct in its owning module.
/// 2. Implement `ModelVisibleContextFragment` (contextual-user role + stable marker rendering).
/// 3. Implement `TurnContextDiffFragment` if the fragment should be built from current turn state
///    and diffed against persisted `TurnContextItem`.
/// 4. Add one builder entry to this list (and keep ordering intentional).
const REGISTERED_CONTEXTUAL_USER_FRAGMENT_BUILDERS: &[ContextualUserFragmentBuilder] =
    &[build_agents_md_fragment, build_environment_context_fragment];

fn build_agents_md_fragment(
    pass: FragmentBuildPass,
    previous: Option<&TurnContextItem>,
    turn_context: &TurnContext,
    params: &TurnContextDiffParams<'_>,
) -> Option<ContextualUserTextFragment> {
    match pass {
        FragmentBuildPass::InitialContext => {
            AgentsMdInstructions::from_turn_context(turn_context, params)
        }
        FragmentBuildPass::SettingsUpdate => previous.and_then(|previous| {
            AgentsMdInstructions::diff_from_turn_context_item(previous, turn_context, params)
        }),
    }
    .map(|fragment| ContextualUserTextFragment::new(fragment.render_text()))
}

fn build_environment_context_fragment(
    pass: FragmentBuildPass,
    previous: Option<&TurnContextItem>,
    turn_context: &TurnContext,
    params: &TurnContextDiffParams<'_>,
) -> Option<ContextualUserTextFragment> {
    match pass {
        FragmentBuildPass::InitialContext => {
            EnvironmentContext::from_turn_context(turn_context, params)
        }
        FragmentBuildPass::SettingsUpdate => previous.and_then(|previous| {
            EnvironmentContext::diff_from_turn_context_item(previous, turn_context, params)
        }),
    }
    .map(|fragment| ContextualUserTextFragment::new(fragment.render_text()))
}

pub(super) fn build_registered_contextual_user_fragments(
    pass: FragmentBuildPass,
    previous: Option<&TurnContextItem>,
    next: &TurnContext,
    params: &TurnContextDiffParams<'_>,
) -> Vec<ContextualUserTextFragment> {
    REGISTERED_CONTEXTUAL_USER_FRAGMENT_BUILDERS
        .iter()
        .filter_map(|builder| builder(pass, previous, next, params))
        .collect()
}
