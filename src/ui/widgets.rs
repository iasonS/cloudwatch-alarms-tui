//! History graph widget.
//!
//! From macmon (MIT, see LICENSE-macmon), with a color per column. Drawn here rather than with
//! ratatui's `Sparkline`, which rounds bars down (a small non-zero sample stays blank) and scales
//! to the largest sample of all its data, not only the columns on screen.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::widgets::Widget;

use super::theme;

/// Bar heights in one row: `▁` (1/8) up to `█` (8/8).
const BAR_LEVELS: u64 = 8;
/// Code point before `▁`: the bar of level `n` is `BAR_BASE + n`.
const BAR_BASE: u32 = 0x2580;
/// Full bar.
const FULL: char = '█';
/// Half-filled cell of a three-level bar.
const HALF: char = '▄';

/// History graph for newest-first samples over its whole area, right-aligned (newest sample on
/// the right): one solid bar per column, growing from the bottom row up in eighths of a row
/// (`▁`…`█`; blank / `▄` / `█` with `three_levels`), each in its own color from `colors` (newest
/// first, as the data). Zero values leave the column blank.
pub(super) struct Graph<'a> {
  data: &'a [u64],
  max: Option<u64>,
  colors: &'a [Color],
  three_levels: bool,
  gap: u64,
}

impl<'a> Graph<'a> {
  /// Graph scaled to its largest visible sample, every bar in `theme::OK`.
  pub(super) fn new(data: &'a [u64]) -> Self {
    Self { data, max: None, colors: &[], three_levels: false, gap: 0 }
  }

  /// Eighths of a row left empty at the top, so the bars of graphs on adjacent rows don't touch.
  pub(super) fn gap(mut self, eighths: u64) -> Self {
    self.gap = eighths;
    self
  }

  /// Bars in three levels (blank, `▄`, `█`) instead of eighths, for Apple Terminal, which draws
  /// gaps between the eighth blocks.
  pub(super) fn three_levels(mut self, three_levels: bool) -> Self {
    self.three_levels = three_levels;
    self
  }

  /// Value drawn at full height.
  pub(super) fn max(mut self, max: u64) -> Self {
    self.max = Some(max);
    self
  }

  /// Color of each bar, newest first; bars past the end of `colors` are `theme::OK`.
  pub(super) fn colors(mut self, colors: &'a [Color]) -> Self {
    self.colors = colors;
    self
  }
}

impl Widget for Graph<'_> {
  fn render(self, area: Rect, buf: &mut Buffer) {
    let area = area.intersection(buf.area);
    if area.is_empty() {
      return;
    }

    let visible = &self.data[..self.data.len().min(usize::from(area.width))];
    let max = self.max.unwrap_or_else(|| visible.iter().copied().max().unwrap_or(0)).max(1);
    let levels = (u64::from(area.height) * BAR_LEVELS).saturating_sub(self.gap);
    for (i, (x, &value)) in (area.left()..area.right()).rev().zip(visible).enumerate() {
      let mut level = bar_level(value, max, levels);
      let color = self.colors.get(i).copied().unwrap_or(theme::OK);
      for y in (area.top()..area.bottom()).rev() {
        if level == 0 {
          break;
        }

        let eighths = level.min(BAR_LEVELS);
        let symbol = bar_symbol(eighths, self.three_levels);
        if symbol != ' ' {
          buf[(x, y)].set_char(symbol).set_fg(color);
        }
        level -= eighths;
      }
    }
  }
}

/// Glyph of a cell filled `eighths` of a row from the bottom: `▁`…`█`, or with three levels
/// blank up to 1/8, `▄` up to 6/8 and `█` above.
fn bar_symbol(eighths: u64, three_levels: bool) -> char {
  match (three_levels, eighths) {
    (_, 0) | (true, 1) => ' ',
    (true, 2..=6) => HALF,
    (true, _) => FULL,
    (false, _) => char::from_u32(BAR_BASE + eighths.min(BAR_LEVELS) as u32).unwrap_or(' '),
  }
}

/// Bar height of `value` in eighths of a row out of `levels`, rounded up: non-zero values get at
/// least `▁`.
fn bar_level(value: u64, max: u64, levels: u64) -> u64 {
  if value == 0 {
    return 0;
  }

  // in u128, so large values don't overflow
  let level = (u128::from(value) * u128::from(levels)).div_ceil(u128::from(max.max(1)));
  level.min(u128::from(levels)) as u64
}

#[cfg(test)]
mod tests {
  use ratatui::buffer::Buffer;
  use ratatui::layout::Rect;
  use ratatui::style::Color;
  use ratatui::widgets::Widget;

  use super::Graph;
  use crate::ui::theme;

  fn draw(widget: impl Widget, width: u16, height: u16) -> Buffer {
    let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
    widget.render(buf.area, &mut buf);
    buf
  }

  fn rows(buf: &Buffer) -> Vec<String> {
    let row = |y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
    (0..buf.area.height).map(row).collect()
  }

  #[test]
  fn graph_bars_grow_across_rows() {
    // three rows, newest on the right: 100 % full, 50 % one and a half rows, 30 % (7.2 eighths
    // rounded up to 8) one row, a tiny value one eighth at the bottom, zero blank
    let buf = draw(Graph::new(&[100, 50, 30, 1, 0]).max(100), 5, 3);
    assert_eq!(rows(&buf), ["█", "▄█", "▁███"].map(|r| format!("{r:>5}")));

    // every level of a two-row bar, bottom row full before the top row starts
    let levels: Vec<u64> = (1..=16).rev().collect();
    let buf = draw(Graph::new(&levels).max(16), 16, 2);
    assert_eq!(rows(&buf), ["        ▁▂▃▄▅▆▇█", "▁▂▃▄▅▆▇█████████"]);

    // without a maximum: scaled to the largest sample on screen, the older 100 doesn't fit
    assert_eq!(rows(&draw(Graph::new(&[10, 20, 100]), 2, 2)), ["█ ", "██"]);
  }

  #[test]
  fn each_bar_takes_its_own_color() {
    // newest first: the red bar is on the right; a bar without a color is green
    let colors = [theme::ALARM, theme::OK];
    let buf = draw(Graph::new(&[8, 8, 8]).max(8).colors(&colors), 3, 1);
    assert_eq!(rows(&buf), ["███"]);
    assert_eq!([0, 1, 2].map(|x| buf[(x, 0)].fg), [Color::Green, Color::Green, Color::Red]);
  }

  #[test]
  fn a_gap_keeps_the_top_of_the_row_empty() {
    // full and half values in one row with two eighths left empty: `▆` and `▃`
    assert_eq!(rows(&draw(Graph::new(&[8, 4, 0]).max(8).gap(2), 3, 1)), [" ▃▆"]);
  }

  #[test]
  fn graph_in_three_levels_for_apple_terminal() {
    // 1/8 blank, 2/8–6/8 `▄`, 7/8–8/8 `█`, full cells below the top one
    let levels: Vec<u64> = (1..=16).rev().collect();
    let buf = draw(Graph::new(&levels).max(16).three_levels(true), 16, 2);
    assert_eq!(rows(&buf), ["         ▄▄▄▄▄██", " ▄▄▄▄▄██████████"]);
  }
}
