//! The screen: a box per alarm group on top (its state and how near its alarms came to their
//! thresholds), the alarms below with a graph each, and the details of one alarm over them.

mod boxes;
pub(crate) mod theme;
mod widgets;

use std::collections::HashMap;

use boxes::{ELLIPSIS, Titles, draw_box, draw_scrollbar, head, hint, render_bottom_border};
use chrono::{Local, TimeZone};
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use theme::{colored, dim, heading, text};
use widgets::Graph;

use crate::alarms::{self, Alarm, FULL, Points, State};

/// Narrowest group box.
const BOX_WIDTH: u16 = 24;
/// Fewest rows left for the alarm list (borders and column names included) before the group boxes
/// lose their graph, then go.
const LIST_MIN: u16 = 8;
/// Widest alarm name before it is cut, and the narrowest it is cut to for the graph.
const NAME_MAX: usize = 40;
const NAME_MIN: usize = 12;
/// Narrowest graph in the list before names are cut for it.
const GRAPH_MIN: usize = 16;
/// Cells of the list's text columns other than the name: state dot, group, value, limit, age and
/// the gaps between them.
const TEXT_COLUMNS: usize = 2 + 1 + GROUP + 1 + 5 + 1 + 6 + 1 + 4 + 2;
const GROUP: usize = 13;

/// Time span of the graphs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Window {
  Hours3,
  Hours24,
}

impl Window {
  pub fn seconds(self) -> i64 {
    match self {
      Window::Hours3 => 3 * 3600,
      Window::Hours24 => 24 * 3600,
    }
  }

  fn label(seconds: i64) -> String {
    format!("{}h", seconds / 3600)
  }

  fn next(self) -> Self {
    match self {
      Window::Hours3 => Window::Hours24,
      Window::Hours24 => Window::Hours3,
    }
  }
}

/// Datapoints of the alarms' metrics over `[start, end)`, by alarm name.
pub struct History {
  pub start: i64,
  pub end: i64,
  pub points: HashMap<String, Points>,
  /// The first failed call, when only some of them failed.
  pub error: Option<String>,
}

pub enum Action {
  None,
  Quit,
  /// Fetch everything again, for the current window.
  Refresh,
}

pub struct App {
  profile: String,
  region: String,
  /// Start of alarm names left out on screen.
  prefix: String,
  pub three_levels: bool,
  /// In `alarms::sort` order.
  alarms: Vec<Alarm>,
  history: Option<History>,
  error: Option<String>,
  fetching: bool,
  /// Unix seconds when the alarms on screen were fetched.
  updated: Option<i64>,
  window: Window,
  selected: usize,
  /// First alarm on screen.
  offset: usize,
  details: bool,
  /// Alarm rows in the last frame, for paging.
  page: usize,
}

impl App {
  pub fn new(profile: String, region: String, prefix: String) -> Self {
    App {
      profile,
      region,
      prefix,
      three_levels: false,
      alarms: vec![],
      history: None,
      error: None,
      fetching: false,
      updated: None,
      window: Window::Hours3,
      selected: 0,
      offset: 0,
      details: false,
      page: 1,
    }
  }

  pub fn window_seconds(&self) -> i64 {
    self.window.seconds()
  }

  pub fn fetching(&mut self) {
    self.fetching = true;
  }

  /// New alarms, at unix seconds `now`; the selection stays on the same alarm. An error keeps the
  /// alarms on screen and shows under them.
  pub fn set_alarms(&mut self, result: Result<Vec<Alarm>, String>, now: i64) {
    match result {
      Ok(alarms) => {
        let selected = self.alarms.get(self.selected).map(|alarm| alarm.name.clone());
        self.alarms = alarms;
        self.sort(selected);
        self.updated = Some(now);
        self.error = None;
      }
      Err(err) => {
        self.error = Some(err);
        self.fetching = false;
      }
    }
  }

  pub fn set_history(&mut self, result: Result<History, String>) {
    match result {
      Ok(history) => {
        let selected = self.alarms.get(self.selected).map(|alarm| alarm.name.clone());
        self.error = history.error.clone();
        self.history = Some(history);
        self.sort(selected);
      }
      Err(err) => self.error = Some(err),
    }
    self.fetching = false;
  }

  /// Sorts the alarms by state and by how near they came to their thresholds in the history on
  /// screen; `selected` (an alarm name) stays selected.
  fn sort(&mut self, selected: Option<String>) {
    let history = self.history.as_ref();
    alarms::sort(&mut self.alarms, |alarm| {
      let points = history.and_then(|history| history.points.get(&alarm.name));
      points.map_or(0, |points| alarms::band(points, alarm.op, alarm.threshold))
    });
    let position = selected.and_then(|name| self.alarms.iter().position(|a| a.name == name));
    self.selected = position.unwrap_or(0);
  }

  pub fn handle_key(&mut self, key: KeyEvent) -> Action {
    if key.kind != KeyEventKind::Press {
      return Action::None;
    }
    let ctrl_c = key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c');
    if ctrl_c || key.code == KeyCode::Char('q') {
      return Action::Quit;
    }
    if self.details {
      if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
        self.details = false;
      }
      return Action::None;
    }

    let page = self.page as isize;
    match key.code {
      KeyCode::Char('r') => return Action::Refresh,
      KeyCode::Char('w') => {
        self.window = self.window.next();
        return Action::Refresh;
      }
      KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
      KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
      KeyCode::PageUp => self.move_by(-page),
      KeyCode::PageDown | KeyCode::Char(' ') => self.move_by(page),
      KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
      KeyCode::End | KeyCode::Char('G') => self.selected = self.alarms.len().saturating_sub(1),
      KeyCode::Enter => self.details = !self.alarms.is_empty(),
      _ => {}
    }
    Action::None
  }

  fn move_by(&mut self, rows: isize) {
    let last = self.alarms.len().saturating_sub(1);
    self.selected = self.selected.saturating_add_signed(rows).min(last);
  }

  /// The whole screen at unix seconds `now`.
  pub fn render(&mut self, f: &mut Frame, now: i64) {
    let area = f.area();
    if area.width < BOX_WIDTH || area.height < 4 {
      f.render_widget(Paragraph::new("window too small").wrap(Wrap { trim: true }), area);
      return;
    }
    let [header, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    self.render_header(f, header);

    let groups = alarms::groups(&self.alarms);
    let (boxes, list) = grid(body, groups.len());
    for ((name, members), area) in groups.iter().zip(boxes) {
      self.render_group(f, area, name, members);
    }
    self.render_list(f, list, now);
    if self.details && self.selected < self.alarms.len() {
      self.render_details(f, area, now);
    }
  }

  /// One frame of `width` × `height` cells as text, a line per row without trailing blanks.
  pub fn screen(&mut self, width: u16, height: u16, now: i64) -> String {
    let mut term =
      ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    let buf = term.draw(|f| self.render(f, now)).unwrap().buffer.clone();
    let row = |y| (0..width).map(|x| buf[(x, y)].symbol()).collect::<String>();
    (0..height).map(|y| row(y).trim_end().to_string() + "\n").collect()
  }

  fn render_header(&self, f: &mut Frame, area: Rect) {
    let tab = Style::new().fg(theme::TEXT).add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let left = Line::from(vec![heading(" opsmon"), dim("  "), Span::styled("Alarms", tab)]);
    let right = Line::from(dim(format!("{} · {} ", self.profile, self.region)));
    let buf = f.buffer_mut();
    buf.set_line(area.x, area.y, &left, area.width);
    let room = area.width.saturating_sub(left.width() as u16 + 1);
    let width = (right.width() as u16).min(room);
    buf.set_line(area.right() - width, area.y, &right, width);
  }

  /// A group's box: its name, how many of its alarms are in ALARM or without data, and a graph of
  /// how near the nearest of them came to its threshold in each column.
  fn render_group(&self, f: &mut Frame, area: Rect, name: &str, members: &[usize]) {
    let count = |state| members.iter().filter(|&&i| self.alarms[i].state == state).count();
    let (alarm, no_data, total) = (count(State::Alarm), count(State::NoData), members.len());
    let status = match (alarm, no_data) {
      (0, 0) => Line::from(colored(format!("{total} ok"), theme::OK)),
      (0, n) => {
        Line::from(vec![colored(format!("{n} no data"), theme::NO_DATA), dim(format!("/{total}"))])
      }
      (n, _) => {
        Line::from(vec![colored(format!("{n} alarm"), theme::ALARM), dim(format!("/{total}"))])
      }
    };
    // the name is cut so the count always fits: two border cells at each end, a blank on both
    // sides of each title and one between them
    let room = usize::from(area.width).saturating_sub(status.width() + 9);
    let name = fit(name, Span::raw(name).width().min(room), false);
    let inner = draw_box(f, area, Titles::new(heading(name)).right(status));
    let Some(history) = &self.history else { return };
    if inner.is_empty() {
      return;
    }

    let n = usize::from(inner.width);
    let mut heights = vec![0; n];
    let mut beyond = vec![false; n];
    for alarm in members.iter().map(|&i| &self.alarms[i]) {
      let cols = columns(history, alarm, n);
      for (i, (height, past)) in
        alarms::nearness(&cols, alarm.op, alarm.threshold).into_iter().enumerate()
      {
        heights[i] = heights[i].max(height);
        beyond[i] |= past;
      }
    }
    if heights.iter().all(|&height| height == 0) {
      let calm = Paragraph::new(Line::from(dim("nothing near a limit")).centered());
      f.render_widget(calm, Rect { y: inner.bottom() - 1, height: 1, ..inner });
      return;
    }
    let colors: Vec<Color> =
      beyond.iter().map(|&b| if b { theme::ALARM } else { theme::OK }).collect();
    let graph = Graph::new(&heights).max(FULL).colors(&colors).three_levels(self.three_levels);
    f.render_widget(graph, inner);
  }

  fn render_list(&mut self, f: &mut Frame, area: Rect, now: i64) {
    let count = dim(format!("{} alarms", self.alarms.len()));
    let inner = draw_box(f, area, Titles::new(heading("Alarms")).right(count));
    self.render_footer(f, area);
    if inner.height < 2 || inner.width < 2 {
      return;
    }
    if self.alarms.is_empty() {
      let message = match &self.error {
        Some(err) => Line::from(colored(err.clone(), theme::ALARM)),
        None => Line::from(dim("loading alarms…")),
      };
      f.render_widget(Paragraph::new(message).wrap(Wrap { trim: true }), inner);
      return;
    }

    let [names, rows] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(inner);
    let longest = self.alarms.iter().map(|a| Span::raw(self.short_name(a)).width()).max();
    let (name_width, graph_width) = list_widths(usize::from(inner.width), longest.unwrap_or(0));
    let span = self.history.as_ref().map_or(self.window.seconds(), |h| h.end - h.start);
    let column_names = format!(
      "  {} {} {:>5} {:<6} {:>4}  {}",
      fit("alarm", name_width, false),
      fit("group", GROUP, false),
      "now",
      "limit",
      "for",
      Window::label(span)
    );
    f.buffer_mut().set_line(names.x, names.y, &Line::from(dim(column_names)), names.width);

    self.page = usize::from(rows.height).max(1);
    self.selected = self.selected.min(self.alarms.len() - 1);
    self.offset = self.offset.min(self.selected).max((self.selected + 1).saturating_sub(self.page));
    self.offset = self.offset.min(self.alarms.len().saturating_sub(self.page));

    for (y, i) in (rows.y..rows.bottom()).zip(self.offset..self.alarms.len()) {
      let alarm = &self.alarms[i];
      let points = self.history.as_ref().and_then(|h| h.points.get(&alarm.name));
      let line = self.row(alarm, points, name_width, i == self.selected, now);
      let width = f.buffer_mut().set_line(rows.x, y, &line, rows.width).0 - rows.x;

      let graph_area = Rect { x: rows.x + width, y, width: graph_width as u16, height: 1 };
      if let (Some(history), true) = (&self.history, graph_width > 0) {
        let cols = columns(history, alarm, graph_width);
        let bars = alarms::nearness(&cols, alarm.op, alarm.threshold);
        let heights: Vec<u64> = bars.iter().map(|&(h, _)| h).collect();
        let colors: Vec<Color> =
          bars.iter().map(|&(_, b)| if b { theme::ALARM } else { theme::OK }).collect();
        // two eighths left empty at the top, so the rows' graphs don't run into each other
        let graph = Graph::new(&heights).max(FULL).colors(&colors).gap(2);
        let graph = graph.three_levels(self.three_levels);
        f.render_widget(graph, graph_area.intersection(rows));
      }
    }
    draw_scrollbar(f, area, rows, self.alarms.len(), self.offset);
  }

  /// The text columns of an alarm's row: state, name, group, the latest value, the threshold and
  /// how long the alarm has been in its state.
  fn row(
    &self,
    alarm: &Alarm,
    points: Option<&Points>,
    name_width: usize,
    selected: bool,
    now: i64,
  ) -> Line<'static> {
    let latest = points.and_then(|p| p.last()).map(|&(_, value)| value);
    let beyond = latest.is_some_and(|v| alarm.op.breaches(v, alarm.threshold));
    let value = latest.map_or("–".into(), alarms::number);
    let limit = format!("{}{}", alarm.op.symbol(), alarms::number(alarm.threshold));
    let mut spans = vec![
      text(format!("{} ", fit(self.short_name(alarm), name_width, false))),
      dim(format!("{} ", fit(&alarm.group, GROUP, false))),
      colored(fit(&value, 5, true), if beyond { theme::ALARM } else { theme::TEXT }),
      dim(format!(" {} ", fit(&limit, 6, false))),
      dim(format!("{}  ", fit(&alarms::age(now - alarm.changed), 4, true))),
    ];
    if selected {
      spans.iter_mut().for_each(|span| span.style = theme::SELECTED);
    }
    spans.insert(0, colored("● ", state_color(alarm.state)));
    Line::from(spans)
  }

  fn render_footer(&self, f: &mut Frame, area: Rect) {
    let window = format!("window {}", Window::label(self.window.seconds()));
    // an error takes the room of the hints it can do without
    let hints = match self.error {
      Some(_) => vec![hint("r", "refresh"), hint("q", "quit")],
      None => {
        vec![hint("r", "refresh"), hint("w", window), hint("Enter", "details"), hint("q", "quit")]
      }
    };
    let status = match (&self.error, self.fetching, self.updated) {
      (Some(err), ..) => vec![colored(err.clone(), theme::ALARM)],
      (None, true, _) => vec![dim("refreshing…")],
      (None, false, Some(updated)) => vec![dim("updated "), text(local(updated, "%H:%M:%S"))],
      (None, false, None) => vec![],
    };
    render_bottom_border(f, area, hints, |_| status);
  }

  fn render_details(&self, f: &mut Frame, area: Rect, now: i64) {
    let alarm = &self.alarms[self.selected];
    let width = area.width.saturating_sub(8).clamp(area.width.min(30), 100);
    let height = area.height.saturating_sub(4).clamp(area.height.min(8), 20);
    let rect = Rect {
      x: area.x + (area.width - width) / 2,
      y: area.y + (area.height - height) / 2,
      width,
      height,
    };
    f.render_widget(Clear, rect);
    let state = colored(alarm.state.label(), state_color(alarm.state));
    let inner = draw_box(f, rect, Titles::new(heading(alarm.name.clone())).right(state));
    render_bottom_border(f, rect, vec![hint("Esc", "close")], |_| vec![]);

    let label = |name: &str| dim(format!("{name:<8} "));
    let notifications = if alarm.actions { "notifications on" } else { "notifications off" };
    let fires = format!(
      "{} {} in {} of the last {} datapoints of {} · missing data: {}",
      alarm.op.symbol(),
      alarms::number(alarm.threshold),
      alarm.datapoints_to_alarm,
      alarm.evaluation_periods,
      alarms::age(alarm.period),
      alarm.missing_data,
    );
    let mut lines = vec![
      Line::from(vec![
        label("since"),
        text(local(alarm.changed, "%a %d %b %H:%M")),
        dim(format!(" ({} ago)", alarms::age(now - alarm.changed))),
      ]),
      Line::from(vec![
        label("group"),
        text(alarm.group.clone()),
        dim(format!(" · {notifications}")),
      ]),
      Line::from(vec![label("metric"), text(alarm.metric.clone())]),
      Line::from(vec![label("fires"), text(fires)]),
      Line::default(),
      Line::from(vec![label("reason"), text(alarm.reason.clone())]),
    ];
    if !alarm.description.is_empty() {
      lines.extend([
        Line::default(),
        Line::from(vec![label("about"), text(alarm.description.clone())]),
      ]);
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
  }

  fn short_name<'a>(&self, alarm: &'a Alarm) -> &'a str {
    alarm.name.strip_prefix(self.prefix.as_str()).unwrap_or(&alarm.name)
  }
}

/// Boxes for `n` groups in rows across the top of `area`, and the area left for the list. Boxes
/// are at least `BOX_WIDTH` cells wide; they have two graph rows, one when the list would get
/// fewer than `LIST_MIN` rows, and none show when even that is too much.
fn grid(area: Rect, n: usize) -> (Vec<Rect>, Rect) {
  if n == 0 {
    return (vec![], area);
  }
  let rows = n.div_ceil(usize::from(area.width / BOX_WIDTH).clamp(1, n));
  let per_row = n.div_ceil(rows);
  let fits = |height: u16| area.height.saturating_sub(height * rows as u16) >= LIST_MIN;
  let Some(height) = [4, 3].into_iter().find(|&height| fits(height)) else {
    return (vec![], area);
  };

  let [top, list] =
    Layout::vertical([Constraint::Length(height * rows as u16), Constraint::Fill(1)]).areas(area);
  let mut boxes = vec![];
  for row in 0..rows {
    let count = per_row.min(n - row * per_row);
    let row = Rect { y: top.y + row as u16 * height, height, ..top };
    boxes.extend(Layout::horizontal(vec![Constraint::Fill(1); count]).split(row).iter());
  }
  (boxes, list)
}

/// Widths of the name and the graph in a list `width` cells wide whose longest name is `longest`
/// cells: names up to `NAME_MAX`, cut down to `NAME_MIN` while the graph is narrower than
/// `GRAPH_MIN`.
fn list_widths(width: usize, longest: usize) -> (usize, usize) {
  let room = width.saturating_sub(TEXT_COLUMNS);
  let name = longest.min(NAME_MAX).min(room.saturating_sub(GRAPH_MIN)).max(NAME_MIN.min(longest));
  (name, room.saturating_sub(name))
}

/// `alarm`'s datapoints in `n` columns over the history's window.
fn columns(history: &History, alarm: &Alarm, n: usize) -> Vec<Option<f64>> {
  let points = history.points.get(&alarm.name).map_or(&[][..], Vec::as_slice);
  alarms::columns(points, alarm.period, history.start, history.end, n, alarm.op)
}

/// `text` in exactly `width` cells: cut with `…` when wider, padded with blanks when narrower, on
/// the right or with `right_aligned` on the left.
fn fit(text: &str, width: usize, right_aligned: bool) -> String {
  let cut;
  let text = if Span::raw(text).width() > width {
    cut = format!("{}{ELLIPSIS}", head(text, width.saturating_sub(1)));
    if width == 0 { "" } else { cut.as_str() }
  } else {
    text
  };
  let pad = " ".repeat(width.saturating_sub(Span::raw(text).width()));
  if right_aligned { format!("{pad}{text}") } else { format!("{text}{pad}") }
}

fn state_color(state: State) -> Color {
  match state {
    State::Alarm => theme::ALARM,
    State::NoData => theme::NO_DATA,
    State::Ok => theme::OK,
  }
}

/// Unix seconds as local time in `format`.
fn local(seconds: i64, format: &str) -> String {
  Local
    .timestamp_opt(seconds, 0)
    .single()
    .map_or_else(String::new, |t| t.format(format).to_string())
}

#[cfg(test)]
mod tests {
  use ratatui::crossterm::event::{KeyCode, KeyEvent};

  use super::*;
  use crate::alarms::tests::alarms;

  /// Unix seconds of the screens below: 2026-10-15 16:00 UTC.
  const NOW: i64 = 1_792_080_000;

  /// An app with the four test alarms and three hours of history: the CPU at 45 % every minute,
  /// the Kafka controller count at 1 then 0 for the last 15 minutes.
  fn app() -> App {
    let mut app = App::new("readonly".into(), "eu-west-1".into(), "acme-prod-".into());
    app.set_alarms(Ok(alarms()), NOW);
    let (start, end) = (NOW - 3 * 3600, NOW);
    let cpu = (start..end).step_by(60).map(|t| (t, 45.0)).collect();
    let kafka =
      (start..end).step_by(300).map(|t| (t, if t >= end - 900 { 0.0 } else { 1.0 })).collect();
    let points = HashMap::from([
      ("acme-prod-timescale-cpu".to_string(), cpu),
      ("acme-prod-kafka-active-controller".to_string(), kafka),
    ]);
    app.set_history(Ok(History { start, end, points, error: None }));
    app
  }

  fn press(app: &mut App, code: KeyCode) -> Action {
    app.handle_key(KeyEvent::from(code))
  }

  #[test]
  fn the_screen_shows_groups_then_alarms_worst_first() {
    let screen = app().screen(120, 30, NOW);
    let lines: Vec<&str> = screen.lines().collect();
    assert!(lines[0].starts_with(" opsmon  Alarms"), "{}", lines[0]);
    assert!(lines[0].ends_with("readonly · eu-west-1"), "{}", lines[0]);
    // four groups fit in one row of boxes, alphabetical
    for (group, status) in [("EC2", "1 ok"), ("Gateways", "1 no data/1"), ("Kafka", "1 alarm/1")] {
      assert!(lines[1].contains(&format!(" {group} ")), "{group}: {}", lines[1]);
      assert!(lines[1].contains(&format!(" {status} ")), "{status}: {}", lines[1]);
    }
    // EC2 and Gateways have no datapoints to draw
    assert_eq!(lines[3].matches("nothing near a limit").count(), 2, "{}", lines[3]);
    // the list: column names, then the alarm first, the prefix left out
    assert!(lines[6].contains("alarm") && lines[6].contains(" for  3h "), "{}", lines[6]);
    assert!(lines[7].starts_with("│● kafka-active-controller"), "{}", lines[7]);
    assert!(lines[7].contains("Kafka") && lines[7].contains("<1") && lines[7].contains("8h"));
    assert!(lines[8].contains("gateway-power-cut"), "{}", lines[8]);
    assert!(lines[9].contains("timescale-cpu") && lines[9].contains("   45 >90"), "{}", lines[9]);
    assert!(screen.contains("refresh") && screen.contains("quit"));
  }

  #[test]
  fn graphs_are_red_where_the_threshold_was_crossed() {
    let mut app = app();
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
    let buf = term.draw(|f| app.render(f, NOW)).unwrap().buffer.clone();
    // the Kafka controller count: red bars at the right end where it fell to 0, below the top
    // eighths left for the gap, and nothing while it was a steady 1
    let right = 118;
    assert_eq!((buf[(right, 7)].symbol(), buf[(right, 7)].fg), ("▆", theme::ALARM));
    assert!((60..right - 5).all(|x| buf[(x, 7)].symbol() == " "));
    // the CPU row: bars half way up to the gap (45 of 90) in green
    assert_eq!((buf[(right, 9)].symbol(), buf[(right, 9)].fg), ("▃", theme::OK));
  }

  #[test]
  fn keys_move_open_details_and_quit() {
    let mut app = app();
    let _ = app.screen(120, 30, NOW);
    assert!(matches!(press(&mut app, KeyCode::Char('j')), Action::None));
    assert_eq!(app.selected, 1);
    assert!(matches!(press(&mut app, KeyCode::End), Action::None));
    assert_eq!(app.selected, 3);
    assert!(matches!(press(&mut app, KeyCode::Down), Action::None));
    assert_eq!(app.selected, 3);
    press(&mut app, KeyCode::Home);
    press(&mut app, KeyCode::Enter);
    let screen = app.screen(120, 30, NOW);
    assert!(screen.contains("was less than the threshold"), "{screen}");
    assert!(screen.contains("< 1 in 1 of the last 1 datapoints of 5m"), "{screen}");
    press(&mut app, KeyCode::Esc);
    assert!(!app.details);
    assert!(matches!(press(&mut app, KeyCode::Char('w')), Action::Refresh));
    assert_eq!(app.window_seconds(), 24 * 3600);
    assert!(matches!(press(&mut app, KeyCode::Char('q')), Action::Quit));
  }

  #[test]
  fn new_alarms_keep_the_selection_and_errors_keep_the_alarms() {
    let mut app = app();
    app.selected = 2;
    let name = app.alarms[2].name.clone();
    app.set_alarms(Ok(alarms()), NOW + 60);
    assert_eq!(app.alarms[app.selected].name, name);
    app.set_alarms(Err("AWS login expired: run aws sso login --sso-session acme".into()), NOW);
    assert_eq!(app.alarms.len(), 4);
    let screen = app.screen(120, 30, NOW);
    assert!(screen.contains("AWS login expired"), "{screen}");
  }

  #[test]
  fn a_box_cuts_its_name_before_its_count() {
    // four boxes of 25 cells: "Gateways" and "1 no data/1" don't both fit
    let screen = app().screen(100, 30, NOW);
    let boxes = screen.lines().nth(1).unwrap();
    assert!(boxes.contains("─ Gate… ─ 1 no data/1 ─"), "{boxes}");
    assert!(boxes.contains(" Kafka ") && boxes.contains(" 1 alarm/1 "), "{boxes}");
  }

  #[test]
  fn errors_show_in_full_over_the_hints() {
    let mut app = app();
    // some calls failed: the graphs that came back, and the error
    let error = Some("Rate exceeded".to_string());
    app.set_history(Ok(History { start: NOW - 3600, end: NOW, points: HashMap::new(), error }));
    assert!(app.screen(120, 30, NOW).contains("Rate exceeded"));

    // the whole login hint at 80 columns, with the hints it needs
    app.set_alarms(Err("AWS login expired: run aws sso login --sso-session acme".into()), NOW);
    let screen = app.screen(80, 30, NOW);
    let footer = screen.lines().last().unwrap();
    assert!(footer.contains("AWS login expired: run aws sso login --sso-session acme"), "{footer}");
    assert!(footer.contains("refresh | quit"), "{footer}");
  }

  #[test]
  fn any_window_size_draws() {
    for (width, height) in (0..=60).flat_map(|w| (0..=24).map(move |h| (w * 3, h))) {
      let mut app = app();
      let _ = app.screen(width, height, NOW);
      app.details = true;
      let _ = app.screen(width, height, NOW);
    }
    let mut empty = App::new("p".into(), "r".into(), String::new());
    assert!(empty.screen(60, 10, NOW).contains("loading alarms"));
  }

  #[test]
  fn text_fits_its_column() {
    assert_eq!(fit("kafka", 7, false), "kafka  ");
    assert_eq!(fit("45", 5, true), "   45");
    assert_eq!(fit("timescale-inference-disk-ops", 10, false), "timescale…");
    assert_eq!(fit("abc", 0, false), "");
    // names cut to make room for the graph, but not below NAME_MIN
    assert_eq!(list_widths(120, 50), (NAME_MAX, 120 - TEXT_COLUMNS - NAME_MAX));
    assert_eq!(list_widths(60, 30), (NAME_MIN, 60 - TEXT_COLUMNS - NAME_MIN));
    assert_eq!(list_widths(10, 30), (NAME_MIN, 0));
  }
}
