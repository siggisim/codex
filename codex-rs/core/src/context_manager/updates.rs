mod developer_update_fragments;

use crate::codex::TurnContext;
use crate::model_visible_context::ContextualUserContextRole;
use crate::model_visible_context::ContextualUserTextFragment;
use crate::model_visible_context::DeveloperContextRole;
use crate::model_visible_context::DeveloperTextFragment;
use crate::model_visible_context::ModelVisibleContextFragment;
use crate::model_visible_context::ModelVisibleContextRole;
use crate::model_visible_context::TurnContextDiffFragment;
use crate::model_visible_context::TurnContextDiffParams;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TurnContextItem;
use std::marker::PhantomData;

// Keep fragment-specific diff/render logic in sibling modules so this file can
// focus on one canonical registration list and envelope assembly.
//
// Turn-state context is always emitted as exactly two model-visible envelopes:
// - one developer message
// - one contextual-user message

pub(crate) struct TurnStateEnvelopeFragments {
    pub(crate) developer: Vec<DeveloperTextFragment>,
    pub(crate) contextual_user: Vec<ContextualUserTextFragment>,
}

type DeveloperTurnStateFragmentBuilder = fn(
    Option<&TurnContextItem>,
    &TurnContext,
    &TurnContextDiffParams<'_>,
) -> Option<DeveloperTextFragment>;

fn build_developer_turn_state_fragment<F>(
    reference_context_item: Option<&TurnContextItem>,
    turn_context: &TurnContext,
    params: &TurnContextDiffParams<'_>,
) -> Option<DeveloperTextFragment>
where
    F: TurnContextDiffFragment<Role = DeveloperContextRole>,
{
    let fragment = F::build(turn_context, reference_context_item, params)?;
    Some(DeveloperTextFragment::new(fragment.render_text()))
}

/// Canonical ordered registry for all turn-state developer fragments.
///
/// Add new turn-state fragments by:
/// 1. Defining a typed fragment struct.
/// 2. Implementing `ModelVisibleContextFragment` with `Role = DeveloperContextRole`.
/// 3. Implementing `TurnContextDiffFragment::build`.
/// 4. Registering the type here with `build_developer_turn_state_fragment::<YourType>`.
const REGISTERED_DEVELOPER_TURN_STATE_FRAGMENT_BUILDERS: &[DeveloperTurnStateFragmentBuilder] = &[
    // Keep model-switch instructions first so model-specific guidance is read
    // before any other developer context on this turn.
    build_developer_turn_state_fragment::<
        developer_update_fragments::ModelInstructionsUpdateFragment,
    >,
    build_developer_turn_state_fragment::<developer_update_fragments::PermissionsUpdateFragment>,
    build_developer_turn_state_fragment::<
        developer_update_fragments::CustomDeveloperInstructionsUpdateFragment,
    >,
    build_developer_turn_state_fragment::<
        developer_update_fragments::CollaborationModeUpdateFragment,
    >,
    build_developer_turn_state_fragment::<developer_update_fragments::RealtimeUpdateFragment>,
    build_developer_turn_state_fragment::<developer_update_fragments::PersonalityUpdateFragment>,
];

// Adjacent ContentItems in a single message are effectively concatenated in
// the model-visible token stream, so we inject an explicit separator between
// text fragments to preserve boundaries.
const MODEL_VISIBLE_FRAGMENT_SEPARATOR: &str = "\n\n";

struct ModelVisibleContextEnvelopeBuilder<R: ModelVisibleContextRole> {
    content: Vec<ContentItem>,
    role: PhantomData<R>,
}

impl<R: ModelVisibleContextRole> ModelVisibleContextEnvelopeBuilder<R> {
    fn new() -> Self {
        Self {
            content: Vec::new(),
            role: PhantomData,
        }
    }

    fn push_fragment(&mut self, fragment: impl ModelVisibleContextFragment<Role = R>) {
        if let Some(ContentItem::InputText { text }) = self.content.last_mut()
            && !text.ends_with(MODEL_VISIBLE_FRAGMENT_SEPARATOR)
        {
            text.push_str(MODEL_VISIBLE_FRAGMENT_SEPARATOR);
        }
        self.content.push(fragment.into_content_item());
    }

    fn build(self) -> Option<ResponseItem> {
        build_message::<R>(self.content)
    }
}

pub(crate) struct DeveloperEnvelopeBuilder(
    ModelVisibleContextEnvelopeBuilder<DeveloperContextRole>,
);

impl Default for DeveloperEnvelopeBuilder {
    fn default() -> Self {
        Self(ModelVisibleContextEnvelopeBuilder::new())
    }
}

impl DeveloperEnvelopeBuilder {
    pub(crate) fn push(
        &mut self,
        fragment: impl ModelVisibleContextFragment<Role = DeveloperContextRole>,
    ) {
        self.0.push_fragment(fragment);
    }

    pub(crate) fn build(self) -> Option<ResponseItem> {
        self.0.build()
    }
}

pub(crate) struct ContextualUserEnvelopeBuilder(
    ModelVisibleContextEnvelopeBuilder<ContextualUserContextRole>,
);

impl Default for ContextualUserEnvelopeBuilder {
    fn default() -> Self {
        Self(ModelVisibleContextEnvelopeBuilder::new())
    }
}

impl ContextualUserEnvelopeBuilder {
    pub(crate) fn push_fragment(
        &mut self,
        fragment: impl ModelVisibleContextFragment<Role = ContextualUserContextRole>,
    ) {
        self.0.push_fragment(fragment);
    }

    pub(crate) fn build(self) -> Option<ResponseItem> {
        self.0.build()
    }
}

fn build_message<R: ModelVisibleContextRole>(content: Vec<ContentItem>) -> Option<ResponseItem> {
    if content.is_empty() {
        return None;
    }

    Some(ResponseItem::Message {
        id: None,
        role: R::MESSAGE_ROLE.to_string(),
        content,
        end_turn: None,
        phase: None,
    })
}

pub(crate) fn build_turn_state_envelope_fragments(
    reference_context_item: Option<&TurnContextItem>,
    next: &TurnContext,
    params: &TurnContextDiffParams<'_>,
) -> TurnStateEnvelopeFragments {
    let mut fragments = TurnStateEnvelopeFragments {
        developer: Vec::new(),
        contextual_user: Vec::new(),
    };

    for build in REGISTERED_DEVELOPER_TURN_STATE_FRAGMENT_BUILDERS {
        if let Some(fragment) = build(reference_context_item, next, params) {
            fragments.developer.push(fragment);
        }
    }

    fragments.contextual_user =
        crate::model_visible_context::build_contextual_user_turn_state_fragments(
            reference_context_item,
            next,
            params,
        );

    fragments
}

pub(crate) fn build_settings_update_items(
    previous: Option<&TurnContextItem>,
    next: &TurnContext,
    params: &TurnContextDiffParams<'_>,
) -> Vec<ResponseItem> {
    let mut developer_envelope = DeveloperEnvelopeBuilder::default();
    let fragments = build_turn_state_envelope_fragments(previous, next, params);
    for fragment in fragments.developer {
        developer_envelope.push(fragment);
    }

    let mut contextual_user_envelope = ContextualUserEnvelopeBuilder::default();
    for fragment in fragments.contextual_user {
        contextual_user_envelope.push_fragment(fragment);
    }

    let mut items = Vec::with_capacity(2);
    if let Some(developer_message) = developer_envelope.build() {
        items.push(developer_message);
    }
    if let Some(model_visible_context) = contextual_user_envelope.build() {
        items.push(model_visible_context);
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_visible_context::ContextualUserContextRole;
    use crate::model_visible_context::DeveloperContextRole;
    use crate::model_visible_context::DeveloperTextFragment;
    use codex_protocol::models::ContentItem;
    use pretty_assertions::assert_eq;

    #[test]
    fn developer_envelope_builder_emits_one_message_in_order() {
        let mut builder = DeveloperEnvelopeBuilder::default();
        builder.push(DeveloperTextFragment::new("first"));
        builder.push(DeveloperTextFragment::new("second"));

        let item = builder.build().expect("developer message expected");
        let ResponseItem::Message { role, content, .. } = item else {
            panic!("expected message");
        };

        assert_eq!(role, "developer");
        assert_eq!(
            content,
            vec![
                ContentItem::InputText {
                    text: "first\n\n".to_string()
                },
                ContentItem::InputText {
                    text: "second".to_string()
                },
            ]
        );
    }

    #[derive(Clone, Copy)]
    struct FakeFragment {
        text: &'static str,
    }

    impl ModelVisibleContextFragment for FakeFragment {
        type Role = ContextualUserContextRole;

        fn render_text(&self) -> String {
            self.text.to_string()
        }
    }

    #[derive(Clone, Copy)]
    struct FakeDeveloperFragment {
        text: &'static str,
    }

    impl ModelVisibleContextFragment for FakeDeveloperFragment {
        type Role = DeveloperContextRole;

        fn render_text(&self) -> String {
            self.text.to_string()
        }
    }

    #[test]
    fn contextual_user_envelope_builder_emits_one_message_in_order() {
        let mut builder = ContextualUserEnvelopeBuilder::default();
        builder.push_fragment(FakeFragment { text: "first" });
        builder.push_fragment(FakeFragment { text: "second" });

        let item = builder.build().expect("user message expected");
        let ResponseItem::Message { role, content, .. } = item else {
            panic!("expected message");
        };

        assert_eq!(role, "user");
        assert_eq!(
            content,
            vec![
                ContentItem::InputText {
                    text: "first\n\n".to_string()
                },
                ContentItem::InputText {
                    text: "second".to_string()
                },
            ]
        );
    }

    #[test]
    fn developer_envelope_builder_emits_one_message_with_custom_fragments() {
        let mut builder = DeveloperEnvelopeBuilder::default();
        builder.push(FakeDeveloperFragment { text: "first" });
        builder.push(FakeDeveloperFragment { text: "second" });

        let item = builder.build().expect("developer message expected");
        let ResponseItem::Message { role, content, .. } = item else {
            panic!("expected message");
        };

        assert_eq!(role, "developer");
        assert_eq!(
            content,
            vec![
                ContentItem::InputText {
                    text: "first\n\n".to_string()
                },
                ContentItem::InputText {
                    text: "second".to_string()
                },
            ]
        );
    }
}
