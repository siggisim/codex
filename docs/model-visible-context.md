# Model-visible context fragments

Codex injects model-visible context through two envelopes:

- the developer envelope, rendered as a single `developer` message
- the contextual-user envelope, rendered as a single `user` message whose contents are contextual state rather than real user intent

Both envelopes use the same internal fragment contract in `codex-rs/core`.
Envelope builders normalize text fragment boundaries by inserting `\n\n` between adjacent text content items, so fragments do not run together in the model-visible token stream.

## Canonical rules

1. All model-visible injected context must be represented as a typed `ModelVisibleContextFragment`.
2. Turn-state context assembly always produces exactly two envelopes: one developer message and one contextual-user message.
3. Contextual-user fragments must have stable detection so history parsing can distinguish contextual state from true user intent.
4. If a fragment is derived from durable/current turn state and should survive history-mutating flows (resume/fork/compaction/backtracking) via re-diffing, it must implement `TurnContextDiffFragment`.
5. Do not hand-construct model-visible `ResponseItem::Message` payloads in new code. Use fragment conversion (`into_message` / `into_response_input_item`) and envelope builders.

Rule 2 applies to turn-state context assembly (`build_initial_context` / `build_settings_update_items`). Runtime or session-prefix events may inject standalone fragment messages, but those still must be typed `ModelVisibleContextFragment`s (rules 1 and 5).

## Blessed path

When adding new model-visible context:

1. Define a typed fragment type.
2. Implement `ModelVisibleContextFragment` for it.
3. Set the fragment `type Role` to the correct developer or contextual-user role.
4. If it is a contextual-user fragment, implement contextual-user detection:
   - prefer `TaggedContextualUserFragment` for marker-based detection/wrapping
   - use `ContextualUserFragmentDetector` when matching is dynamic (for example AGENTS.md tags that embed directory names)
5. If the fragment is derived from durable/current turn state and should be diffed/reinjected after history mutations, also implement `TurnContextDiffFragment`.
6. Register the fragment type:
   - developer turn-state fragments: add to `REGISTERED_DEVELOPER_TURN_STATE_FRAGMENT_BUILDERS` in `context_manager/updates.rs`.
   - contextual-user fragments: add once to `REGISTERED_CONTEXTUAL_USER_FRAGMENTS` in `model_visible_context.rs` (this powers both history detection and optional turn-state diff assembly).
7. Push the resulting fragments through the shared envelope builders.

Do not hand-build developer or contextual-user model-visible `ResponseItem`s in new code.

The role lives in the fragment's associated `type Role`.

## Choosing an envelope

Use the developer envelope for developer-role guidance:

- permissions / approval policy instructions
- collaboration-mode developer guidance
- model switch and realtime notices
- personality guidance
- subagent roster and subagent notifications
- other developer-only instructions

Use `CustomDeveloperInstructions` only for custom developer override text (for example config/app-server `developer_instructions` values).

For system-generated developer guidance (permissions, collaboration-mode wrappers, realtime notices, personality wrappers, model-switch notices), use typed developer fragments whose text comes from the neutral `developer_*_text` helpers in `codex_protocol::models`.

Use the contextual-user envelope for contextual state or runtime markers that should not count as real user turns:

- AGENTS / user instructions
- plugin instructions
- environment context
- skill instructions
- user shell command records
- turn-aborted markers

Contextual-user fragments must have stable detection because history parsing uses it to distinguish contextual state from real user intent.

Use `<environment_context>` only for environment facts derived from turn/session state (`TurnContext`) that may need turn-to-turn diffing. Today that includes `cwd`, `shell`, optional `current_date`, optional `timezone`, and optional network allow/deny domain summaries. Do not put developer policy/instructions or plugin/skill metadata into `<environment_context>`; those belong in their own typed fragments.

## Turn-backed fragments

If a fragment is derived from durable turn/session state and should be updated/reinjected by diff after history mutation, keep its extraction, diffing, and rendering logic together by implementing `TurnContextDiffFragment`.

`TurnContextDiffFragment` exposes one `build(...)` method that receives:

- current `TurnContext`
- optional `reference_context_item` (the turn context state already represented in model-visible history, if available)
- `TurnContextDiffParams` for shared runtime inputs (for example shell, previous-turn bridge state, exec-policy rendering context, and feature gating flags)

This is envelope-agnostic: both contextual-user state fragments and developer state-diff fragments use the same trait.

If a fragment is runtime-event/session-prefix only (for example subagent completion notification, turn-aborted marker, or user-shell-command marker), `ModelVisibleContextFragment` alone is enough.

That trait is the blessed path for fragments that need to:

- build full initial context when no reference context item is available
- compute turn diffs when a reference context item is available

`EnvironmentContext` is the canonical example. Future turn-backed contextual fragments should follow the same pattern instead of introducing one-off extraction or diff helpers.

## History behavior

Developer fragments do not need contextual-user marker matching because they are already separable by message role.

Contextual-user fragments do need marker matching because they share the `user` role with real user turns, and history parsing / truncation must avoid treating injected context as actual user input.
