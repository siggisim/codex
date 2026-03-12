//! Shared model-visible context abstractions.
//!
//! Use this path for any injected prompt context, whether it renders in the
//! developer envelope or the contextual-user envelope.
//!
//! Contextual-user fragments must provide stable markers so history parsing can
//! distinguish them from real user intent. Developer fragments do not need
//! markers because they are already separable by role.

use crate::codex::PreviousTurnSettings;
use crate::codex::TurnContext;
use crate::shell::Shell;
use codex_execpolicy::Policy;
use codex_protocol::models::ContentItem;
use codex_protocol::models::CustomDeveloperInstructions;
use codex_protocol::models::MessageRole;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TurnContextItem;

pub(crate) const SKILL_OPEN_TAG: &str = "<skill>";
pub(crate) const SKILL_CLOSE_TAG: &str = "</skill>";
pub(crate) const USER_SHELL_COMMAND_OPEN_TAG: &str = "<user_shell_command>";
pub(crate) const USER_SHELL_COMMAND_CLOSE_TAG: &str = "</user_shell_command>";
pub(crate) const TURN_ABORTED_OPEN_TAG: &str = "<turn_aborted>";
pub(crate) const TURN_ABORTED_CLOSE_TAG: &str = "</turn_aborted>";
pub(crate) const PLUGINS_OPEN_TAG: &str = "<plugins>";
pub(crate) const PLUGINS_CLOSE_TAG: &str = "</plugins>";
pub(crate) const SUBAGENTS_OPEN_TAG: &str = "<subagents>";
pub(crate) const SUBAGENTS_CLOSE_TAG: &str = "</subagents>";
pub(crate) const SUBAGENT_NOTIFICATION_OPEN_TAG: &str = "<subagent_notification>";
pub(crate) const SUBAGENT_NOTIFICATION_CLOSE_TAG: &str = "</subagent_notification>";

pub(crate) trait ModelVisibleContextRole {
    const MESSAGE_ROLE: MessageRole;
}

pub(crate) struct DeveloperContextRole;

impl ModelVisibleContextRole for DeveloperContextRole {
    const MESSAGE_ROLE: MessageRole = MessageRole::Developer;
}

pub(crate) struct ContextualUserContextRole;

impl ModelVisibleContextRole for ContextualUserContextRole {
    const MESSAGE_ROLE: MessageRole = MessageRole::User;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ContextualUserFragmentMarkers {
    start_marker: &'static str,
    end_marker: &'static str,
}

pub(crate) trait ContextualUserFragmentDetector {
    fn matches_contextual_user_text(text: &str) -> bool;
}

pub(crate) trait TaggedContextualUserFragment {
    const MARKERS: ContextualUserFragmentMarkers;

    fn wrap_contextual_user_body(body: String) -> String {
        Self::MARKERS.wrap_body(body)
    }
}

impl<T: TaggedContextualUserFragment> ContextualUserFragmentDetector for T {
    fn matches_contextual_user_text(text: &str) -> bool {
        T::MARKERS.matches_text(text)
    }
}

impl ContextualUserFragmentMarkers {
    pub(crate) const fn new(start_marker: &'static str, end_marker: &'static str) -> Self {
        Self {
            start_marker,
            end_marker,
        }
    }

    pub(crate) fn matches_text(self, text: &str) -> bool {
        let trimmed = text.trim_start();
        let starts_with_marker = trimmed
            .get(..self.start_marker.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(self.start_marker));
        let trimmed = trimmed.trim_end();
        let ends_with_marker = trimmed
            .get(trimmed.len().saturating_sub(self.end_marker.len())..)
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(self.end_marker));
        starts_with_marker && ends_with_marker
    }

    pub(crate) fn wrap_body(self, body: String) -> String {
        format!("{}\n{}\n{}", self.start_marker, body, self.end_marker)
    }
}

pub(crate) fn model_visible_content_item(text: String) -> ContentItem {
    ContentItem::InputText { text }
}

pub(crate) fn model_visible_message<R: ModelVisibleContextRole>(text: String) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: R::MESSAGE_ROLE.to_string(),
        content: vec![model_visible_content_item(text)],
        end_turn: None,
        phase: None,
    }
}

pub(crate) fn model_visible_response_input_item<R: ModelVisibleContextRole>(
    text: String,
) -> ResponseInputItem {
    ResponseInputItem::Message {
        role: R::MESSAGE_ROLE.to_string(),
        content: vec![model_visible_content_item(text)],
    }
}

/// Implement this for any model-visible prompt fragment, regardless of which
/// envelope it renders into.
pub(crate) trait ModelVisibleContextFragment {
    type Role: ModelVisibleContextRole;

    fn render_text(&self) -> String;

    fn into_content_item(self) -> ContentItem
    where
        Self: Sized,
    {
        model_visible_content_item(self.render_text())
    }

    fn into_message(self) -> ResponseItem
    where
        Self: Sized,
    {
        model_visible_message::<Self::Role>(self.render_text())
    }

    fn into_response_input_item(self) -> ResponseInputItem
    where
        Self: Sized,
    {
        model_visible_response_input_item::<Self::Role>(self.render_text())
    }
}

pub(crate) struct DeveloperTextFragment {
    text: String,
}

impl DeveloperTextFragment {
    pub(crate) fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

pub(crate) struct ContextualUserTextFragment {
    text: String,
}

impl ContextualUserTextFragment {
    pub(crate) fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

type ContextualUserTurnStateBuilder = fn(
    Option<&TurnContextItem>,
    &TurnContext,
    &TurnContextDiffParams<'_>,
) -> Option<ContextualUserTextFragment>;

#[derive(Clone, Copy)]
struct ContextualUserFragmentRegistration {
    detect: fn(&str) -> bool,
    turn_state_builder: Option<ContextualUserTurnStateBuilder>,
}

impl ContextualUserFragmentRegistration {
    const fn new(
        detect: fn(&str) -> bool,
        turn_state_builder: Option<ContextualUserTurnStateBuilder>,
    ) -> Self {
        Self {
            detect,
            turn_state_builder,
        }
    }
}

pub(crate) struct TurnContextDiffParams<'a> {
    pub(crate) shell: &'a Shell,
    pub(crate) previous_turn_settings: Option<&'a PreviousTurnSettings>,
    pub(crate) exec_policy: &'a Policy,
    pub(crate) personality_feature_enabled: bool,
    pub(crate) base_instructions: Option<&'a str>,
}

impl<'a> TurnContextDiffParams<'a> {
    pub(crate) fn new(
        shell: &'a Shell,
        previous_turn_settings: Option<&'a PreviousTurnSettings>,
        exec_policy: &'a Policy,
        personality_feature_enabled: bool,
        base_instructions: Option<&'a str>,
    ) -> Self {
        Self {
            shell,
            previous_turn_settings,
            exec_policy,
            personality_feature_enabled,
            base_instructions,
        }
    }
}

/// Implement this for fragments that are built from current/persisted turn
/// state rather than one-off runtime events.
pub(crate) trait TurnContextDiffFragment: ModelVisibleContextFragment + Sized {
    /// Build the fragment from the current turn state and an optional baseline
    /// context item.
    ///
    /// `reference_context_item` is the last persisted turn-context snapshot whose
    /// effects are already represented in model-visible history. Implementations
    /// should diff `turn_context` against this baseline and return `None` when
    /// there is no model-visible change to inject.
    ///
    /// `reference_context_item` is `None` for initial-context assembly and when
    /// no baseline turn context can be recovered (for example after
    /// compaction/backtracking/resume), so implementations should treat that as
    /// "no known represented baseline" and decide whether to emit full current
    /// state or nothing.
    fn build(
        turn_context: &TurnContext,
        reference_context_item: Option<&TurnContextItem>,
        params: &TurnContextDiffParams<'_>,
    ) -> Option<Self>;
}

fn detect_contextual_user_fragment<F: ContextualUserFragmentDetector>(text: &str) -> bool {
    F::matches_contextual_user_text(text)
}

fn build_contextual_user_turn_state_fragment<F>(
    reference_context_item: Option<&TurnContextItem>,
    turn_context: &TurnContext,
    params: &TurnContextDiffParams<'_>,
) -> Option<ContextualUserTextFragment>
where
    F: TurnContextDiffFragment<Role = ContextualUserContextRole>,
{
    let fragment = F::build(turn_context, reference_context_item, params)?;
    Some(ContextualUserTextFragment::new(fragment.render_text()))
}

/// Canonical contextual-user fragment registry.
///
/// Add new contextual-user fragment types here so injected context is not
/// mistaken for real user intent during event mapping/truncation, and to wire
/// turn-state contextual-user diff fragments in one place.
const REGISTERED_CONTEXTUAL_USER_FRAGMENTS: &[ContextualUserFragmentRegistration] = &[
    ContextualUserFragmentRegistration::new(
        detect_contextual_user_fragment::<crate::instructions::AgentsMdInstructions>,
        Some(
            build_contextual_user_turn_state_fragment::<crate::instructions::AgentsMdInstructions>,
        ),
    ),
    ContextualUserFragmentRegistration::new(
        detect_contextual_user_fragment::<crate::environment_context::EnvironmentContext>,
        Some(
            build_contextual_user_turn_state_fragment::<
                crate::environment_context::EnvironmentContext,
            >,
        ),
    ),
    ContextualUserFragmentRegistration::new(
        detect_contextual_user_fragment::<crate::instructions::SkillInstructions>,
        None,
    ),
    ContextualUserFragmentRegistration::new(
        detect_contextual_user_fragment::<crate::instructions::PluginInstructions>,
        None,
    ),
    ContextualUserFragmentRegistration::new(
        detect_contextual_user_fragment::<crate::user_shell_command::UserShellCommandFragment>,
        None,
    ),
    ContextualUserFragmentRegistration::new(
        detect_contextual_user_fragment::<crate::tasks::TurnAbortedMarker>,
        None,
    ),
];

fn is_legacy_contextual_user_fragment(text: &str) -> bool {
    // TODO(ccunningham): Drop this once old user-role subagent notification
    // history no longer needs resume/compaction compatibility.
    ContextualUserFragmentMarkers::new(
        SUBAGENT_NOTIFICATION_OPEN_TAG,
        SUBAGENT_NOTIFICATION_CLOSE_TAG,
    )
    .matches_text(text)
}

pub(crate) fn is_contextual_user_fragment(content_item: &ContentItem) -> bool {
    let ContentItem::InputText { text } = content_item else {
        return false;
    };
    REGISTERED_CONTEXTUAL_USER_FRAGMENTS
        .iter()
        .any(|registration| (registration.detect)(text))
        || is_legacy_contextual_user_fragment(text)
}

pub(crate) fn build_contextual_user_turn_state_fragments(
    reference_context_item: Option<&TurnContextItem>,
    turn_context: &TurnContext,
    params: &TurnContextDiffParams<'_>,
) -> Vec<ContextualUserTextFragment> {
    REGISTERED_CONTEXTUAL_USER_FRAGMENTS
        .iter()
        .filter_map(|registration| {
            registration
                .turn_state_builder
                .and_then(|build| build(reference_context_item, turn_context, params))
        })
        .collect()
}

impl ModelVisibleContextFragment for CustomDeveloperInstructions {
    type Role = DeveloperContextRole;

    fn render_text(&self) -> String {
        self.clone().into_text()
    }
}

impl ModelVisibleContextFragment for DeveloperTextFragment {
    type Role = DeveloperContextRole;

    fn render_text(&self) -> String {
        self.text.clone()
    }
}

impl ModelVisibleContextFragment for ContextualUserTextFragment {
    type Role = ContextualUserContextRole;

    fn render_text(&self) -> String {
        self.text.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_environment_context_fragment() {
        assert!(is_contextual_user_fragment(&ContentItem::InputText {
            text: "<environment_context>\n<cwd>/tmp</cwd>\n</environment_context>".to_string(),
        }));
    }

    #[test]
    fn detects_agents_instructions_fragment() {
        assert!(is_contextual_user_fragment(&ContentItem::InputText {
            text: "# AGENTS.md instructions for /tmp\n\n<INSTRUCTIONS>\nbody\n</INSTRUCTIONS>"
                .to_string(),
        }));
    }

    #[test]
    fn detects_legacy_subagent_notification_fragment() {
        assert!(is_contextual_user_fragment(&ContentItem::InputText {
            text: "<subagent_notification>\n{\"agent_id\":\"a\",\"status\":\"completed\"}\n</subagent_notification>"
                .to_string(),
        }));
    }

    #[test]
    fn ignores_regular_user_text() {
        assert!(!is_contextual_user_fragment(&ContentItem::InputText {
            text: "hello".to_string(),
        }));
    }

    #[test]
    fn marker_matching_ignores_plain_text() {
        assert!(
            !<crate::instructions::SkillInstructions as ContextualUserFragmentDetector>::matches_contextual_user_text("plain text")
        );
    }
}
