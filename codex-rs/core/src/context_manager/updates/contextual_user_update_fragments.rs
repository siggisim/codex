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
use crate::model_visible_context::TurnContextDiffFragment;
use crate::model_visible_context::TurnContextDiffParams;
use codex_protocol::protocol::TurnContextItem;

fn build_registered_contextual_user_fragment<F>(
    pass: FragmentBuildPass,
    previous: Option<&TurnContextItem>,
    turn_context: &TurnContext,
    params: &TurnContextDiffParams<'_>,
) -> Option<ContextualUserTextFragment>
where
    F: TurnContextDiffFragment<Role = crate::model_visible_context::ContextualUserContextRole>,
{
    let fragment = match pass {
        FragmentBuildPass::InitialContext => F::from_turn_context(turn_context, params),
        FragmentBuildPass::SettingsUpdate => match previous {
            Some(previous) => F::diff_from_turn_context_item(previous, turn_context, params),
            None => F::from_turn_context(turn_context, params),
        },
    }?;
    Some(ContextualUserTextFragment::new(fragment.render_text()))
}

struct ContextualUserFragmentRegistration {
    build: fn(
        FragmentBuildPass,
        Option<&TurnContextItem>,
        &TurnContext,
        &TurnContextDiffParams<'_>,
    ) -> Option<ContextualUserTextFragment>,
}

impl ContextualUserFragmentRegistration {
    const fn of<F>() -> Self
    where
        F: TurnContextDiffFragment<Role = crate::model_visible_context::ContextualUserContextRole>,
    {
        Self {
            build: build_registered_contextual_user_fragment::<F>,
        }
    }

    fn build(
        &self,
        pass: FragmentBuildPass,
        previous: Option<&TurnContextItem>,
        turn_context: &TurnContext,
        params: &TurnContextDiffParams<'_>,
    ) -> Option<ContextualUserTextFragment> {
        (self.build)(pass, previous, turn_context, params)
    }
}

// TurnContextDiffFragment uses static constructors returning `Self`, which are
// not object-safe for `dyn` dispatch. This typed registration adapter preserves
// "register a fragment type once" ergonomics while keeping fragment behavior in
// the trait implementations.

/// Canonical registry for turn-state contextual-user fragments.
///
/// To add a new turn-state contextual-user model-visible fragment:
/// 1. Define a typed fragment struct in its owning module.
/// 2. Implement `ModelVisibleContextFragment` (contextual-user role + stable marker rendering).
/// 3. Implement `TurnContextDiffFragment` if the fragment should be built from current turn state
///    and diffed against persisted `TurnContextItem`.
/// 4. Register the type here with `ContextualUserFragmentRegistration::of::<YourType>()` and
///    keep ordering intentional.
const REGISTERED_CONTEXTUAL_USER_FRAGMENT_BUILDERS: &[ContextualUserFragmentRegistration] = &[
    ContextualUserFragmentRegistration::of::<AgentsMdInstructions>(),
    ContextualUserFragmentRegistration::of::<EnvironmentContext>(),
];

pub(super) fn build_registered_contextual_user_fragments(
    pass: FragmentBuildPass,
    previous: Option<&TurnContextItem>,
    next: &TurnContext,
    params: &TurnContextDiffParams<'_>,
) -> Vec<ContextualUserTextFragment> {
    REGISTERED_CONTEXTUAL_USER_FRAGMENT_BUILDERS
        .iter()
        .filter_map(|registration| registration.build(pass, previous, next, params))
        .collect()
}
