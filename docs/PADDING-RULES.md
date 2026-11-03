# Output log display padding rules

The output log should be kept visually stable (within reason), when output blocks grow or shrink due to model or tool streaming. The visual truncation rules also trigger block size changes.

The goal is to keep the top streaming block's top line vertically stable until the end of the agent loop, unless pushed up by additional content.

A block that shrinks between frames should cause the padding space to increase accordingly.

A block that grows between frames should first consume padding, and then if possible consume the throbber line.

The throbber reappears when the stream is unexpectedly idle (chunks are arriving much slower than anticipated).

The model throbber always appears at the start of a model request.

When the throbber appears, it will also try to consume 1 line of padding, to prevent scrolling.

## Final validation test

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

Notes:

- The activity row keeps **its own** row. Appearing takes one padding row out of
  the counter; disappearing gives one back. That is what "tries to consume 1 line
  of padding" means, and it is why the row can also be consumed by growth
  (step 4) without the content moving.
- A tool block never shrinks by itself — previews and results render at the same
  capped height (`head_lines`/`tail_lines`/`diff_lines`) and blocks are appended,
  not replaced. The shrinks a stream can drive are the streaming `edit_file` diff
  (step 3, several rows) and the trailing line of a streaming text/thinking block
  (step 5, one row).

The focused tests cover the accounting primitives and renderer transitions:

- `ui::tests::padding_tour_deltas_keep_the_content_row_stable` replays a
  representative delta script through `PaddingState` and `log_window`.
- `ui::tests::growth_consumes_visible_throbber_row_without_returning_padding`
  and `growth_with_no_padding_consumes_visible_throbber_row` exercise renderer
  growth with a visible activity row, asserting both screen position and the
  remaining padding.
- `ui::tests::activity_row_keeps_its_own_space` pins independent row accounting.
- The debug log records the state machine per frame. `layout padding: …
  padding=a->b leftover_growth=…` reports padding consumption, while
  `render geometry: … content_top_row=…` already gives the screen row of
  content line 0. Use `content_top_row` directly; do not subtract `first_line`
  again. The content should not move down as a result of growth. An activity row
  appearing without padding consumes its own row and moves content up when no
  padding is available.
