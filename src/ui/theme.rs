//! Colors from the terminal's own palette (the default foreground and the 16 ANSI colors), so
//! opsmon follows the terminal theme, and the bar glyphs the terminal draws without gaps.
//!
//! From macmon (MIT, see LICENSE-macmon).

use std::borrow::Cow;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

/// Borders: bright black (ANSI 8).
pub(super) const BORDER: Color = Color::DarkGray;
/// Secondary text (separators, units, column names): bright black (ANSI 8).
pub(super) const DIM: Color = Color::DarkGray;
/// Titles and text: the terminal's default foreground.
pub(super) const TEXT: Color = Color::Reset;
/// The key in a hint: cyan (ANSI 6), which no state uses.
pub(super) const KEY: Color = Color::Cyan;
/// Selected row: reverse video over the default colors.
pub(super) const SELECTED: Style = Style::new().fg(TEXT).add_modifier(Modifier::REVERSED);

/// An alarm in OK, and a datapoint within the threshold.
pub(super) const OK: Color = Color::Green;
/// An alarm in ALARM, and a datapoint beyond the threshold.
pub(super) const ALARM: Color = Color::Red;
/// An alarm in INSUFFICIENT_DATA.
pub(super) const NO_DATA: Color = Color::Yellow;

/// Box or column name: bold, in the default color.
pub(super) fn heading<'a>(text: impl Into<Cow<'a, str>>) -> Span<'a> {
  Span::styled(text, Style::new().fg(TEXT).add_modifier(Modifier::BOLD))
}

/// Text in the default color. The color is set, not left out, so text drawn over a border doesn't
/// take the border's color.
pub(super) fn text<'a>(text: impl Into<Cow<'a, str>>) -> Span<'a> {
  Span::styled(text, TEXT)
}

pub(super) fn dim<'a>(text: impl Into<Cow<'a, str>>) -> Span<'a> {
  Span::styled(text, DIM)
}

pub(super) fn colored<'a>(text: impl Into<Cow<'a, str>>, color: Color) -> Span<'a> {
  Span::styled(text, color)
}

/// The key to press in a hint: `KEY`, no modifier.
pub(super) fn key<'a>(text: impl Into<Cow<'a, str>>) -> Span<'a> {
  Span::styled(text, KEY)
}

/// The label of a hint: the default color, faint (SGR 2). Terminals that support it blend the
/// text toward the background, so it reads the same in light and dark themes, unlike ANSI 8; the
/// others show plain text.
pub(super) fn faint<'a>(text: impl Into<Cow<'a, str>>) -> Span<'a> {
  Span::styled(text, Style::new().fg(TEXT).add_modifier(Modifier::DIM))
}

/// True in Apple Terminal (`TERM_PROGRAM`), which draws gaps between the eighth blocks, so graph
/// bars come in three levels there (blank, `▄`, `█`).
pub(crate) fn detect_three_level_bars() -> bool {
  std::env::var("TERM_PROGRAM").ok().as_deref() == Some("Apple_Terminal")
}
