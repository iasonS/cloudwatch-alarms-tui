//! CloudWatch metric alarms as `describe-alarms` returns them, the datapoints of their metrics as
//! `get-metric-data` returns them, and those datapoints as graph columns.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};

use chrono::DateTime;
use serde::Deserialize;
use serde_json::Value;

/// Alarm state, worst first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
  Alarm,
  NoData,
  Ok,
}

impl State {
  fn parse(state: &str) -> Self {
    match state {
      "ALARM" => State::Alarm,
      "OK" => State::Ok,
      _ => State::NoData,
    }
  }

  pub fn label(self) -> &'static str {
    match self {
      State::Alarm => "ALARM",
      State::NoData => "NO DATA",
      State::Ok => "OK",
    }
  }
}

/// How a datapoint is compared with the threshold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
  Gt,
  Ge,
  Lt,
  Le,
  /// Anomaly-detection bands: no fixed threshold to draw against.
  Other,
}

impl Op {
  fn parse(op: &str) -> Self {
    match op {
      "GreaterThanThreshold" => Op::Gt,
      "GreaterThanOrEqualToThreshold" => Op::Ge,
      "LessThanThreshold" => Op::Lt,
      "LessThanOrEqualToThreshold" => Op::Le,
      _ => Op::Other,
    }
  }

  pub fn symbol(self) -> &'static str {
    match self {
      Op::Gt => ">",
      Op::Ge => "≥",
      Op::Lt => "<",
      Op::Le => "≤",
      Op::Other => "~",
    }
  }

  /// Whether `value` is beyond `threshold`.
  pub fn breaches(self, value: f64, threshold: f64) -> bool {
    match self {
      Op::Gt => value > threshold,
      Op::Ge => value >= threshold,
      Op::Lt => value < threshold,
      Op::Le => value <= threshold,
      Op::Other => false,
    }
  }

  /// Of two values, the one nearer to breaching: the lower one for a lower bound, else the
  /// higher one.
  pub fn worst(self, a: f64, b: f64) -> f64 {
    match self {
      Op::Lt | Op::Le => a.min(b),
      _ => a.max(b),
    }
  }
}

/// What to fetch for an alarm's graph.
#[derive(Clone, Debug)]
pub enum Query {
  /// One statistic of one metric.
  Stat { namespace: String, metric: String, dimensions: Vec<(String, String)>, stat: String },
  /// The alarm's own metric queries (metric math or a Metrics Insights SQL query), as they are;
  /// `id` is the one the alarm evaluates.
  Expr { queries: Vec<Value>, id: String },
}

#[derive(Clone, Debug)]
pub struct Alarm {
  pub name: String,
  pub description: String,
  pub state: State,
  pub reason: String,
  /// Unix seconds of the last state change.
  pub changed: i64,
  pub threshold: f64,
  pub op: Op,
  /// Seconds per datapoint.
  pub period: i64,
  pub evaluation_periods: u32,
  pub datapoints_to_alarm: u32,
  pub missing_data: String,
  /// Whether the alarm's actions (its notifications) are on.
  pub actions: bool,
  pub group: String,
  /// The metric in words: namespace, name and statistic, or the expression.
  pub metric: String,
  pub query: Option<Query>,
}

/// Datapoints of one metric: unix seconds and value, oldest first.
pub type Points = Vec<(i64, f64)>;

// MARK: describe-alarms

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Described {
  #[serde(default)]
  metric_alarms: Vec<Raw>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Raw {
  alarm_name: String,
  alarm_description: Option<String>,
  #[serde(default)]
  actions_enabled: bool,
  state_value: String,
  #[serde(default)]
  state_reason: String,
  state_updated_timestamp: Option<String>,
  state_transitioned_timestamp: Option<String>,
  metric_name: Option<String>,
  namespace: Option<String>,
  statistic: Option<String>,
  extended_statistic: Option<String>,
  #[serde(default)]
  dimensions: Vec<Dimension>,
  period: Option<i64>,
  #[serde(default)]
  evaluation_periods: u32,
  datapoints_to_alarm: Option<u32>,
  threshold: Option<f64>,
  comparison_operator: String,
  treat_missing_data: Option<String>,
  #[serde(default)]
  metrics: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Dimension {
  name: String,
  value: String,
}

/// The metric alarms in a `describe-alarms` output.
pub fn parse_alarms(json: &str) -> Result<Vec<Alarm>, String> {
  let described: Described = serde_json::from_str(json)
    .map_err(|err| format!("unexpected describe-alarms output: {err}"))?;
  Ok(described.metric_alarms.into_iter().map(Alarm::from_raw).collect())
}

impl Alarm {
  fn from_raw(raw: Raw) -> Self {
    let mut period = raw.period.unwrap_or(60);
    let mut namespace = String::new();
    let mut metric = String::new();
    let mut query = None;

    if let (Some(ns), Some(name)) = (&raw.namespace, &raw.metric_name) {
      let stat = raw.extended_statistic.or(raw.statistic).unwrap_or_else(|| "Average".into());
      namespace = ns.clone();
      metric = format!("{ns} {name} ({stat})");
      let dimensions = raw.dimensions.into_iter().map(|d| (d.name, d.value)).collect();
      query = Some(Query::Stat { namespace: ns.clone(), metric: name.clone(), dimensions, stat });
    } else if let Some(main) =
      raw.metrics.iter().find(|q| q["ReturnData"] == true).or(raw.metrics.first())
    {
      // a math expression has no period of its own: it takes the one of the metrics it reads
      period = main["Period"]
        .as_i64()
        .or(main["MetricStat"]["Period"].as_i64())
        .or_else(|| raw.metrics.iter().filter_map(|q| q["MetricStat"]["Period"].as_i64()).max())
        .unwrap_or(period);
      let expression = main["Expression"].as_str();
      namespace = expression
        .and_then(sql_namespace)
        .or_else(|| {
          raw.metrics.iter().find_map(|q| q["MetricStat"]["Metric"]["Namespace"].as_str())
        })
        .unwrap_or_default()
        .to_string();
      metric = expression.map_or_else(|| namespace.clone(), str::to_string);
      let id = main["Id"].as_str().unwrap_or_default().to_string();
      query = Some(Query::Expr { queries: raw.metrics.clone(), id });
    }

    let changed = raw.state_transitioned_timestamp.or(raw.state_updated_timestamp);
    let evaluation_periods = raw.evaluation_periods.max(1);
    Alarm {
      group: group_name(&namespace),
      name: raw.alarm_name,
      description: raw.alarm_description.unwrap_or_default(),
      state: State::parse(&raw.state_value),
      reason: raw.state_reason,
      changed: changed.as_deref().and_then(parse_time).unwrap_or(0),
      threshold: raw.threshold.unwrap_or(f64::NAN),
      op: Op::parse(&raw.comparison_operator),
      period: period.max(1),
      evaluation_periods,
      datapoints_to_alarm: raw.datapoints_to_alarm.unwrap_or(evaluation_periods),
      missing_data: raw.treat_missing_data.unwrap_or_else(|| "missing".into()),
      actions: raw.actions_enabled,
      metric,
      query,
    }
  }
}

/// The namespace a Metrics Insights query reads: `FROM "AWS/EC2"` or `FROM SCHEMA(ns, dims)`.
fn sql_namespace(expression: &str) -> Option<&str> {
  // ASCII upper-casing keeps byte offsets
  let from = expression.to_ascii_uppercase().find("FROM ")? + 5;
  let mut rest = expression[from..].trim_start();
  if rest.get(..7).is_some_and(|s| s.eq_ignore_ascii_case("SCHEMA(")) {
    rest = rest[7..].trim_start();
  }
  let namespace = match rest.strip_prefix('"') {
    Some(quoted) => &quoted[..quoted.find('"')?],
    None => rest.split([',', ')', ' ']).next()?,
  };
  (!namespace.is_empty()).then_some(namespace)
}

/// Box name for a namespace: the last part of it (`AWS/Kafka` is Kafka), or a plainer word.
pub fn group_name(namespace: &str) -> String {
  match namespace {
    "AWS/ApplicationELB" => "Load balancer",
    "ContainerInsights" => "EKS",
    "" => "Other",
    other => other.rsplit('/').next().unwrap_or(other),
  }
  .to_string()
}

fn parse_time(time: &str) -> Option<i64> {
  DateTime::parse_from_rfc3339(time).ok().map(|t| t.timestamp())
}

/// Alarms in ALARM first, then without data, then OK. Within each, by `band` (see `band`), highest
/// first, then by group and name, so the rows with bars come together and the calm ones sink.
pub fn sort(alarms: &mut [Alarm], band: impl Fn(&Alarm) -> u8) {
  alarms.sort_by_cached_key(|a| (a.state, Reverse(band(a)), a.group.clone(), a.name.clone()));
}

/// Group names in alphabetical order, each with the indices of its alarms in `alarms`' order.
pub fn groups(alarms: &[Alarm]) -> Vec<(String, Vec<usize>)> {
  let mut groups: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
  for (i, alarm) in alarms.iter().enumerate() {
    groups.entry(&alarm.group).or_default().push(i);
  }
  groups.into_iter().map(|(name, alarms)| (name.to_string(), alarms)).collect()
}

// MARK: get-metric-data

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct MetricData {
  #[serde(default)]
  metric_data_results: Vec<MetricResult>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct MetricResult {
  id: String,
  #[serde(default)]
  timestamps: Vec<String>,
  #[serde(default)]
  values: Vec<f64>,
}

/// Datapoints by query id in a `get-metric-data` output. A query can come back as several results:
/// one per group of a SQL query with `GROUP BY`, or one per page. They are merged into the worst
/// value at each timestamp, by the comparison `op_of(id)`.
pub fn parse_metric_data(
  json: &str,
  op_of: impl Fn(&str) -> Op,
) -> Result<HashMap<String, Points>, String> {
  let data: MetricData = serde_json::from_str(json)
    .map_err(|err| format!("unexpected get-metric-data output: {err}"))?;
  let mut merged: HashMap<String, BTreeMap<i64, f64>> = HashMap::new();
  for result in data.metric_data_results {
    let op = op_of(&result.id);
    let points = merged.entry(result.id).or_default();
    for (time, value) in result.timestamps.iter().zip(result.values) {
      if let Some(time) = parse_time(time) {
        points.entry(time).and_modify(|v| *v = op.worst(*v, value)).or_insert(value);
      }
    }
  }
  Ok(merged.into_iter().map(|(id, points)| (id, points.into_iter().collect())).collect())
}

// MARK: Graph columns

/// Height of a bar at the threshold, and of every bar beyond it.
pub const FULL: u64 = 1000;

/// The values of `points` in `n` columns over `[start, end)`, oldest first. A datapoint covers its
/// period from its timestamp, and a column takes the worst value (by `op`) of the datapoints that
/// cover any of it; a column that none covers is `None`.
pub fn columns(
  points: &[(i64, f64)],
  period: i64,
  start: i64,
  end: i64,
  n: usize,
  op: Op,
) -> Vec<Option<f64>> {
  let mut cols = vec![None; n];
  if n == 0 || end <= start {
    return cols;
  }

  let (span, n_) = (i128::from(end - start), n as i128);
  for &(time, value) in points {
    let from = i128::from(time - start);
    let to = from + i128::from(period.max(1));
    if !value.is_finite() || to <= 0 || from >= span {
      continue;
    }

    let first = (from.max(0) * n_ / span) as usize;
    // rounded up: the last column `to` reaches into
    let last = ((to.min(span) * n_ + span - 1) / span - 1) as usize;
    for col in &mut cols[first..=last.min(n - 1)] {
      *col = Some(col.map_or(value, |c| op.worst(c, value)));
    }
  }
  cols
}

/// Share of the way to the threshold a row has to reach somewhere to be drawn at all: a row that
/// stays further from its limit says nothing at a glance, and thin bars on every row make the rows
/// hard to tell apart.
const QUIET: f64 = 1.0 / 8.0;

/// How near each column came to the threshold, newest first as `Graph` draws them: `(height out of
/// FULL, beyond the threshold)`. A column beyond it is a full bar. Otherwise the height is the
/// share of the way to the threshold: up from zero to an upper limit; down from the largest value
/// shown to a lower limit, so a steady healthy value (a healthy-host count, a burst balance) draws
/// nothing; without a fixed limit, the value as a share of the largest one. A row that never comes
/// `QUIET` of the way draws nothing; one that does draws every column, so it isn't broken up.
pub fn nearness(cols: &[Option<f64>], op: Op, threshold: f64) -> Vec<(u64, bool)> {
  let largest = cols.iter().flatten().copied().fold(f64::NAN, f64::max);
  let way = |value: f64| match op {
    Op::Gt | Op::Ge => value / threshold,
    Op::Lt | Op::Le => (largest - value) / (largest - threshold),
    Op::Other => value / largest,
  };
  let reaches = |value: f64| op.breaches(value, threshold) || way(value) >= QUIET;
  let quiet = !cols.iter().flatten().any(|&value| reaches(value));
  cols
    .iter()
    .rev()
    .map(|col| match *col {
      None => (0, false),
      Some(value) if op.breaches(value, threshold) => (FULL, true),
      Some(_) if quiet => (0, false),
      Some(value) => (height(way(value)), false),
    })
    .collect()
}

/// A share of the way to the threshold as a bar height out of `FULL`; none when there is no way to
/// speak of (`0 / 0`: a zero against a zero threshold, a flag that never moved).
fn height(way: f64) -> u64 {
  if way.is_nan() || way <= 0.0 {
    return 0;
  }
  (way.min(1.0) * FULL as f64).round() as u64
}

/// How near `points` came to the threshold, for sorting: 3 beyond it, 2 two thirds of the way or
/// more, 1 drawn at all, 0 not drawn.
pub fn band(points: &[(i64, f64)], op: Op, threshold: f64) -> u8 {
  let cols: Vec<Option<f64>> = points.iter().map(|&(_, value)| Some(value)).collect();
  let bars = nearness(&cols, op, threshold);
  let highest = bars.iter().map(|&(height, _)| height).max().unwrap_or(0);
  match highest {
    _ if bars.iter().any(|&(_, beyond)| beyond) => 3,
    height if 3 * height >= 2 * FULL => 2,
    0 => 0,
    _ => 1,
  }
}

// MARK: Text

/// A value in at most 5 cells: `0.61`, `12.5`, `437`, `4.5k`, `12k`, `1.2M`.
pub fn number(value: f64) -> String {
  let size = value.abs();
  let short = |v: f64, unit: &str| {
    let text = if v.abs() >= 10.0 { format!("{v:.0}") } else { format!("{v:.1}") };
    format!("{}{unit}", text.strip_suffix(".0").unwrap_or(&text))
  };
  if !value.is_finite() {
    "–".into()
  } else if size >= 1e9 {
    short(value / 1e9, "G")
  } else if size >= 1e6 {
    short(value / 1e6, "M")
  } else if size >= 1e3 {
    short(value / 1e3, "k")
  } else if size >= 100.0 || value.fract() == 0.0 {
    format!("{value:.0}")
  } else if size >= 10.0 {
    format!("{value:.1}")
  } else {
    format!("{value:.2}")
  }
}

/// Seconds as the largest whole unit: `45s`, `12m`, `5h`, `3d`.
pub fn age(seconds: i64) -> String {
  match seconds.max(0) {
    s if s < 60 => format!("{s}s"),
    s if s < 3600 => format!("{}m", s / 60),
    s if s < 86400 => format!("{}h", s / 3600),
    s => format!("{}d", s / 86400),
  }
}

#[cfg(test)]
pub(crate) mod tests {
  use super::*;

  /// Four alarms in the shape `describe-alarms` returns them: a statistic against an upper bound,
  /// one against a lower bound, a SQL query with GROUP BY, and one against `≥`.
  pub(crate) const ALARMS: &str = r#"{
    "MetricAlarms": [
      {
        "AlarmName": "acme-prod-timescale-cpu",
        "AlarmDescription": "Timescale prod CPU is high",
        "ActionsEnabled": true,
        "StateValue": "OK",
        "StateReason": "Threshold Crossed: 3 datapoints were not greater than the threshold (90.0).",
        "StateUpdatedTimestamp": "2026-10-15T09:00:00.000000+02:00",
        "StateTransitionedTimestamp": "2026-10-14T09:00:00.000000+02:00",
        "MetricName": "CpuPercent",
        "Namespace": "Acme/Timescale",
        "Statistic": "Average",
        "Dimensions": [{"Name": "Service", "Value": "prod"}],
        "Period": 60,
        "EvaluationPeriods": 3,
        "DatapointsToAlarm": 3,
        "Threshold": 90.0,
        "ComparisonOperator": "GreaterThanThreshold",
        "TreatMissingData": "notBreaching"
      },
      {
        "AlarmName": "acme-prod-kafka-active-controller",
        "ActionsEnabled": true,
        "StateValue": "ALARM",
        "StateReason": "Threshold Crossed: 1 datapoint [0.0] was less than the threshold (1.0).",
        "StateUpdatedTimestamp": "2026-10-15T08:00:00.000000Z",
        "MetricName": "ActiveControllerCount",
        "Namespace": "AWS/Kafka",
        "Statistic": "Maximum",
        "Dimensions": [{"Name": "Cluster Name", "Value": "acme-prod-kafka"}],
        "Period": 300,
        "EvaluationPeriods": 1,
        "Threshold": 1.0,
        "ComparisonOperator": "LessThanThreshold",
        "TreatMissingData": "breaching"
      },
      {
        "AlarmName": "gateway-power-cut",
        "ActionsEnabled": false,
        "StateValue": "INSUFFICIENT_DATA",
        "StateReason": "Insufficient Data",
        "StateUpdatedTimestamp": "2026-10-15T07:00:00.000000Z",
        "Metrics": [{
          "Id": "q1",
          "Expression": "SELECT SUM(PowerCut) FROM SCHEMA(\"Acme/Gateways\", Gateway) GROUP BY Gateway",
          "ReturnData": true,
          "Period": 900
        }],
        "EvaluationPeriods": 1,
        "Threshold": 1.0,
        "ComparisonOperator": "GreaterThanThreshold"
      },
      {
        "AlarmName": "acme-prod-jenkins-status-check",
        "ActionsEnabled": true,
        "StateValue": "OK",
        "StateReason": "ok",
        "StateUpdatedTimestamp": "2026-10-13T00:00:00.000000Z",
        "MetricName": "StatusCheckFailed",
        "Namespace": "AWS/EC2",
        "Statistic": "Maximum",
        "Period": 60,
        "EvaluationPeriods": 2,
        "Threshold": 1.0,
        "ComparisonOperator": "GreaterThanOrEqualToThreshold"
      }
    ],
    "CompositeAlarms": []
  }"#;

  pub(crate) fn alarms() -> Vec<Alarm> {
    parse_alarms(ALARMS).unwrap()
  }

  #[test]
  fn reads_statistic_and_sql_alarms() {
    let alarms = alarms();
    let cpu = &alarms[0];
    assert_eq!((cpu.group.as_str(), cpu.state, cpu.op), ("Timescale", State::Ok, Op::Gt));
    assert_eq!((cpu.threshold, cpu.period, cpu.evaluation_periods), (90.0, 60, 3));
    assert_eq!(cpu.metric, "Acme/Timescale CpuPercent (Average)");
    // the last state change, not the last update
    assert_eq!(cpu.changed, parse_time("2026-10-14T07:00:00Z").unwrap());
    let Some(Query::Stat { dimensions, stat, .. }) = &cpu.query else { panic!() };
    assert_eq!((dimensions[0].1.as_str(), stat.as_str()), ("prod", "Average"));

    let gateways = &alarms[2];
    assert_eq!((gateways.group.as_str(), gateways.state), ("Gateways", State::NoData));
    assert_eq!((gateways.period, gateways.actions, gateways.datapoints_to_alarm), (900, false, 1));
    let Some(Query::Expr { id, queries }) = &gateways.query else { panic!() };
    assert_eq!((id.as_str(), queries.len()), ("q1", 1));
    assert!(gateways.metric.starts_with("SELECT SUM(PowerCut)"));

    assert_eq!((alarms[1].group.as_str(), alarms[1].op), ("Kafka", Op::Lt));
    assert_eq!((alarms[3].group.as_str(), alarms[3].op), ("EC2", Op::Ge));
  }

  #[test]
  fn finds_the_namespace_of_a_sql_query() {
    let cases = [
      (r#"SELECT SUM(x) FROM SCHEMA("Acme/Gateways", Gateway) GROUP BY Gateway"#, "Acme/Gateways"),
      (
        "SELECT MAX(m) FROM SCHEMA(ContainerInsights, ClusterName) WHERE a = 'b'",
        "ContainerInsights",
      ),
      (r#"select avg(CPUUtilization) from "AWS/EC2""#, "AWS/EC2"),
      ("SELECT AVG(CPUUtilization) FROM AWS/EC2 WHERE x = 1", "AWS/EC2"),
    ];
    for (sql, namespace) in cases {
      assert_eq!(sql_namespace(sql), Some(namespace), "{sql}");
    }
    assert_eq!(sql_namespace("m1 / m2 * 100"), None);
    assert_eq!(group_name("AWS/RDS"), "RDS");
    assert_eq!(group_name("ContainerInsights"), "EKS");
  }

  #[test]
  fn a_math_alarm_takes_the_period_of_its_metrics() {
    let json = r#"{"MetricAlarms": [{
      "AlarmName": "error-rate",
      "StateValue": "OK",
      "StateUpdatedTimestamp": "2026-10-15T07:00:00.000000Z",
      "Metrics": [
        {"Id": "e1", "Expression": "m1 / m2 * 100", "ReturnData": true},
        {"Id": "m1", "ReturnData": false, "MetricStat": {"Metric": {"Namespace": "AWS/ApplicationELB",
          "MetricName": "HTTPCode_Target_5XX_Count"}, "Period": 300, "Stat": "Sum"}},
        {"Id": "m2", "ReturnData": false, "MetricStat": {"Metric": {"Namespace": "AWS/ApplicationELB",
          "MetricName": "RequestCount"}, "Period": 300, "Stat": "Sum"}}
      ],
      "EvaluationPeriods": 3,
      "Threshold": 5.0,
      "ComparisonOperator": "GreaterThanThreshold"
    }]}"#;
    let alarm = &parse_alarms(json).unwrap()[0];
    assert_eq!((alarm.period, alarm.group.as_str()), (300, "Load balancer"));
    assert_eq!(alarm.metric, "m1 / m2 * 100");
    let Some(Query::Expr { id, queries }) = &alarm.query else { panic!() };
    assert_eq!((id.as_str(), queries.len()), ("e1", 3));
  }

  #[test]
  fn sorts_by_state_then_by_how_near_then_by_group() {
    let names = |alarms: &[Alarm]| alarms.iter().map(|a| a.name.clone()).collect::<Vec<_>>();
    let mut alarms = alarms();
    sort(&mut alarms, |_| 0);
    assert_eq!(
      names(&alarms),
      [
        "acme-prod-kafka-active-controller",
        "gateway-power-cut",
        "acme-prod-jenkins-status-check",
        "acme-prod-timescale-cpu"
      ]
    );
    // an OK alarm that came near its threshold goes before the calm ones
    sort(&mut alarms, |a| if a.name.ends_with("timescale-cpu") { 2 } else { 0 });
    assert_eq!(names(&alarms)[2..], ["acme-prod-timescale-cpu", "acme-prod-jenkins-status-check"]);

    let groups: Vec<String> = groups(&alarms).into_iter().map(|(name, _)| name).collect();
    assert_eq!(groups, ["EC2", "Gateways", "Kafka", "Timescale"]);
  }

  #[test]
  fn merges_grouped_series_into_the_worst_value() {
    let json = r#"{"MetricDataResults": [
      {"Id": "q1", "Label": "1 - gw-a", "Timestamps": ["2026-10-15T10:00:00Z", "2026-10-15T10:15:00Z"], "Values": [1.0, 4.0], "StatusCode": "Complete"},
      {"Id": "q1", "Label": "2 - gw-b", "Timestamps": ["2026-10-15T10:00:00+02:00"], "Values": [3.0], "StatusCode": "Complete"},
      {"Id": "a0", "Timestamps": ["2026-10-15T10:01:00Z", "2026-10-15T10:00:00Z"], "Values": [1.0, 0.0], "StatusCode": "Complete"}
    ], "Messages": []}"#;
    let op = |id: &str| if id == "a0" { Op::Lt } else { Op::Gt };
    let data = parse_metric_data(json, op).unwrap();
    let t = |s| parse_time(s).unwrap();
    // gw-b's 10:00 local is 08:00 UTC, so it doesn't meet gw-a
    assert_eq!(
      data["q1"],
      [
        (t("2026-10-15T08:00:00Z"), 3.0),
        (t("2026-10-15T10:00:00Z"), 1.0),
        (t("2026-10-15T10:15:00Z"), 4.0)
      ]
    );
    // oldest first, whatever order they came in
    assert_eq!(data["a0"], [(t("2026-10-15T10:00:00Z"), 0.0), (t("2026-10-15T10:01:00Z"), 1.0)]);
  }

  #[test]
  fn columns_take_the_worst_datapoint_that_covers_them() {
    // 10 minutes in 5 columns of 2 minutes; 1-minute datapoints
    let points = [(0, 1.0), (60, 5.0), (120, 2.0), (420, 7.0)];
    let cols = columns(&points, 60, 0, 600, 5, Op::Gt);
    assert_eq!(cols, [Some(5.0), Some(2.0), None, Some(7.0), None]);
    // against a lower bound the lowest value is the worst
    assert_eq!(columns(&points, 60, 0, 600, 5, Op::Lt)[0], Some(1.0));
    // a 5-minute datapoint covers every column of its period, and ones outside the window none
    let cols = columns(&[(-600, 9.0), (0, 3.0), (600, 9.0)], 300, 0, 600, 5, Op::Gt);
    assert_eq!(cols, [Some(3.0), Some(3.0), Some(3.0), None, None]);
    // one wide column takes all of them; no columns, no panic
    assert_eq!(columns(&points, 60, 0, 600, 1, Op::Gt), [Some(7.0)]);
    assert!(columns(&points, 60, 0, 600, 0, Op::Gt).is_empty());
  }

  #[test]
  fn bars_are_the_share_of_the_way_to_the_threshold() {
    // oldest first in, newest first out; beyond the threshold a full bar
    let cols = [Some(45.0), None, Some(95.0), Some(0.0)];
    assert_eq!(nearness(&cols, Op::Gt, 90.0), [(0, false), (FULL, true), (0, false), (500, false)]);
    // a lower bound: how far it dropped from the largest value toward the threshold
    let cols = [Some(100.0), Some(60.0), Some(40.0)];
    assert_eq!(nearness(&cols, Op::Lt, 50.0), [(FULL, true), (800, false), (0, false)]);
    // a healthy flag that never moved draws nothing, a zero beyond it a full bar
    assert_eq!(nearness(&[Some(1.0), Some(1.0)], Op::Lt, 1.0), [(0, false), (0, false)]);
    assert_eq!(nearness(&[Some(1.0), Some(0.0)], Op::Lt, 1.0), [(FULL, true), (0, false)]);
    // a threshold of zero: any value beyond it, a zero nothing
    assert_eq!(nearness(&[Some(0.0), Some(2.0)], Op::Gt, 0.0), [(FULL, true), (0, false)]);
  }

  #[test]
  fn a_row_far_from_its_threshold_stays_blank() {
    // never an eighth of the way: nothing; once somewhere: every column, small ones too
    assert_eq!(nearness(&[Some(11.0), Some(11.2)], Op::Gt, 90.0), [(0, false), (0, false)]);
    assert_eq!(
      nearness(&[Some(11.0), Some(11.25), Some(30.0)], Op::Gt, 90.0),
      [(333, false), (125, false), (122, false)]
    );
    // a burst balance at 99 of 100 against 50 hasn't gone anywhere
    assert_eq!(nearness(&[Some(100.0), Some(99.0)], Op::Lt, 50.0), [(0, false), (0, false)]);
  }

  #[test]
  fn bands_sort_beyond_then_near_then_drawn() {
    let band_of = |value| band(&[(0, 1.0), (60, value)], Op::Gt, 90.0);
    assert_eq!([95.0, 70.0, 45.0, 5.0].map(band_of), [3, 2, 1, 0]);
    assert_eq!(band(&[], Op::Gt, 90.0), 0);
  }

  #[test]
  fn numbers_fit_in_five_cells() {
    let cases = [
      (0.0, "0"),
      (0.6016, "0.60"),
      (12.46, "12.5"),
      (90.0, "90"),
      (437.2, "437"),
      (2000.0, "2k"),
      (4500.0, "4.5k"),
      (12_345.0, "12k"),
      (1_260_000.0, "1.3M"),
      (-3.0, "-3"),
      (f64::NAN, "–"),
    ];
    for (value, text) in cases {
      assert_eq!(number(value), text, "{value}");
    }
    assert_eq!([59, 60, 7199, 86400 * 3].map(age), ["59s", "1m", "1h", "3d"]);
  }
}
