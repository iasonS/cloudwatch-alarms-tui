//! The `aws` CLI with a read-only profile: the alarms, and the datapoints of their metrics.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::process::Command;
use std::thread;

use chrono::DateTime;
use serde_json::{Value, json};

use crate::alarms::{self, Alarm, Op, Points, Query};

/// Most queries in one `get-metric-data` call.
const MAX_QUERIES: usize = 500;

#[derive(Clone, Debug)]
pub struct Aws {
  pub profile: Option<String>,
  pub region: Option<String>,
  /// Named in the hint when the SSO login behind the profile has expired.
  pub sso_session: Option<String>,
}

/// Queries for one `get-metric-data` call, with the alarm (name, comparison) of each query id
/// whose result is wanted.
#[derive(Debug, Default)]
struct Batch {
  queries: Vec<Value>,
  alarms: HashMap<String, (String, Op)>,
}

impl Aws {
  fn run(&self, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("aws");
    command.args(args).args(["--output", "json"]);
    if let Some(profile) = &self.profile {
      command.args(["--profile", profile]);
    }
    if let Some(region) = &self.region {
      command.args(["--region", region]);
    }
    let out = command.env("AWS_PAGER", "").output().map_err(|err| match err.kind() {
      ErrorKind::NotFound => "aws CLI not found on PATH".to_string(),
      _ => format!("aws CLI: {err}"),
    })?;
    if !out.status.success() {
      return Err(explain(&String::from_utf8_lossy(&out.stderr), self.sso_session.as_deref()));
    }
    String::from_utf8(out.stdout).map_err(|err| format!("aws CLI: {err}"))
  }

  pub fn alarms(&self) -> Result<Vec<Alarm>, String> {
    alarms::parse_alarms(&self.run(&[
      "cloudwatch",
      "describe-alarms",
      "--alarm-types",
      "MetricAlarm",
    ])?)
  }

  /// Datapoints of every alarm's metric from `start` to `end` (unix seconds), by alarm name: one
  /// call for the single-metric alarms and one per metric-math alarm, all at once. Calls that
  /// fail leave their alarms out, and the first error comes with the rest; when all fail, that
  /// error is the result.
  pub fn history(
    &self,
    alarms: &[Alarm],
    start: i64,
    end: i64,
  ) -> Result<(HashMap<String, Points>, Option<String>), String> {
    let (start, end) = (iso(start), iso(end));
    let results: Vec<_> = thread::scope(|s| {
      let calls: Vec<_> = batches(alarms)
        .into_iter()
        .map(|batch| s.spawn(|| self.fetch(batch, &start, &end)))
        .collect();
      calls
        .into_iter()
        .map(|call| call.join().unwrap_or_else(|_| Err("fetch failed".into())))
        .collect()
    });

    let mut points = HashMap::new();
    let mut error = None;
    for result in results {
      match result {
        Ok(batch) => points.extend(batch),
        Err(err) => error = error.or(Some(err)),
      }
    }
    match error {
      Some(err) if points.is_empty() => Err(err),
      error => Ok((points, error)),
    }
  }

  fn fetch(&self, batch: Batch, start: &str, end: &str) -> Result<HashMap<String, Points>, String> {
    let queries = serde_json::to_string(&batch.queries).map_err(|err| err.to_string())?;
    let out = self.run(&[
      "cloudwatch",
      "get-metric-data",
      "--metric-data-queries",
      &queries,
      "--start-time",
      start,
      "--end-time",
      end,
    ])?;
    let op_of = |id: &str| batch.alarms.get(id).map_or(Op::Other, |&(_, op)| op);
    let by_id = alarms::parse_metric_data(&out, op_of)?;
    let named =
      by_id.into_iter().filter_map(|(id, points)| Some((batch.alarms.get(&id)?.0.clone(), points)));
    Ok(named.collect())
  }
}

/// The `get-metric-data` calls for `alarms`: their single metrics together (up to `MAX_QUERIES`
/// per call), then each metric-math alarm's own queries alone, as their ids may clash.
fn batches(alarms: &[Alarm]) -> Vec<Batch> {
  let mut single: Vec<Batch> = vec![];
  let mut math = vec![];
  for (i, alarm) in alarms.iter().enumerate() {
    match &alarm.query {
      Some(Query::Stat { namespace, metric, dimensions, stat }) => {
        if single.last().is_none_or(|batch| batch.queries.len() >= MAX_QUERIES) {
          single.push(Batch::default());
        }
        let batch = single.last_mut().unwrap();
        let id = format!("a{i}");
        let dimensions: Vec<Value> =
          dimensions.iter().map(|(name, value)| json!({"Name": name, "Value": value})).collect();
        batch.queries.push(json!({
          "Id": id,
          "MetricStat": {
            "Metric": {"Namespace": namespace, "MetricName": metric, "Dimensions": dimensions},
            "Period": alarm.period,
            "Stat": stat,
          },
          "ReturnData": true,
        }));
        batch.alarms.insert(id, (alarm.name.clone(), alarm.op));
      }
      Some(Query::Expr { queries, id }) => math.push(Batch {
        queries: queries.clone(),
        alarms: HashMap::from([(id.clone(), (alarm.name.clone(), alarm.op))]),
      }),
      None => {}
    }
  }
  single.extend(math);
  single
}

/// Unix seconds as the CLI takes them: `2026-10-15T16:04:00Z`.
fn iso(seconds: i64) -> String {
  DateTime::from_timestamp(seconds, 0).map_or_else(String::new, |t| t.format("%FT%TZ").to_string())
}

/// A short message for a failed `aws` call: how to log in again when the SSO login has expired,
/// else the CLI's last line.
fn explain(stderr: &str, sso_session: Option<&str>) -> String {
  let lower = stderr.to_lowercase();
  if lower.contains("sso") && (lower.contains("expired") || lower.contains("refresh failed")) {
    return match sso_session {
      Some(session) => format!("AWS login expired: run aws sso login --sso-session {session}"),
      None => "AWS login expired: run aws sso login".to_string(),
    };
  }
  let last = stderr.lines().map(str::trim).rfind(|line| !line.is_empty());
  last.unwrap_or("aws CLI failed").to_string()
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::alarms::tests::alarms;

  #[test]
  fn single_metrics_share_a_call_and_sql_queries_get_their_own() {
    let batches = batches(&alarms());
    assert_eq!(batches.len(), 2);

    let single = &batches[0];
    assert_eq!(single.queries.len(), 3);
    assert_eq!(single.alarms["a0"], ("acme-prod-timescale-cpu".to_string(), Op::Gt));
    assert_eq!(single.alarms["a1"].1, Op::Lt);
    let cpu = &single.queries[0];
    assert_eq!(cpu["MetricStat"]["Period"], 60);
    assert_eq!(cpu["MetricStat"]["Stat"], "Average");
    assert_eq!(cpu["MetricStat"]["Metric"]["Dimensions"][0]["Value"], "prod");

    let sql = &batches[1];
    assert_eq!(sql.queries[0]["Id"], "q1");
    assert_eq!(sql.alarms["q1"].0, "gateway-power-cut");
  }

  #[test]
  fn an_expired_sso_login_says_how_to_log_in() {
    let expired = "\nError when retrieving token from sso: Token has expired and refresh failed\n";
    assert_eq!(
      explain(expired, Some("acme")),
      "AWS login expired: run aws sso login --sso-session acme"
    );
    let denied =
      "\nAn error occurred (AccessDenied) when calling the DescribeAlarms operation: no\n\n";
    assert_eq!(
      explain(denied, None),
      "An error occurred (AccessDenied) when calling the DescribeAlarms operation: no"
    );
    assert_eq!(iso(0), "1970-01-01T00:00:00Z");
  }
}
