# Plan: Theme configuration (theme.toml)

**Date:** 2026-05-31  
**Branch:** `theme-config`  
**Spec:** `docs/THEME.md`

---

## Goal

Consolidate all hardcoded theme/style values across the Xi codebase into a
central `Theme` struct loaded from `~/.config/xi/theme.toml`. Users can
override any value; missing values fall back to built-in defaults that
reproduce the current appearance exactly.

---

## Scope

**In scope:**
- `src/theme.rs` — all theme types and `Default` implementations
- Serde deserialization from TOML with per-field defaults
- Color parsing: `#rrggbb`, CSS named colors, terminal palette names
- `XiConfig` gains `theme_path: Option<PathBuf>`
- `App` holds `theme: Theme`, threaded into all render calls
- `--theme <path>` CLI flag
- All hardcoded `const Color` / inline `Color::Rgb(...)` in `ui/` and `markdown.rs` deleted and replaced
- Display thresholds (`MAX_MULTILINE_SHELL_COMMAND_LINES` etc.) moved to `XiConfig::display`
- `docs/CONFIG.md` updated with `[display]` section

**Out of scope:**
- Model/context thresholds (`DEFAULT_MAX_LINES`, `DEFAULT_MAX_BYTES`, etc.)
- Alternative built-in themes
- Runtime hot-reload

---

## Code-level done conditions

| Symbol / interface | Done condition |
|---|---|
| `src/theme.rs` | Exists; all types implemented; `Theme::default()` reproduces current appearance |
| `StyleSpec.to_ratatui_style()` | Converts all fields to `ratatui::Style` |
| Color deserializer | Accepts `#rrggbb`, CSS names, terminal palette names |
| `XiConfig.theme_path` | Loaded from `config.toml`; overridden by `--theme` flag |
| `App.theme` | Populated at startup; passed into all render functions |
| All `const Color` in `ui/*.rs`, `markdown.rs` | **Deleted**; replaced by `theme.*` references |
| `MAX_MULTILINE_SHELL_COMMAND_LINES`, `MAX_ONE_LINE_CHARS`, `SINGLE_LINE_MAX_BYTES` | Moved to `XiConfig::display`; old consts deleted |
| Dead code | Zero unused symbols left behind |

---

## Steps

1. **`src/theme.rs`** — define all types bottom-up; implement `Default` for each
2. **Color parsing** — custom serde deserializer for `Color`
3. **`XiConfig`** — add `theme_path` and `display: DisplayConfig`; update `docs/CONFIG.md`
4. **CLI flag** — add `--theme <path>` to `main.rs`
5. **Theme loading** — `Theme::load(path)` with field-by-field fallback to `Theme::default()`
6. **Thread into `App`** — add `theme: Theme` field; pass into render functions
7. **Migrate `ui/` modules** — delete consts, replace with `theme.*` (log, input, menu, status, info, login, pending)
8. **Migrate `markdown.rs`**
9. **Migrate `tool_presentation.rs`** — move display thresholds to `config.display`
10. **Verification pass** — grep checks + `just preflight`

---

## Affected files

- New: `src/theme.rs`
- Modified: `src/config.rs`, `src/main.rs`, `src/app.rs`, `src/ui.rs`,
  `src/ui/log.rs`, `src/ui/input.rs`, `src/ui/menu.rs`, `src/ui/status.rs`,
  `src/ui/info.rs`, `src/ui/login.rs`, `src/ui/pending.rs`,
  `src/markdown.rs`, `src/tool_presentation.rs`
- Docs: `docs/CONFIG.md`, `docs/THEME.md`

---

## Risks and assumptions

- `ratatui::Color` is not `Deserialize` — owned via newtype/wrapper
- CSS named color lookup — use `csscolorparser` crate or a `phf` static map
- Render fn signatures — most take `&App`; a few may need audit if `App` is not in scope
- Tests asserting old const values (e.g. `DEFAULT_MAX_LINES`) must be updated
- `Theme::default()` must exactly match current hardcoded values — visual regression risk

---

## Verification

```sh
# No raw color literals outside theme.rs
grep -r 'Color::Rgb\|Color::Cyan\|Color::Green\|Color::Red\|Color::Blue\|Color::Yellow\|Color::White\|Color::DarkGray\|Color::LightBlue\|Color::LightRed' src/ --include='*.rs' | grep -v theme.rs

# No old display threshold consts outside config.rs
grep -r 'MAX_MULTILINE\|MAX_ONE_LINE\|SINGLE_LINE_MAX' src/ --include='*.rs' | grep -v config.rs

# Full preflight
just preflight
```
