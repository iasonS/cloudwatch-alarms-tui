# opsmon

The CloudWatch alarms of one AWS account in the terminal, in the look of
[macmon](https://github.com/vladkens/macmon). It only reads: every call goes through the `aws` CLI
with a read-only profile.

## Run

    opsmon                   # the AWS CLI's default profile and region
    opsmon --profile readonly --region eu-west-1 --prefix prod-
    opsmon --print 150x48    # one screen as text, then exit
    opsmon --help            # refresh interval, SSO login hint

It needs the `aws` CLI with credentials that may read CloudWatch (`cloudwatch:DescribeAlarms`,
`cloudwatch:GetMetricData`).

## What it shows

- A box per group of alarms, grouped by metric namespace (Kafka, Timescale, EKS, Logs…): how many
  are in ALARM or without data, and a graph of how near the nearest alarm came to its threshold.
- The alarms: ALARM first, then no data, then OK. Within each, the ones that crossed their limit
  in the window come first, then those that came within a third of it, then those with any bars,
  then the calm ones, by group and name. Each row has the latest value, the threshold, how long the
  alarm has been in its state, and a graph of its metric.
- A bar is how near the alarm came to its limit: up from zero to an upper limit, or down from its
  usual level to a lower limit (healthy hosts, burst balance), so a healthy alarm draws nothing.
  A row that never comes an eighth of the way draws nothing either. Red bars crossed the limit.
- "Wrong" means what the alarms say. Something that should be watched and isn't gets an alarm in
  Terraform, not a rule here.

## Keys

`r` refresh · `w` 3h / 24h graphs · `↑↓` `j` `k` `PgUp` `PgDn` select · `Enter` details ·
`Esc` close · `q` quit

## How it fetches

On start, every minute and on `r`: `describe-alarms`, then `get-metric-data` for each alarm's own
metric at its own period. The single-metric alarms share one call; each metric-math or SQL alarm
gets its own, and the calls run side by side. CloudWatch bills `get-metric-data` per metric.

## Build

    brew install rust
    cargo build --release    # target/release/opsmon
    cargo test

The box, graph and color code comes from macmon (MIT, see `LICENSE-macmon`).
