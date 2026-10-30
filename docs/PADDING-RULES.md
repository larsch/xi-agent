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
| 2 | `read_file` on the fixture | commit growth into an eight-row body (also consumes the activity row row) |
| 3 | streaming `edit_file` whose diff collapses section by section | **multi-row shrink**: five diff rows disappear, padding grows to 5 |
| 4 | `bash` printing 7 lines at 350 ms | growth spent on the padding row by row, content does not move |
| 5 | 2 s streaming thinking, ending on a line break | the other shrink source: a streaming block trims its trailing row |
| 6 | `bash` 1 line, then 4 s idle | activity row returns onto the padding |
| 7 | batch: `bash` (8 lines at 400 ms) + `read_file` + `write_file` | asynchronous multi-tool growth in overlapping frames |
| 8 | `bash` sleeping 5 s, then the answer | idle activity row, padding dropped at the end of the loop |

A reference run (`XI_DEBUG=1`, 512 frames) reports: 16 shrinks (padding up to 5),
18 growths absorbed by padding, 3 activity rows consumed by growth, **0**
state-machine violations, **0** downward moves of the content.

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

Two checks back it up:

- `ui::tests::padding_tour_deltas_keep_the_content_row_stable` replays the tour's
  delta script (growth, trims, padding consumption, a multi-row shrink, the
  multi-tool batch) through `PaddingState` and `log_window`, asserting that the
  content moves by exactly the leftover growth and never moves down.
  `ui::tests::activity_row_keeps_its_own_space` pins the row accounting above.
- The debug log records the state machine per frame. `layout padding: delta=…
  padding=a->b leftover_growth=…` must satisfy `b == a - delta` (clamped at 0),
  and `render geometry: … first_line=… content_top_row=…` gives the row of
  content line 0 as `content_top_row - first_line`, which must never increase.
  The only allowed increase is growth that lands exactly on the last padding
  row; the activity row appearing with no padding available moves the content up
  one row, which the rules accept ("try").
