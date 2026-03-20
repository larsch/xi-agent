# UI layout engine test expansion plan (theme-safe)

**Date:** 2026-03-20  
**Status:** Implemented  
**Priority:** High

## Problem

`src/ui.rs` has a useful baseline of unit tests, but current coverage is mostly helper-level and does not fully protect layout behavior across state combinations (login, selection, completion popup, info bar, streaming states).

At the same time, upcoming theming/styling work means tests must avoid brittle coupling to specific colors or style spans.

## Goals

1. Increase confidence in the TUI layout engine’s structural behavior.
2. Keep tests stable under future theme/style changes.
3. Cover high-risk state interactions (login vs input, selection vs completion, narrow terminal widths).
4. Add a small number of coarse integration render tests for real `draw(...)` behavior.

## Non-goals

- Pixel-perfect or color/style-precise assertions.
- Snapshot tests tied to current ratatui style implementation details.
- Broad refactors unrelated to testability.

## Testing principles (locked)

1. **Assert semantics, not styling**: verify panel visibility, line counts, ordering, and visible text tokens.
2. **No color/style assertions**: avoid checking `Style`, `Color`, or modifiers except where semantic (ideally none).
3. **Prefer pure helpers**: test deterministic layout/content helpers before full render integration.
4. **Use coarse integration assertions**: for `draw(...)`, assert key text presence/absence and pane behavior only.

## Proposed work

### Phase 1 — Extract and test layout decisions (highest ROI)

Introduce a pure helper in `src/ui.rs` that computes panel heights used by `draw(...)`, e.g.:

- completion height
- selection header/items heights
- login header/content heights
- effective input height
- halfblock visibility height
- info bar height

Representative tests:

1. `layout_hides_input_and_halfblocks_when_login_active`
2. `layout_hides_completion_when_login_or_selection_active`
3. `layout_shows_resume_hint_row_when_applicable`
4. `layout_selection_item_rows_are_clamped_to_max_visible`
5. `layout_input_height_is_capped_at_40_percent_of_terminal`
6. `layout_info_bar_height_follows_toggle`
7. `layout_handles_very_small_terminal_heights_without_negative_results`

### Phase 2 — Expand existing pure line-builder tests

Target functions:

- `build_login_content_lines`
- `build_completion_lines`
- `build_selection_lines`
- `build_log_lines` and append helpers

Representative tests:

1. Login content:
   - `login_content_uses_device_flow_instruction`
   - `login_content_uses_redirect_flow_instruction`
   - `login_content_wraps_url_for_narrow_width`
   - `login_content_shows_code_row_only_when_present`
2. Completion popup:
   - `completion_rows_omit_separator_when_detail_empty`
   - `completion_loading_rows_render_without_detail_column`
   - `completion_label_column_alignment_is_structurally_consistent`
3. Selection menu:
   - `selection_window_respects_scroll_and_max_visible`
   - `selection_selected_row_contains_cursor_prefix`
   - `selection_loading_row_renders_label_only`
4. Log rendering:
   - `hidden_user_messages_are_not_rendered`
   - `streaming_empty_assistant_message_shows_cursor`
   - `stream_suffix_is_only_on_final_visible_chunk`
   - `user_message_renders_block_edges`
   - `read_file_tool_call_annotates_range_from_next_result_header`
   - `read_file_result_display_omits_header_line`
   - `tool_result_preview_truncates_with_ellipsis_after_limit`

### Phase 3 — Add minimal `draw(...)` integration tests

Use `ratatui::backend::TestBackend` and assert only coarse output invariants.

Representative tests:

1. `draw_login_mode_renders_auth_header_and_hides_input_textarea`
2. `draw_selection_mode_renders_title_and_visible_items`
3. `draw_info_bar_renders_provider_model_context_sections`

Assertions limited to:

- key text present/absent in rendered buffer
- expected section behavior (shown/hidden)

No assertions on colors/backgrounds/modifiers.

## Implementation order

1. Phase 1 (layout helper + tests)
2. Phase 2 (pure builder edge cases)
3. Phase 3 (2–3 coarse integration tests)
4. Run full quality gates

## Progress updates

### 2026-03-20 — Phase 1 completed

Completed in `src/ui.rs`:

- Extracted layout math into pure helper:
  - `PanelHeights`
  - `compute_panel_heights(...)`
- Updated `draw(...)` to consume `compute_panel_heights(...)` output.
- Added style-agnostic layout tests:
  - `layout_hides_input_and_halfblocks_when_login_active`
  - `layout_hides_completion_when_login_or_selection_active`
  - `layout_shows_resume_hint_row_when_applicable`
  - `layout_selection_item_rows_are_clamped_to_max_visible`
  - `layout_input_height_is_capped_at_40_percent_of_terminal`
  - `layout_info_bar_height_follows_toggle`
  - `layout_handles_small_terminals_without_underflow`
- Verification run:
  - `cargo test -q ui::tests -- --nocapture` (pass)

### 2026-03-20 — Phase 2 completed

Added style-agnostic pure-function tests in `src/ui.rs` for:

- `build_login_content_lines`:
  - `login_content_uses_device_flow_instruction`
  - `login_content_uses_redirect_flow_instruction`
  - `login_content_wraps_url_for_narrow_width`
  - `login_content_shows_code_row_only_when_present`
- `build_completion_lines`:
  - `completion_rows_omit_separator_when_detail_empty`
  - `completion_loading_rows_render_without_detail_column`
  - `completion_label_column_alignment_is_structurally_consistent`
- `build_selection_lines`:
  - `selection_window_respects_scroll_and_max_visible`
  - `selection_selected_row_contains_cursor_prefix`
  - `selection_loading_row_renders_label_only`
- `build_log_lines` behavior:
  - `hidden_user_messages_are_not_rendered`
  - `streaming_empty_assistant_message_shows_cursor`
  - `stream_suffix_is_only_on_final_visible_chunk`
  - `user_message_renders_block_edges`
  - `read_file_tool_call_annotates_range_from_next_result_header`
  - `read_file_result_display_omits_header_line`
  - `tool_result_preview_truncates_with_ellipsis_after_limit`

### 2026-03-20 — Phase 3 completed

Added coarse render integration tests (no style/color assertions), using `ratatui::backend::TestBackend`:

- `draw_login_mode_renders_auth_header_and_hides_input_textarea`
- `draw_selection_mode_renders_title_and_visible_items`
- `draw_info_bar_renders_provider_model_context_sections`

Also added test helper:

- `render_to_plain_lines(...)` for buffer-to-text assertions.

Verification run after Phases 2+3:

- `cargo test -q ui::tests -- --nocapture` (pass; 45 tests)

### 2026-03-20 — Final quality gates completed

Repository-wide checks executed successfully:

- `cargo fmt --all`
- `cargo clippy --all-targets --all-features -q`
- `cargo test -q` (pass; 146 tests)

## Affected files

Expected:

- `src/ui.rs`
- `docs/plans/2026-03-20-ui-layout-tests-plan.md`

Possible (if helper extraction suggests better placement):

- `src/ui/layout.rs` (new module, optional)

## Risks and mitigations

1. **Risk: test fragility from incidental spacing changes**  
   **Mitigation:** assert structural substrings and invariants rather than full-line exact matches unless spacing is semantic.

2. **Risk: overfitting tests to current symbol choices (icons/prefix glyphs)**  
   **Mitigation:** only assert symbol presence where symbol is semantic behavior (e.g., selection cursor marker), otherwise assert plain text structure.

3. **Risk: integration tests become hard to maintain**  
   **Mitigation:** keep integration suite small and coarse; push edge-case coverage into pure helper tests.

## Verification checklist

Required before marking implemented:

1. `cargo fmt`
2. `cargo clippy --all-targets --all-features`
3. `cargo test`
4. Confirm tests contain no direct style/color assertions in `src/ui.rs` test module.

## Acceptance criteria

1. New tests cover layout interactions between login, selection, completion, input, and info bar.
2. Existing + new UI tests pass reliably under unchanged styling and under trivial style constant changes.
3. At least 2 coarse `draw(...)` integration tests exist and are style-agnostic.
