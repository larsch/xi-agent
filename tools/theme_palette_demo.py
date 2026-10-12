#!/usr/bin/env python3
"""Render a catalog of Xi display classes using the built-in default palette.

This is intentionally a small, dependency-free palette laboratory. Edit the
``PALETTE`` dictionary below, run the script, and inspect the resulting
synthetic terminal log:

    python3 tools/theme_palette_demo.py
    python3 tools/theme_palette_demo.py --plain

The samples are representative rather than a pixel-perfect replacement for
Ratatui layout. Labels make the semantic class of every color/style visible.
"""

from __future__ import annotations

import argparse
import shutil
from dataclasses import dataclass


# Edit this dictionary when experimenting with a new default palette. Values
# are RGB tuples; ``None`` means the terminal's default foreground/background.
PALETTE: dict[str, tuple[int, int, int] | None] = {
    # Log blocks and decoration
    "log.user.bg": (50, 50, 64),
    "log.ask_user.bg": (27, 71, 31),
    "log.edge_marker.fg": (100, 100, 120),
    "log.assistant.thinking.fg": (150, 150, 160),
    "log.assistant.provisional.fg": (180, 190, 220),
    "log.assistant.final.fg": (220, 225, 240),
    "log.steering.fg": (200, 200, 120),
    "log.diff.added.fg": (85, 255, 85),
    "log.diff.removed.fg": (255, 85, 85),
    "log.diff.unchanged.fg": (136, 136, 136),
    # Tool families
    "tools.default.fg": (170, 170, 170),
    "tools.executing.headline.fg": (0, 255, 255),
    "tools.executing.body.fg": (80, 210, 210),
    "tools.file.headline.fg": (128, 192, 255),
    "tools.file.body.fg": (100, 140, 180),
    "tools.python.headline.fg": (130, 150, 210),
    "tools.ask_user.fg": (255, 220, 80),
    "tools.placeholder.fg": (100, 100, 120),
    "tools.placeholder.counter.fg": (80, 80, 100),
    "tools.placeholder.changed.fg": (120, 120, 80),
    # Input panels
    "input.normal.bg": (30, 30, 40),
    "input.shell.bg": (24, 34, 32),
    "input.ask_user.bg": (50, 30, 15),
    "input.prompt.fg": (120, 140, 140),
    "input.shell_prompt.fg": (140, 220, 140),
    "input.ask_user_prompt.fg": (255, 220, 80),
    "input.placeholder.fg": (120, 140, 140),
    "input.at_file.fg": (0, 255, 255),
    # Menus and control bands
    "menu.completion.bg": (22, 22, 38),
    "menu.completion.selected.bg": (55, 55, 100),
    "menu.completion.command.fg": (120, 200, 255),
    "menu.completion.description.fg": (140, 140, 160),
    "menu.completion.match.fg": (255, 220, 80),
    "menu.selection.bg": (18, 35, 18),
    "menu.selection.header.bg": (20, 45, 20),
    "menu.selection.selected.bg": (30, 90, 30),
    "menu.selection.item.fg": (140, 220, 140),
    "status.provider.fg": (160, 200, 255),
    "status.model.fg": (100, 140, 100),
    "status.cost.fg": (220, 180, 80),
    "status.idle.fg": (160, 160, 180),
    # Other panels
    "info.bg": (20, 20, 30),
    "info.separator.fg": (60, 60, 80),
    "info.key.fg": (100, 100, 130),
    "info.value.fg": (180, 200, 255),
    "info.hint.fg": (60, 60, 80),
    "login.header.bg": (20, 30, 60),
    "login.content.bg": (15, 22, 48),
    "login.instruction.fg": (180, 180, 200),
    "login.status.fg": (255, 255, 255),
    "login.key.fg": (120, 200, 255),
    "login.value.fg": (100, 220, 100),
    "login.code.fg": (255, 255, 0),
    # Markdown
    "markdown.code.fg": (210, 160, 100),
    "markdown.table.header.fg": (200, 210, 240),
    "markdown.table.header.bg": (30, 40, 60),
    "markdown.table.even.bg": (22, 26, 34),
    "markdown.table.odd.bg": (30, 35, 45),
    "markdown.table.data.fg": (210, 215, 225),
}

RESET = "\x1b[0m"
BOLD = "\x1b[1m"
DIM = "\x1b[2m"
ITALIC = "\x1b[3m"
UNDERLINE = "\x1b[4m"


@dataclass(frozen=True)
class Paint:
    key: str | None = None
    bold: bool = False
    dim: bool = False
    italic: bool = False
    underline: bool = False
    bg: str | None = None

    def open(self, plain: bool) -> str:
        if plain:
            return ""
        codes: list[str] = []
        color = PALETTE.get(self.key) if self.key else None
        if color is not None:
            codes.append(f"\x1b[38;2;{color[0]};{color[1]};{color[2]}m")
        background = PALETTE.get(self.bg) if self.bg else None
        if background is not None:
            codes.append(f"\x1b[48;2;{background[0]};{background[1]};{background[2]}m")
        if self.bold:
            codes.append(BOLD)
        if self.dim:
            codes.append(DIM)
        if self.italic:
            codes.append(ITALIC)
        if self.underline:
            codes.append(UNDERLINE)
        return "".join(codes)


def paint(text: str, style: Paint, plain: bool) -> str:
    return f"{style.open(plain)}{text}{'' if plain else RESET}"


def label(title: str, key: str, plain: bool) -> str:
    return paint(f"[{title} · {key}]", Paint("info.key.fg", bold=True), plain)


def block(title: str, key: str, lines: list[str], plain: bool, bg: str | None = None) -> None:
    print(label(title, key, plain))
    for line in lines:
        print(paint(f"│ {line}", Paint(key, bg=bg), plain))
    print()


def section(title: str, plain: bool) -> None:
    print(paint(f"\n══ {title} ══", Paint("status.provider.fg", bold=True), plain))


def render(args: argparse.Namespace) -> None:
    plain = args.plain
    width = min(shutil.get_terminal_size((100, 24)).columns, 100)
    print(paint(" Xi palette laboratory ".center(width, "═"), Paint("status.provider.fg", bold=True), plain))
    print(paint("Synthetic log: every logical block, decoration, and control surface", Paint("status.idle.fg", italic=True), plain))

    section("Conversation log blocks", plain)
    block("user content / solid background", "log.user.bg", ["Please inspect the new palette."], plain, "log.user.bg")
    block("assistant thinking / dim", "log.assistant.thinking.fg", ["I am comparing semantic roles rather than tool names."], plain)
    block("assistant provisional", "log.assistant.provisional.fg", ["💭 The response is still streaming…"], plain)
    block("assistant final + markdown", "log.assistant.final.fg", ["💬 The answer is settled.", "  **bold**  _italic_  `inline code`  [link]"], plain)
    block("queued steering + icon", "log.steering.fg", ["🕹️ Also check the contrast of the tool output."], plain)
    block("ask-user context", "log.ask_user.bg", ["Context: this choice affects the generated config."], plain, "log.ask_user.bg")
    block("ask-user question", "tools.ask_user.fg", ["❓ Which palette direction should we try?"], plain)
    block("ask-user response", "log.ask_user.bg", ["Selected: warm and readable"], plain, "log.ask_user.bg")

    section("Tool intent, placeholders, and bodies", plain)
    for title, key, icon, text in [
        ("executing tool intent", "tools.executing.headline.fg", "💻", "bash · cargo test"),
        ("file tool intent", "tools.file.headline.fg", "👀", "read_file · src/theme.rs"),
        ("python tool intent", "tools.python.headline.fg", "🐍", "run_python · palette experiment"),
        ("generic tool intent", "tools.default.fg", "⚙️", "custom_tool · inspect output"),
    ]:
        block(title, key, [f"{icon} {text}"], plain)
    block("pending placeholder", "tools.placeholder.fg", ["💻 running…"], plain)
    block("pending line counter", "tools.placeholder.counter.fg", ["💻 running… 12 lines"], plain)
    block("edit pending counters", "tools.placeholder.changed.fg", ["📝 editing… 4 unchanged · 2 changed"], plain)
    block("tool body", "tools.executing.body.fg", ["stdout: compiling palette demo", "stdout: finished successfully"], plain)
    block("file body", "tools.file.body.fg", ["1  use ratatui::style::Color;", "2  pub struct Theme { … }"], plain)
    block("default body", "tools.default.fg", ["custom output: no specialized tool theme"], plain)

    section("Truncation and diff variants", plain)
    block("head truncation marker", "tools.file.body.fg", ["first visible line", "second visible line", "… 42 total lines"], plain)
    block("tail truncation marker", "tools.executing.body.fg", ["… 120 total lines", "last visible line", "command completed"], plain)
    block("added diff line", "log.diff.added.fg", ["+ palette = warmer_palette"], plain)
    block("removed diff line", "log.diff.removed.fg", ["- palette = default_palette"], plain)
    block("unchanged diff line", "log.diff.unchanged.fg", ["  return Theme::default();"], plain)

    section("Markdown and table styles", plain)
    block("code", "markdown.code.fg", ["    def choose_palette(name):", "        return palettes[name]"], plain)
    block("table header", "markdown.table.header.fg", ["ROLE       │ FOREGROUND │ BACKGROUND"], plain, "markdown.table.header.bg")
    block("table even row", "markdown.table.data.fg", ["assistant   │ settled    │ none"], plain, "markdown.table.even.bg")
    block("table odd row", "markdown.table.data.fg", ["tool body   │ cyan       │ none"], plain, "markdown.table.odd.bg")
    block("table separator", "markdown.table.data.fg", ["────────────┼────────────┼─────────"], plain)

    section("Status, info, and login panels", plain)
    print(paint(" provider: ", Paint("status.idle.fg"), plain) + paint("Anthropic", Paint("status.provider.fg", bold=True), plain) + "  " + paint("model: claude", Paint("status.model.fg", italic=True), plain) + "  " + paint("cost: $0.04", Paint("status.cost.fg", bold=True), plain))
    print(paint("INFO  ", Paint("info.separator.fg"), plain) + paint("key", Paint("info.key.fg"), plain) + " = " + paint("value", Paint("info.value.fg"), plain) + "  " + paint("hint", Paint("info.hint.fg", italic=True), plain))
    print(paint(" LOGIN ", Paint("login.header.bg"), plain))
    print(paint("  Sign in to continue", Paint("login.instruction.fg", bg="login.content.bg"), plain))
    print(paint("  URL: ", Paint("login.key.fg", bg="login.content.bg"), plain) + paint("https://example.test/device", Paint("login.value.fg", bg="login.content.bg"), plain))
    print(paint("  Code: ", Paint("login.key.fg", bg="login.content.bg"), plain) + paint("ABCD-EFGH", Paint("login.code.fg", bg="login.content.bg"), plain))

    section("Input panels and menus", plain)
    block("normal input + prompt", "input.prompt.fg", ["> type a message…"], plain, "input.normal.bg")
    block("shell input + prompt", "input.shell_prompt.fg", ["$ git status"], plain, "input.shell.bg")
    block("ask-user input + prompt", "input.ask_user_prompt.fg", ["? choose an option"], plain, "input.ask_user.bg")
    block("resolved @file token", "input.at_file.fg", ["> compare @src/theme.rs with @src/ui/log.rs"], plain, "input.normal.bg")
    block("completion popup", "menu.completion.command.fg", ["xi  open session", "theme  edit theme file"], plain, "menu.completion.bg")
    block("completion selected + match", "menu.completion.match.fg", ["▶ theme  edit theme file"], plain, "menu.completion.selected.bg")
    block("selection menu item", "menu.selection.item.fg", ["  warm and readable", "  high contrast"], plain, "menu.selection.bg")
    block("selection menu selected", "menu.selection.item.fg", ["▶ monochrome (selected)"], plain, "menu.selection.selected.bg")

    section("Spacing and decoration checklist", plain)
    print("  margin top/bottom: blank rows outside the block")
    print("  padding style blank: background stops at content")
    print("  padding style solid: background extends into padding")
    print("  padding style half-block: ▀ / ▄ transition rows")
    print("  edge marker: │   prefix/icon: 💻 👀 📝 🔍 ❓ 🕹️")
    print(paint("\nEdit PALETTE above and rerun; use --plain to audit labels and layout.", Paint("status.idle.fg", italic=True), plain))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plain", action="store_true", help="omit ANSI colors and styles")
    render(parser.parse_args())


if __name__ == "__main__":
    main()
