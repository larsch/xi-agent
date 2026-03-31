# Feature Specification: `show_user` Tool (Stacked Artifact Sets)

**Date:** 2026-03-30  
**Status:** Draft  
**Owner:** UI/Agent Runtime

---

## 1) Summary

Introduce a new built-in tool, `show_user`, that allows the agent to present interactive artifacts (text, URLs, files) to the user in a dedicated UI area without interrupting the agent loop or stealing input focus.

Each `show_user` call creates a new **artifact set** that is pushed onto a **stack**. The newest set is the **current set** and is visually emphasized. Users can navigate to older sets at any time.

This enables the pattern:
1. Agent shows context with `show_user`.
2. Agent asks a follow-up question (typically via `ask_user`) referencing the shown set/items.

---

## 2) Goals

- Give the agent a first-class way to present reviewable artifacts to users.
- Support multiple artifact types in one call (e.g., summary text + URLs + file paths).
- Keep artifact UI **non-blocking** and **non-modal**.
- Keep user typing/input workflow uninterrupted.
- Make artifacts easy to interact with:
  - URLs can be opened in browser.
  - Files can be opened in external viewer/editor.
  - Long content can be scrolled.
- Preserve history via a stack of artifact sets, with newest/current clearly visible.

---

## 3) Non-goals (v1)

- No `update_user_view` / `clear_user_view` tools (single-tool model only).
- No hard enforcement that `ask_user` must follow `show_user`.
- No read/ack tracking per item.
- No auto-opening URLs/files initiated by the agent.
- No persistence across app restarts (session-local only).

---

## 4) User Experience Requirements

### 4.1 Non-interference
- New artifact sets must not steal focus from input.
- Agent loop continues normally after `show_user` returns.
- UI may show a subtle “new artifacts” indicator/badge.

### 4.2 Focusability
- User can focus artifact pane via explicit command/keybinding.
- User can return to input focus quickly (e.g., `Esc`).

### 4.3 Stacked sets
- Every `show_user` call appends a new set on top of stack.
- Current set is visually distinct (e.g., header label: `Current`).
- Older sets remain navigable (collapsed/expandable or selectable list).

### 4.4 Artifact interactions
- URL items:
  - selectable/clickable
  - action: open in browser
  - action: copy URL
- File items:
  - show path
  - action: open externally
- Text/markdown items:
  - scrollable content area
  - text selection/copy support where terminal/UI allows

### 4.5 Multiple items per set
- A set can contain one or more items.
- Item list within a set is navigable by keyboard.
- Each item supports independent scrolling state.

---

## 5) Tool Contract

### 5.1 Tool name
`show_user`

### 5.2 Parameters (v1)

```json
{
  "title": "Optional title for the artifact set",
  "items": [
    {
      "type": "text",
      "title": "Summary",
      "content": "Short explanation for the user"
    },
    {
      "type": "url",
      "title": "Issue",
      "url": "https://example.com/issue/123"
    },
    {
      "type": "file",
      "title": "Generated Report",
      "path": "/absolute/or/relative/path"
    }
  ]
}
```

### 5.3 Item types (v1)
- `text` (plain text)
- `url`
- `file`

### 5.4 Return payload

```json
{
  "set_id": "set_000123",
  "item_ids": ["item_1", "item_2", "item_3"]
}
```

`set_id` is stable within session and can be referenced in subsequent assistant text/questions.

---

## 6) Runtime Semantics

- `show_user` is always non-blocking.
- Successful call appends a new set to in-memory stack.
- No mutation of previous sets in v1.
- If an item cannot be acted on (e.g., missing file), UI shows an inline error on action attempt; set remains valid.

---

## 7) UI State Model and Interaction Spec (v1)

### 7.1 UI state model

```text
artifacts_visibility: Hidden | Visible
focus_target: Input | Artifacts
artifacts_subfocus (when focus_target=Artifacts):
  SetList | ItemList | ItemDetail
selected_set_index: usize       // 0 = newest/current
selected_item_index: usize      // within selected set
item_scroll_offset: map<(set_id,item_id), offset>
unread_set_count: usize
```

### 7.2 State invariants
- If `artifacts_visibility = Hidden`, then `focus_target` must be `Input`.
- `selected_set_index = 0` always points to current/newest set.
- Arrival of a new set never changes `focus_target`.
- Switching sets resets `artifacts_subfocus` to `ItemList` (v1 simplification).

### 7.3 Core transitions

1. **On `show_user` success**
   - Push set at top of stack.
   - Set `selected_set_index = 0` **only if** currently viewing current set.
   - Increment `unread_set_count`.
   - Do not change focus.

2. **Focus artifacts**
   - Precondition: `artifacts_visibility = Visible` and stack non-empty.
   - Set `focus_target = Artifacts`.
   - Default `artifacts_subfocus = ItemList`.

3. **Return to input**
   - Set `focus_target = Input`.

4. **Hide artifacts**
   - Set `artifacts_visibility = Hidden`.
   - Force `focus_target = Input`.

5. **Show artifacts**
   - Set `artifacts_visibility = Visible`.
   - Keep current focus unchanged (Input remains default).

6. **Navigate set history**
   - In `SetList`/`ItemList`, move older/newer within bounds.
   - On set change, keep `selected_item_index` clamped in target set.

7. **Navigate items**
   - In `ItemList`, move item selection within selected set.
   - Enter `ItemDetail` to scroll/open actions.

### 7.4 UI regions

- **Header row:** `Artifacts` label, visibility hint, unread badge.
- **Set strip/list:** newest set first; current set marked `Current`.
- **Item list:** titles and type badges for selected set.
- **Detail pane:** content preview + actions (open/copy).

### 7.5 Keyboard interaction map (proposed defaults)

Global:
- `Ctrl+O`: toggle artifacts visibility (show/hide)
- `Ctrl+Shift+O`: focus artifacts pane (if visible)
- `Esc`: return focus to input (from artifacts)

When artifacts focused:
- `Tab` / `Shift+Tab`: cycle `SetList -> ItemList -> ItemDetail`
- `j` / `k` or `Down` / `Up`: move selection in current subfocus list
- `h` / `l` or `Left` / `Right`: older/newer set when in `SetList`
- `Enter`: open selected item action menu or primary action
- `o`: execute primary open action (URL in browser, file external)
- `c`: copy URL/path/content (based on item type)
- `PageDown` / `PageUp`: scroll detail content

### 7.6 Focus/visibility behavior rules
- Input focus remains active during agent execution unless user explicitly changes it.
- New sets produce visual notification but never modal interruption.
- Hiding artifacts preserves stack and selection state.
- Re-showing artifacts restores prior navigation state.

---

## 8) Agent Behavior Guidance

Recommended pattern for prompts/tool policy:
- When requesting user review, agent should call `show_user` first.
- Then ask follow-up question referencing set title and/or item titles.

Example:
- `show_user` with summary + URL + file
- `ask_user`: “Please review the current artifact set (‘Migration options’) and choose option A or B.”

---

## 9) Error Handling

- Invalid tool args: return tool error with schema/field details.
- Empty `items`: reject with validation error.
- Unsupported item type: reject with validation error.
- URL malformed: reject item at tool-validation time.
- File path may be accepted even if currently missing; open failure is reported when user triggers action.

---

## 10) Telemetry & Debug Logging

At debug level, log:
- set creation (`set_id`, item count, item types)
- artifact action attempts (open URL/file) and outcome
- pane focus/toggle events
- visibility/focus state transitions

Logs remain under existing tau debug location.

---

## 11) Accessibility & Safety

- No forced focus changes.
- Clear labeling of selected region (`SetList`, `ItemList`, `ItemDetail`) when focused.
- External open actions require explicit user action.
- Display full URL before open.

---

## 12) Acceptance Criteria

1. Agent can call `show_user` with multiple items and receive `set_id`.
2. UI shows newest set as current and keeps older sets navigable.
3. User can keep typing while new set appears (no focus steal).
4. User can focus artifacts, navigate sets/items, and return focus to input.
5. User can hide/show artifacts without losing stack/history.
6. URL item can be opened in browser by user action.
7. File item can be opened externally by user action.
8. Long text items are scrollable.
9. Agent can immediately continue and ask question after `show_user`.

---

## 13) Out-of-scope follow-ups (possible v2)

- Additional item types (markdown, diff, image, table)
- Set pinning, TTL, manual clear
- Search/filter across sets
- Ask-user linkage enforcement to specific `set_id`
- Persistent artifact history across sessions

---

## 14) Open Decisions

- Maximum retained set count (unbounded vs capped with pruning).
- Final keybinding choices (and conflicts with existing shortcuts).
- Default pane placement (right side vs bottom split).
- Whether file items store path only or optional snapshot metadata.
- Whether to allow optional “replace current set” mode in future.
