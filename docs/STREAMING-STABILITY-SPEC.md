# Streaming Stability Spec: anchor padding and the activity row

While the agent loop runs, the transcript must look calm: lines that are
already on screen keep their rows unless new content pushes them up. Two
mechanisms share that job and cannot be specified apart, because both spend the
same thing — **rows below the content**:

- **Anchor padding** — blank rows held below the streaming content. A block that
  shrinks hands its lost rows to the padding; a block that grows spends them.
- **The activity row** (throbber) — one row shown while the loop works silently.
  It keeps a row of its own, trading it against padding instead of moving
  content, and content growth may consume it.

This document is the single spec for that row budget. It replaces the former
padding-only rules page (`docs/PADDING-RULES.md`, now removed).

## Terms

| Term | Meaning |
|------|---------|
| Content | The rendered transcript lines, bottom-aligned in the output viewport. |
| Padding | Blank rows reserved *below* the content that absorb shrinkage. |
| Activity row | The throbber row, appended after the last content line of the transcript. |
| Reserved below | `padding + (activity row visible ? 1 : 0)`. |
| Visual delta | Net change in rendered content rows between two consecutive frames. |
| Hold-off | The quiet interval after visible output stopped, before the activity row may reappear. |

All geometry uses saturating subtraction: when reserved rows exceed the
available viewport, the content area has zero rows rather than underflowing.

## Invariants

1. **No downward drift.** Streaming never moves content down. The screen row of
   content line 0 (`content_top_row`) may decrease when content is pushed up,
   never increase.
2. **Shrink is absorbed.** A frame that renders fewer rows adds exactly those
   rows to the padding, so the content keeps its rows.
3. **Growth spends, in order.** Growth spends padding rows first, then the
   visible activity row (at most one row), and only what is left scrolls
   existing content up.
4. **The activity row keeps its own space.** It trades exactly one row with the
   padding counter: appearing takes a padding row, disappearing gives one back.
   The total reserved space is unchanged, so the content does not jump.
5. **Padding is state, not a derivation.** It is not recomputed from per-frame
   deltas. Only the explicit resets in [Teardown](#teardown) and the row trades
   above change it, so a frame that rebuilds the layout without a size change
   keeps it.
6. **Bottom alignment.** No trailing empty rows are rendered after the content
   (the activity row counts as content). Padding rows exist only as reserved
   space while the view follows the bottom.
7. **Stability is bounded.** Padding and the activity row trade only apply while
   the turn is active *and* the view is auto-scrolled to the bottom.

## Geometry

Given `inner_height` (viewport rows), `content_len`, `padding` and whether the
activity row is visible:

```
reserved_below = padding + (activity_row_visible ? 1 : 0)
content_rows   = min(content_len, inner_height - reserved_below)
first_line     = content_len - content_rows            // top of the visible window
top_padding    = inner_height - reserved_below - content_len   // filler above short content
bottom_padding = padding
```

`content_top_row = top_padding - first_line` is the observable screen row of
content line 0. It is reported verbatim in the debug log; do not subtract
`first_line` again when reading it.

The budget below the content is therefore *fixed* by the padding counter plus
at most one activity row. Everything in this spec is bookkeeping on those rows.

## Padding accounting

One accounting step per frame, in this order:

| Frame event | Padding | Activity row | Content |
|-------------|---------|--------------|---------|
| Visual delta `< 0` (shrink of N rows) | `+N` | unchanged | keeps its rows |
| Visual delta `> 0` with `N <= padding` | `-N` | unchanged | keeps its rows |
| Visual delta `> 0` with `N > padding`, row visible | `→ 0` | hidden | moves up `N - padding - 1` rows |
| Visual delta `> 0` with `N > padding`, row hidden | `→ 0` | hidden | moves up `N - padding` rows |
| Visual delta `0` / layout change | unchanged | may reappear after the hold-off | unchanged |
| Activity row appears, padding `> 0` | `-1` | visible | unchanged |
| Activity row appears, padding `== 0` | `0` | visible | moves up 1 row |
| Activity row disappears (not consumed by growth) | `+1` | hidden | unchanged |

Notes:

- Only the four operations of `PaddingState` exist: `absorb_shrink(rows)`,
  `consume_growth(rows) -> leftover`, `shrink_by(rows)` (give a row back
  without a rendered shrink) and `rows()`.
- The delta covers **every** block, not just the streaming one: a committing
  tool batch can shrink its block and that shrink needs padding like any other.
- The delta is computed only while the turn is active and the view is
  auto-scrolled. Otherwise the update is "layout changed, no content delta".
- A row that growth consumed in the same frame must not be returned to padding
  when the activity row disappears; the renderer tracks that with a
  consumed-row flag.
- A tool block never shrinks by itself — previews and results render at the same
  capped height (`head_lines` / `tail_lines` / `diff_lines`) and blocks are
  appended, not replaced. The shrinks a stream can drive are the streaming
  `edit_file` diff (several rows) and the trailing line of a streaming
  text/thinking block (one row).

## Activity row visibility

Visibility is evaluated on every frame from the turn state, the user's pending
input, and the hold-off:

| State | Activity row |
|-------|--------------|
| No active turn | hidden |
| The user owes an answer (pending `ask_user`, `ask_user` freeform mode, sudo approval, sudo password entry) | hidden — the ball is in the user's court |
| Login token refresh in progress | visible regardless of turn state |
| Turn active, visible output arrived recently | hidden — something is appearing on screen |
| Turn active, silent for the hold-off interval | visible — work is in progress |

### Timing

- Chunk intervals inside `[lower_bound_ms, upper_bound_ms]` feed an EWMA
  (`alpha`) of the expected chunk interval, seeded from
  `low_confidence_target_ms`. Defaults: `480` / `4000` / `2000` / `0.2`
  (`[throbber]` config section, `ThrobberConfig`). The estimate is re-seeded at
  every turn start and every tool-result continuation.
- The hold-off after hiding is
  `clamp(2 * expected_interval_ms, lower_bound_ms, upper_bound_ms)`.
- Any chunk restarts the hold-off while the row is hidden, so a slow but
  living stream keeps the row away; an unexpectedly idle stream brings it back.
- Switching between model request and local work selects a different animation
  (and starts or stops the runtime display); it does not change the visibility
  state by itself.

### Transitions that show the row immediately

- Agent turn start (`TurnStart`): the row is shown on the first frame of the
  request, before any output exists.
- Tool-result continuation (another model request inside the same user turn):
  the row is shown immediately and the hold-off is cleared, even if growth had
  hidden it a moment earlier.
- `ask_user` answer submitted: see [the `ask_user` handoff](#the-ask_user-handoff).

### Transitions that hide the row

- Visual growth that exceeds the available padding (it consumes the row).
- End of the agent turn.
- An open `ask_user` prompt, freeform answer mode, or sudo approval prompt.

## The `ask_user` handoff

The prompt and the throbber never share the screen, and the handoff between
them must not flicker or consume a padding row it did not earn:

1. While the prompt is open, the row is suppressed (the user owns the turn).
2. On submit, the tool result is rendered **optimistically in the same frame**
   that closes the prompt, and the model-request row becomes visible in that
   same frame, in anticipation of the continuation's `TurnStart`.
3. The previous visual baseline is discarded at two points — when the
   optimistic result is applied and again when the continuation request starts —
   so the tool-result layout change is not mistaken for model-output growth
   that would consume the newly visible row.
4. Existing anchor padding is retained through the handoff, and subsequent
   model output follows the normal growth rules.
5. The asynchronous `ToolCallEnd` that follows confirms the optimistic result;
   it must not visibly replace it.

## Teardown

Padding is dropped (set to zero, with the activity-row bookkeeping reset) by:

- any manual scroll (up or down) — the user takes control of the viewport, which
  turns auto-scroll off,
- a terminal width change (every block reflows, so row counts and the anchor no
  longer describe the layout),
- the end of the agent loop (`TurnEnd`),
- fold/unfold and full-output toggles (user-initiated layout changes),
- loading or resetting a session.

Padding is **not** dropped when an individual tool result arrives or a
continuation starts; those only reset the visual baseline, keeping the padding
that the layout already earned.

## Rendering

- The activity row is always a virtual line appended after the last content
  line, never a separate panel row: it counts as one content line for the
  geometry above, and it scrolls out of view with the rest of the log instead of
  staying pinned to the bottom.
- Animation: a braille triangle for model requests, a braille line-writer for
  local work. Local work also shows elapsed runtime once it exceeds 10 s
  (`12s`, `1m 5s`, `12m`), with coarse rounding for long waits.
- Styling comes from the theme (`status.idle` for model requests,
  `status.provider` dimmed and non-italic for local work).
- Login token refresh forces the row visible even outside an agent turn; the
  login panel's own layout does not take part in the row trades above.

## Debugging

All of this is observable in `~/.cache/xi` debug logs:

- `layout padding: … padding=a->b leftover_growth=…` — padding consumption per
  frame.
- `render geometry: … content_top_row=…` — the screen row of content line 0.
  Use it directly; the content must not move down as a result of growth.
- `throbber.trace` / `[THROB] visible → …` — activity row transitions with the
  reason (`activity_state`, `has_pending_ask`, `!is_active`).

## Validation

`padding-tour` (test provider) walks the rules above in one run:

```sh
XI_DEBUG=1 xi --provider test --prompt padding-tour
```

| Step | Content, then tool call | Rules exercised |
|------|-------------------------|-----------------|
| 1 | `bash` printing 6 lines at 300 ms (and writing the fixture), then 2 s idle | growth scrolls; activity row returns with **no padding** |
| 2 | `read_file` on the fixture | result growth consumes available padding/activity space |
| 3 | streaming `edit_file` whose diff collapses section by section | **multi-row shrink**: diff rows disappear and padding grows |
| 4 | `bash` printing 7 lines at 350 ms | growth spends padding row by row, content does not move |
| 5 | 2 s streaming thinking/tool sequence, ending on a line break | a streaming block trims its trailing row |
| 6 | `bash` printing 1 line, then 4 s idle | activity row returns onto the padding |
| 7 | batch: `bash` (8 lines at 400 ms) + `read_file` + `write_file` | asynchronous multi-tool growth in overlapping frames |
| 8 | `bash` sleeping 5 s, then printing 1 line | local work with an idle activity row |
| 9 | final textual summary | end-of-loop padding cleanup and final answer |

An illustrative debug run reported 16 shrinks (padding up to 5), 18 growths
absorbed by padding, and 3 activity rows consumed by growth. These counts are
not deterministic acceptance criteria: terminal size, scheduling, and frame
cadence affect timing-sensitive provider output.

Focused tests:

- `ui::tests::padding_tour_deltas_keep_the_content_row_stable` replays a
  representative delta script through `PaddingState` and `log_window`.
- `ui::tests::growth_consumes_visible_throbber_row_without_returning_padding`
  and `ui::tests::growth_with_no_padding_consumes_visible_throbber_row` exercise
  renderer growth with a visible activity row, asserting both screen position
  and remaining padding.
- `ui::tests::activity_row_keeps_its_own_space` and
  `ui::tests::throbber_takes_a_padding_row_when_it_appears` pin the independent
  row accounting.
- `ui::tests::ask_user_continuation_keeps_throbber_through_tool_result_layout_update`
  pins the handoff above.
- `ui::tests::anchor_padding_survives_a_frame_without_a_detected_shrink` pins
  invariant 5.
- `agent_turn_state::tests` pin the visibility state machine (start,
  continuation, growth-hides, hold-off expiry, padding-before-row consumption).

## Implementation map

| Concern | Location |
|---------|----------|
| Padding counter and log-view padding/throbber bookkeeping | `src/log_view_state.rs` |
| Per-frame delta accounting, log window geometry, draw-pass row trade | `src/ui.rs` (`build_log_layout_cached`, `log_window`, `draw`) |
| Turn lifecycle, hold-off timing, `activity_visible` | `src/agent_turn_state.rs` |
| Full visibility composition (asks, sudo, login refresh) | `src/app.rs` (`App::throbber_visible`) |
| Animation and styling of the row | `src/ui/status.rs` |
| Panel height of the control band (including the full-output hint row that shares the indicator slot) | `src/ui/layout.rs` |
