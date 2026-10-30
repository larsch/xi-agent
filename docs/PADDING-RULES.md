# Output log display padding rules

The output log should be kept visually stable (within reason), when output blocks grow or shrink due to model or tool streaming. The visual truncation rules also trigger block size changes.

The goal is to keep the top streaming block's top line vertically stable until the end of the agent loop, unless pushed up by additional content.

A block that shrinks between frames should cause the padding space to increase accordingly.

A block that grows between frames should first consume padding, and then if possible consume the throbber line.

The throbber reappears when the stream is unexpectedly idle (chunks are arriving much slower than anticipated).

The model throbber always appears at the start of a model request.

When the throbber appears, it will also try to consume 1 line of padding, to prevent scrolling.

## Final validation test

`padding-tour` (test provider) walks every rule above in one run:

```sh
XI_DEBUG=1 xi --provider test --prompt padding-tour
```

| Step | Content, then tool call | Rules exercised |
|------|-------------------------|-----------------|
| 1 | `bash` printing 6 lines at 300 ms (and creating the fixture), then 2 s idle | growth scrolls; activity row returns with **no padding** |
| 2 | 2 s streaming thinking, ending on a line break | block grows and **shrinks** (trailing-line trim), leaving one padding row |
| 3 | `bash` printing 3 lines at 250 ms | first line spent on padding, next steals the activity row, then scrolls |
| 4 | 1.5 s streaming thinking, then `read_file` | shrink then growth into an eight-row body |
| 5 | `bash` 1 line at 300 ms, then 4 s idle | partial consumption; activity row returns onto the padding |
| 6 | batch: `bash` (8 lines at 400 ms) + `read_file` + `write_file` | asynchronous multi-tool growth in overlapping frames |
| 7 | `bash` sleeping 5 s | activity row with no content change |
| 8 | answer text | padding dropped at the end of the loop |

A reference run (`XI_DEBUG=1`, 490 frames) reports: 18 shrink events, 18
growths absorbed by padding, 3 throbber steals, **0** state-machine violations,
**0** downward moves of the content.

Note on shrinks: a tool block never gets shorter — previews and results render
at the same capped height (`head_lines`/`tail_lines`/`diff_lines`) and blocks
are appended rather than replaced. The shrink a provider can drive is the
trailing line of a streaming text/thinking block: finishing a line removes the
provisional row, so the block shrinks by one row (step 2 ends on a line break so
the padding is deterministic). Deeper shrinks follow the same rule and are
covered by the delta-script test below.

Two checks back it up:

- `ui::tests::padding_tour_deltas_keep_the_content_row_stable` replays the tour's
  delta script (growth, trim oscillation, padding consumption, a multi-row
  shrink, the multi-tool batch) through `PaddingState` and `log_window`,
  asserting that the content moves by exactly the leftover growth and never
  moves down.
- The debug log records the state machine per frame. `layout padding: delta=…
  padding=a->b leftover_growth=…` must satisfy `b == a - delta` (clamped at 0),
  and `render geometry: … first_line=… content_top_row=…` gives the row of
  content line 0 as `content_top_row - first_line`, which must never increase.
  The only two allowed increases are the activity row appearing when padding is
  0 and growth exactly consuming the last padding row, each by exactly one row.
