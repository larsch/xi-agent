//! Theme configuration for Xi's terminal UI.
//!
//! All visual style choices — colors, symbols, padding, margins — are
//! expressed as a [`Theme`] struct that is loaded from
//! `~/.config/xi/theme.toml` (or a path supplied via `--theme`).
//!
//! Every field is optional in the TOML file; missing values fall back to
//! [`Theme::default()`], which reproduces Xi's built-in appearance.

use std::{collections::HashMap, fs, path::Path};

use anyhow::Context;
use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Deserializer, Serialize};

// ── Color deserialization ─────────────────────────────────────────────────────

/// Deserialize a ratatui [`Color`] from one of:
/// - `"#rrggbb"` hex string
/// - CSS/HTML named color (e.g. `"cornflowerblue"`)
/// - Terminal palette name: `"black"`, `"red"`, `"green"`, `"yellow"`,
///   `"blue"`, `"magenta"`, `"cyan"`, `"white"`, and `"bright-<name>"` variants
fn deserialize_color<'de, D>(deserializer: D) -> Result<Option<Color>, D::Error>
where
    D: Deserializer<'de>,
{
    let s: Option<String> = Option::deserialize(deserializer)?;
    let Some(s) = s else { return Ok(None) };
    parse_color(&s).map(Some).map_err(serde::de::Error::custom)
}

fn serialize_color<S>(color: &Option<Color>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match color {
        None => serializer.serialize_none(),
        Some(Color::Rgb(r, g, b)) => serializer.serialize_some(&format!("#{r:02x}{g:02x}{b:02x}")),
        Some(Color::Black) => serializer.serialize_some("black"),
        Some(Color::Red) => serializer.serialize_some("red"),
        Some(Color::Green) => serializer.serialize_some("green"),
        Some(Color::Yellow) => serializer.serialize_some("yellow"),
        Some(Color::Blue) => serializer.serialize_some("blue"),
        Some(Color::Magenta) => serializer.serialize_some("magenta"),
        Some(Color::Cyan) => serializer.serialize_some("cyan"),
        Some(Color::White) => serializer.serialize_some("white"),
        Some(Color::DarkGray) => serializer.serialize_some("bright-black"),
        Some(Color::LightRed) => serializer.serialize_some("bright-red"),
        Some(Color::LightGreen) => serializer.serialize_some("bright-green"),
        Some(Color::LightYellow) => serializer.serialize_some("bright-yellow"),
        Some(Color::LightBlue) => serializer.serialize_some("bright-blue"),
        Some(Color::LightMagenta) => serializer.serialize_some("bright-magenta"),
        Some(Color::LightCyan) => serializer.serialize_some("bright-cyan"),
        Some(Color::Gray) => serializer.serialize_some("bright-white"),
        Some(other) => serializer.serialize_some(&format!("{other:?}")),
    }
}

/// Parse a color string into a ratatui [`Color`].
pub fn parse_color(s: &str) -> anyhow::Result<Color> {
    // Terminal palette names
    match s {
        "black" => return Ok(Color::Black),
        "red" => return Ok(Color::Red),
        "green" => return Ok(Color::Green),
        "yellow" => return Ok(Color::Yellow),
        "blue" => return Ok(Color::Blue),
        "magenta" => return Ok(Color::Magenta),
        "cyan" => return Ok(Color::Cyan),
        "white" => return Ok(Color::White),
        "bright-black" => return Ok(Color::DarkGray),
        "bright-red" => return Ok(Color::LightRed),
        "bright-green" => return Ok(Color::LightGreen),
        "bright-yellow" => return Ok(Color::LightYellow),
        "bright-blue" => return Ok(Color::LightBlue),
        "bright-magenta" => return Ok(Color::LightMagenta),
        "bright-cyan" => return Ok(Color::LightCyan),
        "bright-white" => return Ok(Color::Gray),
        _ => {}
    }

    // #rrggbb hex or CSS named color via csscolorparser
    let c = csscolorparser::parse(s).with_context(|| format!("unknown color: {s:?}"))?;
    let [r, g, b, _a] = c.to_rgba8();
    Ok(Color::Rgb(r, g, b))
}

// ── StyleSpec ─────────────────────────────────────────────────────────────────

/// A flat set of optional visual attributes that map to a ratatui [`Style`].
///
/// All fields are optional — unset fields leave the corresponding ratatui
/// attribute unchanged. Set `visible = false` to hide the element entirely.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct StyleSpec {
    #[serde(
        default,
        deserialize_with = "deserialize_color",
        serialize_with = "serialize_color"
    )]
    pub fg: Option<Color>,
    #[serde(
        default,
        deserialize_with = "deserialize_color",
        serialize_with = "serialize_color"
    )]
    pub bg: Option<Color>,
    #[serde(default)]
    pub bold: Option<bool>,
    #[serde(default)]
    pub dim: Option<bool>,
    #[serde(default)]
    pub italic: Option<bool>,
    #[serde(default)]
    pub underline: Option<bool>,
    /// When `false`, the element is not rendered at all. Defaults to `true`.
    #[serde(default)]
    pub visible: Option<bool>,
}

// Methods are public API; callers added as UI migration completes.
#[allow(dead_code)]
impl StyleSpec {
    /// Returns `true` unless `visible` is explicitly set to `false`.
    pub fn is_visible(&self) -> bool {
        self.visible.unwrap_or(true)
    }

    /// Convert to a ratatui [`Style`], applying only the set attributes.
    pub fn to_ratatui_style(&self) -> Style {
        let mut s = Style::default();
        if let Some(fg) = self.fg {
            s = s.fg(fg);
        }
        if let Some(bg) = self.bg {
            s = s.bg(bg);
        }
        if self.bold == Some(true) {
            s = s.add_modifier(Modifier::BOLD);
        }
        if self.dim == Some(true) {
            s = s.add_modifier(Modifier::DIM);
        }
        if self.italic == Some(true) {
            s = s.add_modifier(Modifier::ITALIC);
        }
        if self.underline == Some(true) {
            s = s.add_modifier(Modifier::UNDERLINED);
        }
        s
    }
}

// ── PrefixStyle ───────────────────────────────────────────────────────────────

/// Style for a leading label or icon rendered before content.
///
/// Used by tool entries, assistant messages, input field prompts, and the
/// log edge marker. The `text` field is the actual string displayed;
/// the remaining fields style that string.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PrefixStyle {
    pub text: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_color",
        serialize_with = "serialize_color"
    )]
    pub fg: Option<Color>,
    #[serde(
        default,
        deserialize_with = "deserialize_color",
        serialize_with = "serialize_color"
    )]
    pub bg: Option<Color>,
    #[serde(default)]
    pub bold: Option<bool>,
    #[serde(default)]
    pub dim: Option<bool>,
    #[serde(default)]
    pub italic: Option<bool>,
    #[serde(default)]
    pub underline: Option<bool>,
    /// When `false`, the prefix is not rendered at all.
    #[serde(default)]
    pub visible: Option<bool>,
}

// Methods are public API; callers added as UI migration completes.
#[allow(dead_code)]
impl PrefixStyle {
    pub fn is_visible(&self) -> bool {
        self.visible.unwrap_or(true)
    }

    pub fn to_ratatui_style(&self) -> Style {
        let mut s = Style::default();
        if let Some(fg) = self.fg {
            s = s.fg(fg);
        }
        if let Some(bg) = self.bg {
            s = s.bg(bg);
        }
        if self.bold == Some(true) {
            s = s.add_modifier(Modifier::BOLD);
        }
        if self.dim == Some(true) {
            s = s.add_modifier(Modifier::DIM);
        }
        if self.italic == Some(true) {
            s = s.add_modifier(Modifier::ITALIC);
        }
        if self.underline == Some(true) {
            s = s.add_modifier(Modifier::UNDERLINED);
        }
        s
    }
}

// ── PaddingStyle ──────────────────────────────────────────────────────────────

/// Rendering style for top and bottom padding rows.
///
/// Left and right padding is always blank; `PaddingStyle` has no effect on
/// horizontal sides.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PaddingStyle {
    /// Padding rows inherit the surrounding background (default).
    #[default]
    Blank,
    /// Uses `▀`/`▄` half-block characters to blend the element background
    /// into the surrounding color — a smooth one-row transition at each edge.
    HalfBlock,
    /// Padding rows are filled with the element's own background color.
    Solid,
}

// ── PaddingSpec ───────────────────────────────────────────────────────────────

/// Padding inside a styled region.
///
/// All size fields accept integers (cells/rows). Resolution order:
/// individual side > axis (`_x`/`_y`) > global (`padding`).
///
/// `padding_style` applies to top and bottom rows only; left/right are always
/// blank regardless of this setting.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PaddingSpec {
    /// Shorthand: sets all four sides.
    pub padding: Option<u16>,
    /// Sets left and right (always blank).
    pub padding_x: Option<u16>,
    /// Sets top and bottom.
    pub padding_y: Option<u16>,
    pub padding_top: Option<u16>,
    pub padding_bottom: Option<u16>,
    pub padding_left: Option<u16>,
    pub padding_right: Option<u16>,
    /// Rendering style for top and bottom padding rows only.
    pub padding_style: Option<PaddingStyle>,
}

// Methods are public API; callers added as UI migration completes.
#[allow(dead_code)]
impl PaddingSpec {
    /// Resolve the four sides to `(top, right, bottom, left)`.
    pub fn resolve(&self) -> (u16, u16, u16, u16) {
        let base = self.padding.unwrap_or(0);
        let x = self.padding_x.unwrap_or(base);
        let y = self.padding_y.unwrap_or(base);
        let top = self.padding_top.unwrap_or(y);
        let bottom = self.padding_bottom.unwrap_or(y);
        let left = self.padding_left.unwrap_or(x);
        let right = self.padding_right.unwrap_or(x);
        (top, right, bottom, left)
    }
}

// ── MarginSpec ────────────────────────────────────────────────────────────────

/// Margin outside a styled region (space between adjacent blocks).
///
/// Margins are always blank. Adjacent blocks have their margins collapsed:
/// the gap is `max(block_a.margin_bottom, block_b.margin_top)`.
///
/// All size fields accept integers (cells/rows). Resolution order:
/// individual side > axis (`_x`/`_y`) > global (`margin`).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct MarginSpec {
    /// Shorthand: sets all four sides.
    pub margin: Option<u16>,
    /// Sets left and right.
    pub margin_x: Option<u16>,
    /// Sets top and bottom.
    pub margin_y: Option<u16>,
    pub margin_top: Option<u16>,
    pub margin_bottom: Option<u16>,
    pub margin_left: Option<u16>,
    pub margin_right: Option<u16>,
}

// Methods are public API; callers added as UI migration completes.
#[allow(dead_code)]
impl MarginSpec {
    /// Resolve the four sides to `(top, right, bottom, left)`.
    pub fn resolve(&self) -> (u16, u16, u16, u16) {
        let base = self.margin.unwrap_or(0);
        let x = self.margin_x.unwrap_or(base);
        let y = self.margin_y.unwrap_or(base);
        let top = self.margin_top.unwrap_or(y);
        let bottom = self.margin_bottom.unwrap_or(y);
        let left = self.margin_left.unwrap_or(x);
        let right = self.margin_right.unwrap_or(x);
        (top, right, bottom, left)
    }
}

// ── Tool theme ────────────────────────────────────────────────────────────────

/// Placeholder counter sub-style (e.g. line counts shown while streaming).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PlaceholderCounterStyle {
    #[serde(
        default,
        deserialize_with = "deserialize_color",
        serialize_with = "serialize_color"
    )]
    pub fg: Option<Color>,
    #[serde(
        default,
        deserialize_with = "deserialize_color",
        serialize_with = "serialize_color"
    )]
    pub bg: Option<Color>,
    #[serde(default)]
    pub bold: Option<bool>,
    #[serde(default)]
    pub dim: Option<bool>,
    #[serde(default)]
    pub italic: Option<bool>,
    #[serde(default)]
    pub underline: Option<bool>,
    /// When `false`, this counter is not shown.
    #[serde(default)]
    pub visible: Option<bool>,
}

// Methods are public API; callers added as UI migration completes.
#[allow(dead_code)]
impl PlaceholderCounterStyle {
    pub fn is_visible(&self) -> bool {
        self.visible.unwrap_or(true)
    }

    pub fn to_ratatui_style(&self) -> Style {
        let mut s = Style::default();
        if let Some(fg) = self.fg {
            s = s.fg(fg);
        }
        if let Some(bg) = self.bg {
            s = s.bg(bg);
        }
        if self.bold == Some(true) {
            s = s.add_modifier(Modifier::BOLD);
        }
        if self.dim == Some(true) {
            s = s.add_modifier(Modifier::DIM);
        }
        if self.italic == Some(true) {
            s = s.add_modifier(Modifier::ITALIC);
        }
        if self.underline == Some(true) {
            s = s.add_modifier(Modifier::UNDERLINED);
        }
        s
    }
}

/// Placeholder style shown while a tool's argument is still streaming.
///
/// `text` overrides the default label (e.g. `"running…"`). Other fields
/// style the label text. Counter sub-keys (`lines`, `common_lines`,
/// `changed_lines`) style the progressive counters shown alongside the label.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PlaceholderStyle {
    /// Override the default pending label text.
    pub text: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_color",
        serialize_with = "serialize_color"
    )]
    pub fg: Option<Color>,
    #[serde(
        default,
        deserialize_with = "deserialize_color",
        serialize_with = "serialize_color"
    )]
    pub bg: Option<Color>,
    #[serde(default)]
    pub bold: Option<bool>,
    #[serde(default)]
    pub dim: Option<bool>,
    #[serde(default)]
    pub italic: Option<bool>,
    #[serde(default)]
    pub underline: Option<bool>,
    /// When `false`, no placeholder is shown at all.
    #[serde(default)]
    pub visible: Option<bool>,
    /// Style for running line-count shown by bash/exec/read_file/find_files.
    #[serde(default)]
    pub lines: PlaceholderCounterStyle,
    /// Style for unchanged-line count shown by edit_file.
    #[serde(default)]
    pub common_lines: PlaceholderCounterStyle,
    /// Style for changed-line count shown by edit_file.
    #[serde(default)]
    pub changed_lines: PlaceholderCounterStyle,
}

// Methods are public API; callers added as UI migration completes.
#[allow(dead_code)]
impl PlaceholderStyle {
    pub fn is_visible(&self) -> bool {
        self.visible.unwrap_or(true)
    }

    pub fn to_ratatui_style(&self) -> Style {
        let mut s = Style::default();
        if let Some(fg) = self.fg {
            s = s.fg(fg);
        }
        if let Some(bg) = self.bg {
            s = s.bg(bg);
        }
        if self.bold == Some(true) {
            s = s.add_modifier(Modifier::BOLD);
        }
        if self.dim == Some(true) {
            s = s.add_modifier(Modifier::DIM);
        }
        if self.italic == Some(true) {
            s = s.add_modifier(Modifier::ITALIC);
        }
        if self.underline == Some(true) {
            s = s.add_modifier(Modifier::UNDERLINED);
        }
        s
    }
}

/// Per-tool presentation style.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ToolTheme {
    /// Icon/label prefix rendered once the argument is known.
    #[serde(default)]
    pub prefix: PrefixStyle,
    /// Style of the tool call headline line (prefix + command/filename).
    /// Defaults to the same color as `body` if unset.
    #[serde(default)]
    pub headline: StyleSpec,
    /// Style of the tool result content area.
    #[serde(default)]
    pub body: StyleSpec,
    /// Style shown while the argument is still streaming.
    #[serde(default)]
    pub placeholder: PlaceholderStyle,
}

// ── Section sub-structs ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogUserStyle {
    #[serde(
        default,
        deserialize_with = "deserialize_color",
        serialize_with = "serialize_color"
    )]
    pub bg: Option<Color>,
    #[serde(flatten)]
    pub padding: PaddingSpec,
    #[serde(flatten)]
    pub margin: MarginSpec,
}

impl Default for LogUserStyle {
    fn default() -> Self {
        Self {
            bg: Some(Color::Rgb(50, 50, 64)),
            padding: PaddingSpec::default(),
            margin: MarginSpec::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogAskUserStyle {
    #[serde(
        default,
        deserialize_with = "deserialize_color",
        serialize_with = "serialize_color"
    )]
    pub bg: Option<Color>,
    #[serde(flatten)]
    pub padding: PaddingSpec,
    #[serde(flatten)]
    pub margin: MarginSpec,
}

impl Default for LogAskUserStyle {
    fn default() -> Self {
        Self {
            bg: Some(Color::Rgb(27, 71, 31)),
            padding: PaddingSpec::default(),
            margin: MarginSpec::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogDiffStyle {
    #[serde(default)]
    pub added: StyleSpec,
    #[serde(default)]
    pub removed: StyleSpec,
    #[serde(default)]
    pub unchanged: StyleSpec,
}

impl Default for LogDiffStyle {
    fn default() -> Self {
        Self {
            added: StyleSpec {
                fg: Some(Color::LightGreen),
                ..Default::default()
            },
            removed: StyleSpec {
                fg: Some(Color::LightRed),
                ..Default::default()
            },
            unchanged: StyleSpec {
                fg: Some(Color::DarkGray),
                ..Default::default()
            },
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct LogAssistantPhaseStyle {
    #[serde(default)]
    pub prefix: PrefixStyle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogAssistantStyle {
    #[serde(default)]
    pub provisional: LogAssistantPhaseStyle,
    #[serde(default)]
    pub r#final: LogAssistantPhaseStyle,
    #[serde(default)]
    pub thinking: StyleSpec,
}

impl Default for LogAssistantStyle {
    fn default() -> Self {
        Self {
            provisional: LogAssistantPhaseStyle {
                prefix: PrefixStyle {
                    text: Some("💭 ".to_string()),
                    ..Default::default()
                },
            },
            r#final: LogAssistantPhaseStyle {
                prefix: PrefixStyle {
                    text: Some("💬 ".to_string()),
                    ..Default::default()
                },
            },
            thinking: StyleSpec {
                dim: Some(true),
                ..Default::default()
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogSteeringStyle {
    #[serde(
        default,
        deserialize_with = "deserialize_color",
        serialize_with = "serialize_color"
    )]
    pub fg: Option<Color>,
    #[serde(
        default,
        deserialize_with = "deserialize_color",
        serialize_with = "serialize_color"
    )]
    pub bg: Option<Color>,
    #[serde(default)]
    pub bold: Option<bool>,
    #[serde(default)]
    pub dim: Option<bool>,
    #[serde(default)]
    pub italic: Option<bool>,
    #[serde(default)]
    pub prefix: PrefixStyle,
}

impl Default for LogSteeringStyle {
    fn default() -> Self {
        Self {
            fg: Some(Color::Rgb(200, 200, 120)),
            bg: None,
            bold: None,
            dim: None,
            italic: Some(true),
            prefix: PrefixStyle {
                text: Some("🕹️ ".to_string()),
                fg: Some(Color::Rgb(200, 200, 120)),
                ..Default::default()
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogTheme {
    #[serde(default)]
    pub user: LogUserStyle,
    #[serde(default)]
    pub ask_user: LogAskUserStyle,
    #[serde(default)]
    pub edge_marker: PrefixStyle,
    #[serde(default)]
    pub assistant: LogAssistantStyle,
    #[serde(default)]
    pub steering: LogSteeringStyle,
    #[serde(default)]
    pub diff: LogDiffStyle,
}

impl Default for LogTheme {
    fn default() -> Self {
        Self {
            user: LogUserStyle::default(),
            ask_user: LogAskUserStyle::default(),
            edge_marker: PrefixStyle {
                text: Some("│".to_string()),
                fg: Some(Color::Rgb(100, 100, 120)),
                ..Default::default()
            },
            assistant: LogAssistantStyle::default(),
            steering: LogSteeringStyle::default(),
            diff: LogDiffStyle::default(),
        }
    }
}

// ── Input theme ───────────────────────────────────────────────────────────────

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct InputFieldStyle {
    #[serde(
        default,
        deserialize_with = "deserialize_color",
        serialize_with = "serialize_color"
    )]
    pub bg: Option<Color>,
    #[serde(
        default,
        deserialize_with = "deserialize_color",
        serialize_with = "serialize_color"
    )]
    pub fg: Option<Color>,
    #[serde(default)]
    pub prefix: PrefixStyle,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct InputModeStyle {
    #[serde(
        default,
        deserialize_with = "deserialize_color",
        serialize_with = "serialize_color"
    )]
    pub bg: Option<Color>,
    #[serde(flatten)]
    pub padding: PaddingSpec,
    #[serde(flatten)]
    pub margin: MarginSpec,
    #[serde(default)]
    pub field: InputFieldStyle,
    #[serde(default)]
    pub placeholder: StyleSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputTheme {
    #[serde(default)]
    pub normal: InputModeStyle,
    #[serde(default)]
    pub shell: InputModeStyle,
    #[serde(default)]
    pub ask_user: InputModeStyle,
    /// Style for resolved @file/@url tokens in the input.
    #[serde(default)]
    pub at_file: StyleSpec,
}

impl Default for InputTheme {
    fn default() -> Self {
        Self {
            normal: InputModeStyle {
                bg: Some(Color::Rgb(30, 30, 40)),
                ..Default::default()
            },
            shell: InputModeStyle {
                bg: Some(Color::Rgb(24, 34, 32)),
                ..Default::default()
            },
            ask_user: InputModeStyle {
                bg: Some(Color::Rgb(50, 30, 15)),
                ..Default::default()
            },
            at_file: StyleSpec {
                fg: Some(Color::Cyan),
                ..Default::default()
            },
        }
    }
}

// ── Menu theme ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionTheme {
    #[serde(
        default,
        deserialize_with = "deserialize_color",
        serialize_with = "serialize_color"
    )]
    pub bg: Option<Color>,
    #[serde(default)]
    pub selected: StyleSpec,
    #[serde(default)]
    pub cmd: StyleSpec,
    #[serde(default)]
    pub desc: StyleSpec,
    #[serde(default)]
    pub r#match: StyleSpec,
}

impl Default for CompletionTheme {
    fn default() -> Self {
        Self {
            bg: Some(Color::Rgb(22, 22, 38)),
            selected: StyleSpec {
                bg: Some(Color::Rgb(55, 55, 100)),
                ..Default::default()
            },
            cmd: StyleSpec {
                fg: Some(Color::Rgb(120, 200, 255)),
                ..Default::default()
            },
            desc: StyleSpec {
                fg: Some(Color::Rgb(140, 140, 160)),
                ..Default::default()
            },
            r#match: StyleSpec {
                fg: Some(Color::Rgb(255, 220, 80)),
                bold: Some(true),
                ..Default::default()
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionTheme {
    #[serde(
        default,
        deserialize_with = "deserialize_color",
        serialize_with = "serialize_color"
    )]
    pub bg: Option<Color>,
    #[serde(default)]
    pub selected: StyleSpec,
    #[serde(default)]
    pub item: StyleSpec,
    /// Header bar background and style.
    #[serde(default)]
    pub header: StyleSpec,
}

impl Default for SelectionTheme {
    fn default() -> Self {
        Self {
            bg: Some(Color::Rgb(18, 35, 18)),
            selected: StyleSpec {
                bg: Some(Color::Rgb(30, 90, 30)),
                ..Default::default()
            },
            item: StyleSpec {
                fg: Some(Color::Rgb(140, 220, 140)),
                ..Default::default()
            },
            header: StyleSpec {
                bg: Some(Color::Rgb(20, 45, 20)),
                ..Default::default()
            },
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct MenuTheme {
    #[serde(default)]
    pub completion: CompletionTheme,
    #[serde(default)]
    pub selection: SelectionTheme,
}

// ── Status theme ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusTheme {
    #[serde(default)]
    pub provider: StyleSpec,
    #[serde(default)]
    pub model: StyleSpec,
    #[serde(default)]
    pub cost: StyleSpec,
    #[serde(default)]
    pub idle: StyleSpec,
}

impl Default for StatusTheme {
    fn default() -> Self {
        Self {
            provider: StyleSpec {
                fg: Some(Color::Rgb(160, 200, 255)),
                bold: Some(true),
                ..Default::default()
            },
            model: StyleSpec {
                fg: Some(Color::Rgb(100, 140, 100)),
                italic: Some(true),
                ..Default::default()
            },
            cost: StyleSpec {
                fg: Some(Color::Rgb(220, 180, 80)),
                bold: Some(true),
                ..Default::default()
            },
            idle: StyleSpec {
                fg: Some(Color::Rgb(160, 160, 180)),
                ..Default::default()
            },
        }
    }
}

// ── Info theme ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InfoTheme {
    #[serde(
        default,
        deserialize_with = "deserialize_color",
        serialize_with = "serialize_color"
    )]
    pub bg: Option<Color>,
    #[serde(default)]
    pub separator: StyleSpec,
    #[serde(default)]
    pub key: StyleSpec,
    #[serde(default)]
    pub value: StyleSpec,
    #[serde(default)]
    pub hint: StyleSpec,
}

impl Default for InfoTheme {
    fn default() -> Self {
        Self {
            bg: Some(Color::Rgb(20, 20, 30)),
            separator: StyleSpec {
                fg: Some(Color::Rgb(60, 60, 80)),
                ..Default::default()
            },
            key: StyleSpec {
                fg: Some(Color::Rgb(100, 100, 130)),
                ..Default::default()
            },
            value: StyleSpec {
                fg: Some(Color::Rgb(180, 200, 255)),
                ..Default::default()
            },
            hint: StyleSpec {
                fg: Some(Color::Rgb(60, 60, 80)),
                italic: Some(true),
                ..Default::default()
            },
        }
    }
}

// ── Login theme ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginTheme {
    #[serde(default)]
    pub header: StyleSpec,
    #[serde(default)]
    pub content: StyleSpec,
    /// Instruction text style.
    #[serde(default)]
    pub instruction: StyleSpec,
    /// Status text style.
    #[serde(default)]
    pub status: StyleSpec,
    /// "URL:" label style.
    #[serde(default)]
    pub url_key: StyleSpec,
    /// URL value style.
    #[serde(default)]
    pub url_val: StyleSpec,
    /// "Code:" label style.
    #[serde(default)]
    pub code_key: StyleSpec,
    /// Code value style.
    #[serde(default)]
    pub code_val: StyleSpec,
}

impl Default for LoginTheme {
    fn default() -> Self {
        Self {
            header: StyleSpec {
                bg: Some(Color::Rgb(20, 30, 60)),
                ..Default::default()
            },
            content: StyleSpec {
                bg: Some(Color::Rgb(15, 22, 48)),
                ..Default::default()
            },
            instruction: StyleSpec {
                fg: Some(Color::Rgb(180, 180, 200)),
                ..Default::default()
            },
            status: StyleSpec {
                fg: Some(Color::White),
                ..Default::default()
            },
            url_key: StyleSpec {
                fg: Some(Color::Rgb(120, 200, 255)),
                ..Default::default()
            },
            url_val: StyleSpec {
                fg: Some(Color::Rgb(100, 220, 100)),
                ..Default::default()
            },
            code_key: StyleSpec {
                fg: Some(Color::Rgb(120, 200, 255)),
                ..Default::default()
            },
            code_val: StyleSpec {
                fg: Some(Color::Yellow),
                ..Default::default()
            },
        }
    }
}

// ── Markdown theme ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkdownTableTheme {
    #[serde(default)]
    pub header: StyleSpec,
    #[serde(default)]
    pub row_even: StyleSpec,
    #[serde(default)]
    pub row_odd: StyleSpec,
    #[serde(default)]
    pub data: StyleSpec,
    #[serde(default)]
    pub separator: StyleSpec,
}

impl Default for MarkdownTableTheme {
    fn default() -> Self {
        Self {
            header: StyleSpec {
                fg: Some(Color::Rgb(200, 210, 240)),
                bg: Some(Color::Rgb(30, 40, 60)),
                bold: Some(true),
                ..Default::default()
            },
            row_even: StyleSpec {
                bg: Some(Color::Rgb(22, 26, 34)),
                ..Default::default()
            },
            row_odd: StyleSpec {
                bg: Some(Color::Rgb(30, 35, 45)),
                ..Default::default()
            },
            data: StyleSpec {
                fg: Some(Color::Rgb(210, 215, 225)),
                ..Default::default()
            },
            separator: StyleSpec {
                fg: Some(Color::Rgb(0, 0, 0)),
                ..Default::default()
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkdownTheme {
    #[serde(default)]
    pub code: StyleSpec,
    #[serde(default)]
    pub table: MarkdownTableTheme,
}

impl Default for MarkdownTheme {
    fn default() -> Self {
        Self {
            code: StyleSpec {
                fg: Some(Color::Rgb(210, 160, 100)),
                ..Default::default()
            },
            table: MarkdownTableTheme::default(),
        }
    }
}

// ── Tools theme ───────────────────────────────────────────────────────────────

fn default_tool_executing() -> ToolTheme {
    ToolTheme {
        prefix: PrefixStyle {
            text: Some("💻 ".to_string()),
            fg: Some(Color::Cyan),
            ..Default::default()
        },
        headline: StyleSpec {
            fg: Some(Color::Cyan),
            bold: Some(true),
            ..Default::default()
        },
        body: StyleSpec {
            fg: Some(Color::Rgb(80, 210, 210)),
            ..Default::default()
        },
        placeholder: PlaceholderStyle {
            fg: Some(Color::Rgb(100, 100, 120)),
            italic: Some(true),
            lines: PlaceholderCounterStyle {
                fg: Some(Color::Rgb(80, 80, 100)),
                ..Default::default()
            },
            ..Default::default()
        },
    }
}

fn default_tool_file() -> ToolTheme {
    ToolTheme {
        headline: StyleSpec {
            fg: Some(Color::LightBlue),
            bold: Some(true),
            ..Default::default()
        },
        body: StyleSpec {
            fg: Some(Color::Rgb(100, 140, 180)),
            ..Default::default()
        },
        placeholder: PlaceholderStyle {
            fg: Some(Color::Rgb(100, 100, 120)),
            italic: Some(true),
            lines: PlaceholderCounterStyle {
                fg: Some(Color::Rgb(80, 80, 100)),
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    }
}

fn default_tools_map() -> HashMap<String, ToolTheme> {
    let mut map = HashMap::new();

    // default fallback
    map.insert(
        "default".to_string(),
        ToolTheme {
            prefix: PrefixStyle {
                text: Some("⚙️ ".to_string()),
                fg: Some(Color::Rgb(170, 170, 170)),
                ..Default::default()
            },
            body: StyleSpec {
                fg: Some(Color::Rgb(170, 170, 170)),
                ..Default::default()
            },
            placeholder: PlaceholderStyle {
                fg: Some(Color::Rgb(100, 100, 120)),
                italic: Some(true),
                ..Default::default()
            },
            ..Default::default()
        },
    );

    // executing group
    map.insert("executing".to_string(), default_tool_executing());

    // python — unique headline color to distinguish source code from shell commands
    let mut python = default_tool_executing();
    python.headline = StyleSpec {
        fg: Some(Color::Rgb(130, 150, 210)),
        bold: Some(true),
        ..Default::default()
    };
    // body inherits from executing group (same as bash/exec program output)
    map.insert("python".to_string(), python);

    // individual file tools
    let mut read = default_tool_file();
    read.prefix = PrefixStyle {
        text: Some("👀 ".to_string()),
        fg: Some(Color::LightBlue),
        ..Default::default()
    };
    map.insert("read_file".to_string(), read);

    let mut write = default_tool_file();
    write.prefix = PrefixStyle {
        text: Some("📄 ".to_string()),
        fg: Some(Color::LightBlue),
        ..Default::default()
    };
    map.insert("write_file".to_string(), write);

    let mut edit = default_tool_file();
    edit.prefix = PrefixStyle {
        text: Some("📝 ".to_string()),
        fg: Some(Color::LightBlue),
        ..Default::default()
    };
    edit.placeholder.common_lines = PlaceholderCounterStyle {
        fg: Some(Color::Rgb(80, 80, 100)),
        ..Default::default()
    };
    edit.placeholder.changed_lines = PlaceholderCounterStyle {
        fg: Some(Color::Rgb(120, 120, 80)),
        ..Default::default()
    };
    map.insert("edit_file".to_string(), edit);

    let mut find = default_tool_file();
    find.prefix = PrefixStyle {
        text: Some("🔍 ".to_string()),
        fg: Some(Color::LightBlue),
        ..Default::default()
    };
    map.insert("find_files".to_string(), find);

    map.insert(
        "ask_user".to_string(),
        ToolTheme {
            prefix: PrefixStyle {
                text: Some("❓ ".to_string()),
                fg: Some(Color::Rgb(255, 220, 80)),
                ..Default::default()
            },
            body: StyleSpec {
                fg: Some(Color::Rgb(255, 220, 80)),
                ..Default::default()
            },
            placeholder: PlaceholderStyle {
                fg: Some(Color::Rgb(100, 100, 120)),
                italic: Some(true),
                ..Default::default()
            },
            ..Default::default()
        },
    );

    map
}

/// Resolution order for tool theme lookup:
/// 1. Named tool entry (e.g. `tools["bash"]`)
/// 2. `tools["executing"]` — for bash, exec, cmd, powershell
/// 3. `tools["default"]`
const EXECUTING_TOOLS: &[&str] = &["bash", "exec", "cmd", "powershell", "python"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsTheme(#[serde(default = "default_tools_map")] pub HashMap<String, ToolTheme>);

impl Default for ToolsTheme {
    fn default() -> Self {
        Self(default_tools_map())
    }
}

impl ToolsTheme {
    /// Look up the resolved [`ToolTheme`] for a tool name, applying the
    /// three-level fallback chain.
    pub fn get(&self, name: &str) -> ResolvedToolTheme<'_> {
        let named = self.0.get(name);
        let group = if EXECUTING_TOOLS.contains(&name) {
            self.0.get("executing")
        } else {
            None
        };
        let default = self.0.get("default");
        ResolvedToolTheme {
            named,
            group,
            default,
        }
    }
}

/// A resolved tool theme that applies the three-level fallback chain for
/// each attribute: named → group (`executing`) → `default`.
pub struct ResolvedToolTheme<'a> {
    named: Option<&'a ToolTheme>,
    group: Option<&'a ToolTheme>,
    default: Option<&'a ToolTheme>,
}

// Methods are public API; callers added as UI migration completes.
#[allow(dead_code)]
impl<'a> ResolvedToolTheme<'a> {
    fn prefix_text_chain(&self) -> impl Iterator<Item = &'a PrefixStyle> {
        self.named
            .map(|t| &t.prefix)
            .into_iter()
            .chain(self.group.map(|t| &t.prefix))
            .chain(self.default.map(|t| &t.prefix))
    }

    /// Resolved prefix text (first non-None wins).
    pub fn prefix_text(&self) -> &str {
        self.prefix_text_chain()
            .find_map(|p| p.text.as_deref())
            .unwrap_or("⚙️ ")
    }

    /// Resolved prefix ratatui style.
    pub fn prefix_style(&self) -> Style {
        // Merge: named overrides group overrides default
        let mut s = Style::default();
        for p in [self.default, self.group, self.named].into_iter().flatten() {
            let ps = &p.prefix;
            if let Some(fg) = ps.fg {
                s = s.fg(fg);
            }
            if let Some(bg) = ps.bg {
                s = s.bg(bg);
            }
            if ps.bold == Some(true) {
                s = s.add_modifier(Modifier::BOLD);
            }
            if ps.dim == Some(true) {
                s = s.add_modifier(Modifier::DIM);
            }
            if ps.italic == Some(true) {
                s = s.add_modifier(Modifier::ITALIC);
            }
        }
        s
    }

    /// Resolved body ratatui style.
    pub fn body_style(&self) -> Style {
        let mut s = Style::default();
        for t in [self.default, self.group, self.named].into_iter().flatten() {
            let b = &t.body;
            if let Some(fg) = b.fg {
                s = s.fg(fg);
            }
            if let Some(bg) = b.bg {
                s = s.bg(bg);
            }
            if b.bold == Some(true) {
                s = s.add_modifier(Modifier::BOLD);
            }
            if b.dim == Some(true) {
                s = s.add_modifier(Modifier::DIM);
            }
            if b.italic == Some(true) {
                s = s.add_modifier(Modifier::ITALIC);
            }
        }
        s
    }

    /// Resolved body color (fg).
    pub fn body_color(&self) -> Option<Color> {
        [self.named, self.group, self.default]
            .into_iter()
            .flatten()
            .find_map(|t| t.body.fg)
    }

    /// Resolved headline color (fg of the tool call intent line).
    /// Falls back to `body_color()` when no explicit headline.fg is set.
    pub fn headline_color(&self) -> Option<Color> {
        [self.named, self.group, self.default]
            .into_iter()
            .flatten()
            .find_map(|t| t.headline.fg)
            .or_else(|| self.body_color())
    }

    /// Resolved headline ratatui style.
    pub fn headline_style(&self) -> Style {
        let mut s = Style::default();
        // Apply body style first, then headline overrides
        for t in [self.default, self.group, self.named].into_iter().flatten() {
            let b = &t.body;
            if let Some(fg) = b.fg {
                s = s.fg(fg);
            }
            if let Some(bg) = b.bg {
                s = s.bg(bg);
            }
        }
        for t in [self.default, self.group, self.named].into_iter().flatten() {
            let h = &t.headline;
            if let Some(fg) = h.fg {
                s = s.fg(fg);
            }
            if let Some(bg) = h.bg {
                s = s.bg(bg);
            }
            if h.bold == Some(true) {
                s = s.add_modifier(Modifier::BOLD);
            }
            if h.dim == Some(true) {
                s = s.add_modifier(Modifier::DIM);
            }
            if h.italic == Some(true) {
                s = s.add_modifier(Modifier::ITALIC);
            }
        }
        s
    }

    /// Resolved placeholder visibility.
    pub fn placeholder_visible(&self) -> bool {
        [self.named, self.group, self.default]
            .into_iter()
            .flatten()
            .find_map(|t| t.placeholder.visible)
            .unwrap_or(true)
    }

    /// Resolved placeholder style.
    pub fn placeholder_style(&self) -> Style {
        let mut s = Style::default();
        for t in [self.default, self.group, self.named].into_iter().flatten() {
            let p = &t.placeholder;
            if let Some(fg) = p.fg {
                s = s.fg(fg);
            }
            if let Some(bg) = p.bg {
                s = s.bg(bg);
            }
            if p.bold == Some(true) {
                s = s.add_modifier(Modifier::BOLD);
            }
            if p.dim == Some(true) {
                s = s.add_modifier(Modifier::DIM);
            }
            if p.italic == Some(true) {
                s = s.add_modifier(Modifier::ITALIC);
            }
        }
        s
    }

    /// Resolved placeholder text.
    pub fn placeholder_text(&self) -> Option<&str> {
        [self.named, self.group, self.default]
            .into_iter()
            .flatten()
            .find_map(|t| t.placeholder.text.as_deref())
    }

    /// Resolved lines-counter visibility.
    pub fn lines_counter_visible(&self) -> bool {
        [self.named, self.group, self.default]
            .into_iter()
            .flatten()
            .find_map(|t| t.placeholder.lines.visible)
            .unwrap_or(true)
    }

    /// Resolved lines-counter style.
    pub fn lines_counter_style(&self) -> Style {
        let mut s = Style::default();
        for t in [self.default, self.group, self.named].into_iter().flatten() {
            let c = &t.placeholder.lines;
            if let Some(fg) = c.fg {
                s = s.fg(fg);
            }
            if let Some(bg) = c.bg {
                s = s.bg(bg);
            }
        }
        s
    }

    /// Resolved common_lines-counter visibility.
    pub fn common_lines_visible(&self) -> bool {
        [self.named, self.group, self.default]
            .into_iter()
            .flatten()
            .find_map(|t| t.placeholder.common_lines.visible)
            .unwrap_or(true)
    }

    /// Resolved common_lines-counter style.
    pub fn common_lines_style(&self) -> Style {
        let mut s = Style::default();
        for t in [self.default, self.group, self.named].into_iter().flatten() {
            let c = &t.placeholder.common_lines;
            if let Some(fg) = c.fg {
                s = s.fg(fg);
            }
            if let Some(bg) = c.bg {
                s = s.bg(bg);
            }
        }
        s
    }

    /// Resolved changed_lines-counter visibility.
    pub fn changed_lines_visible(&self) -> bool {
        [self.named, self.group, self.default]
            .into_iter()
            .flatten()
            .find_map(|t| t.placeholder.changed_lines.visible)
            .unwrap_or(true)
    }

    /// Resolved changed_lines-counter style.
    pub fn changed_lines_style(&self) -> Style {
        let mut s = Style::default();
        for t in [self.default, self.group, self.named].into_iter().flatten() {
            let c = &t.placeholder.changed_lines;
            if let Some(fg) = c.fg {
                s = s.fg(fg);
            }
            if let Some(bg) = c.bg {
                s = s.bg(bg);
            }
        }
        s
    }
}

// ── Root Theme ────────────────────────────────────────────────────────────────

/// The root theme struct. Loaded from `theme.toml`; all fields are optional
/// and default to Xi's built-in appearance.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Theme {
    #[serde(default)]
    pub log: LogTheme,
    #[serde(default)]
    pub input: InputTheme,
    #[serde(default)]
    pub menu: MenuTheme,
    #[serde(default)]
    pub status: StatusTheme,
    #[serde(default)]
    pub info: InfoTheme,
    #[serde(default)]
    pub login: LoginTheme,
    #[serde(default)]
    pub markdown: MarkdownTheme,
    #[serde(default)]
    pub tools: ToolsTheme,
}

impl Theme {
    /// Load a theme from a TOML file. Missing file returns `Theme::default()`.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path)
            .with_context(|| format!("Failed to read theme file: {}", path.display()))?;
        toml::from_str(&raw)
            .with_context(|| format!("Failed to parse theme file: {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hex_color() {
        assert_eq!(parse_color("#a0c8ff").unwrap(), Color::Rgb(160, 200, 255));
    }

    #[test]
    fn test_parse_terminal_color() {
        assert_eq!(parse_color("cyan").unwrap(), Color::Cyan);
        assert_eq!(parse_color("bright-red").unwrap(), Color::LightRed);
    }

    #[test]
    fn test_parse_css_named_color() {
        // cornflowerblue = #6495ED = rgb(100, 149, 237)
        assert_eq!(
            parse_color("cornflowerblue").unwrap(),
            Color::Rgb(100, 149, 237)
        );
    }

    #[test]
    fn test_theme_default_roundtrip() {
        // Default theme serializes and re-parses without error
        let theme = Theme::default();
        let toml_str = toml::to_string(&theme).unwrap();
        let _: Theme = toml::from_str(&toml_str).unwrap();
    }

    #[test]
    fn test_empty_toml_gives_default() {
        let theme: Theme = toml::from_str("").unwrap();
        // user bg should match hardcoded default
        assert_eq!(theme.log.user.bg, Some(Color::Rgb(50, 50, 64)));
    }

    #[test]
    fn test_tool_resolution_named() {
        let theme = Theme::default();
        let resolved = theme.tools.get("bash");
        // bash is an executing tool — headline is bold Cyan, body is dimmer
        assert_eq!(resolved.headline_color(), Some(Color::Cyan));
        assert!(resolved.body_color().is_some());
    }

    #[test]
    fn test_tool_resolution_default_fallback() {
        let theme = Theme::default();
        let resolved = theme.tools.get("unknown_tool");
        assert_eq!(resolved.body_color(), Some(Color::Rgb(170, 170, 170)));
    }
}
