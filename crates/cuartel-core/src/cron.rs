use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Datelike, NaiveDateTime, TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};

use crate::agent::AgentType;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CronError {
    InvalidField { field: &'static str, value: String },
    TooFewFields(usize),
    TooManyFields(usize),
    ValueOutOfRange { field: &'static str, value: u32, min: u32, max: u32 },
    NoNextTick,
}

impl fmt::Display for CronError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CronError::InvalidField { field, value } => {
                write!(f, "invalid cron {field}: {value:?}")
            }
            CronError::TooFewFields(n) => write!(f, "expected 5 cron fields, got {n}"),
            CronError::TooManyFields(n) => write!(f, "expected 5 cron fields, got {n}"),
            CronError::ValueOutOfRange { field, value, min, max } => {
                write!(f, "cron {field} value {value} out of range [{min}, {max}]")
            }
            CronError::NoNextTick => write!(f, "no matching time found within search window"),
        }
    }
}

impl std::error::Error for CronError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CronFieldValue {
    Any,
    Exact(u32),
    Range(u32, u32),
    Step(Box<CronFieldValue>, u32),
    List(Vec<CronFieldValue>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CronField {
    value: CronFieldValue,
    min: u32,
    max: u32,
}

impl CronField {
    fn matches(&self, val: u32) -> bool {
        self.value.matches(val, self.min, self.max)
    }
}

impl CronFieldValue {
    fn matches(&self, val: u32, min: u32, max: u32) -> bool {
        match self {
            CronFieldValue::Any => true,
            CronFieldValue::Exact(v) => val == *v,
            CronFieldValue::Range(lo, hi) => val >= *lo && val <= *hi,
            CronFieldValue::Step(base, step) => {
                if *step == 0 {
                    return false;
                }
                match base.as_ref() {
                    CronFieldValue::Any => (val - min) % step == 0,
                    CronFieldValue::Range(lo, hi) => {
                        val >= *lo && val <= *hi && (val - lo) % step == 0
                    }
                    _ => false,
                }
            }
            CronFieldValue::List(items) => items.iter().any(|i| i.matches(val, min, max)),
        }
    }
}

fn parse_field(input: &str, field_name: &'static str, min: u32, max: u32) -> Result<CronField, CronError> {
    let value = parse_field_value(input, field_name, min, max)?;
    Ok(CronField { value, min, max })
}

fn parse_field_value(input: &str, field_name: &'static str, min: u32, max: u32) -> Result<CronFieldValue, CronError> {
    if input.contains(',') {
        let parts: Result<Vec<CronFieldValue>, CronError> = input
            .split(',')
            .map(|p| parse_field_value(p.trim(), field_name, min, max))
            .collect();
        return Ok(CronFieldValue::List(parts?));
    }

    if let Some((base, step_str)) = input.split_once('/') {
        let step: u32 = step_str
            .parse()
            .map_err(|_| CronError::InvalidField { field: field_name, value: input.to_string() })?;
        if step == 0 {
            return Err(CronError::InvalidField { field: field_name, value: input.to_string() });
        }
        let base_value = parse_field_value(base, field_name, min, max)?;
        return Ok(CronFieldValue::Step(Box::new(base_value), step));
    }

    if let Some((lo_str, hi_str)) = input.split_once('-') {
        let lo: u32 = lo_str
            .parse()
            .map_err(|_| CronError::InvalidField { field: field_name, value: input.to_string() })?;
        let hi: u32 = hi_str
            .parse()
            .map_err(|_| CronError::InvalidField { field: field_name, value: input.to_string() })?;
        validate_range(lo, field_name, min, max)?;
        validate_range(hi, field_name, min, max)?;
        return Ok(CronFieldValue::Range(lo, hi));
    }

    if input == "*" {
        return Ok(CronFieldValue::Any);
    }

    let val: u32 = input
        .parse()
        .map_err(|_| CronError::InvalidField { field: field_name, value: input.to_string() })?;
    validate_range(val, field_name, min, max)?;
    Ok(CronFieldValue::Exact(val))
}

fn validate_range(val: u32, field: &'static str, min: u32, max: u32) -> Result<(), CronError> {
    if val < min || val > max {
        Err(CronError::ValueOutOfRange { field, value: val, min, max })
    } else {
        Ok(())
    }
}

/// Standard 5-field cron expression: minute hour day-of-month month day-of-week.
///
/// Supports `*`, exact values, ranges (`1-5`), steps (`*/15`, `0-30/5`),
/// and comma-separated lists (`1,3,5`). Day-of-week uses 0=Sunday.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CronExpr {
    minute: CronField,
    hour: CronField,
    day_of_month: CronField,
    month: CronField,
    day_of_week: CronField,
}

impl CronExpr {
    pub fn parse(expr: &str) -> Result<Self, CronError> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() < 5 {
            return Err(CronError::TooFewFields(fields.len()));
        }
        if fields.len() > 5 {
            return Err(CronError::TooManyFields(fields.len()));
        }
        Ok(Self {
            minute: parse_field(fields[0], "minute", 0, 59)?,
            hour: parse_field(fields[1], "hour", 0, 23)?,
            day_of_month: parse_field(fields[2], "day_of_month", 1, 31)?,
            month: parse_field(fields[3], "month", 1, 12)?,
            day_of_week: parse_field(fields[4], "day_of_week", 0, 6)?,
        })
    }

    pub fn matches(&self, dt: &DateTime<Utc>) -> bool {
        self.minute.matches(dt.minute())
            && self.hour.matches(dt.hour())
            && self.day_of_month.matches(dt.day())
            && self.month.matches(dt.month())
            && self.day_of_week.matches(dt.weekday().num_days_from_sunday())
    }

    pub fn next_tick(&self, after: &DateTime<Utc>) -> Result<DateTime<Utc>, CronError> {
        let mut candidate = *after + chrono::Duration::minutes(1);
        candidate = candidate
            .with_second(0)
            .and_then(|t| t.with_nanosecond(0))
            .unwrap_or(candidate);

        // Search up to 4 years to handle leap year edge cases.
        let limit = *after + chrono::Duration::days(366 * 4);

        while candidate <= limit {
            if self.matches(&candidate) {
                return Ok(candidate);
            }

            if !self.month.matches(candidate.month()) {
                candidate = advance_month(candidate);
                continue;
            }
            if !self.day_of_month.matches(candidate.day())
                || !self.day_of_week.matches(candidate.weekday().num_days_from_sunday())
            {
                candidate = advance_day(candidate);
                continue;
            }
            if !self.hour.matches(candidate.hour()) {
                candidate = advance_hour(candidate);
                continue;
            }
            candidate = candidate + chrono::Duration::minutes(1);
        }

        Err(CronError::NoNextTick)
    }
}

fn advance_month(dt: DateTime<Utc>) -> DateTime<Utc> {
    let (year, month) = if dt.month() == 12 {
        (dt.year() + 1, 1)
    } else {
        (dt.year(), dt.month() + 1)
    };
    Utc.from_utc_datetime(
        &NaiveDateTime::new(
            chrono::NaiveDate::from_ymd_opt(year, month, 1).unwrap_or(dt.naive_utc().date()),
            chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
        ),
    )
}

fn advance_day(dt: DateTime<Utc>) -> DateTime<Utc> {
    let next = dt.date_naive().succ_opt().unwrap_or(dt.date_naive());
    Utc.from_utc_datetime(&NaiveDateTime::new(
        next,
        chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
    ))
}

fn advance_hour(dt: DateTime<Utc>) -> DateTime<Utc> {
    let next = dt + chrono::Duration::hours(1);
    next.with_minute(0)
        .and_then(|t| t.with_second(0))
        .and_then(|t| t.with_nanosecond(0))
        .unwrap_or(next)
}

impl FromStr for CronExpr {
    type Err = CronError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        CronExpr::parse(s)
    }
}

impl fmt::Display for CronExpr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn field_str(field: &CronField) -> String {
            value_str(&field.value)
        }
        fn value_str(v: &CronFieldValue) -> String {
            match v {
                CronFieldValue::Any => "*".into(),
                CronFieldValue::Exact(n) => n.to_string(),
                CronFieldValue::Range(lo, hi) => format!("{lo}-{hi}"),
                CronFieldValue::Step(base, step) => format!("{}/{step}", value_str(base)),
                CronFieldValue::List(items) => {
                    items.iter().map(value_str).collect::<Vec<_>>().join(",")
                }
            }
        }
        write!(
            f,
            "{} {} {} {} {}",
            field_str(&self.minute),
            field_str(&self.hour),
            field_str(&self.day_of_month),
            field_str(&self.month),
            field_str(&self.day_of_week),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledJob {
    pub id: String,
    pub name: String,
    pub cron_expr: String,
    pub agent_type: AgentType,
    pub prompt: String,
    pub workspace_id: String,
    pub enabled: bool,
    pub last_run_at: Option<DateTime<Utc>>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl ScheduledJob {
    pub fn new(
        id: String,
        name: String,
        cron_expr: String,
        agent_type: AgentType,
        prompt: String,
        workspace_id: String,
    ) -> Result<Self, CronError> {
        let parsed = CronExpr::parse(&cron_expr)?;
        let now = Utc::now();
        let next = parsed.next_tick(&now)?;
        Ok(Self {
            id,
            name,
            cron_expr,
            agent_type,
            prompt,
            workspace_id,
            enabled: true,
            last_run_at: None,
            next_run_at: Some(next),
            created_at: now,
        })
    }

    pub fn advance_schedule(&mut self) -> Result<(), CronError> {
        let now = Utc::now();
        self.last_run_at = Some(now);
        let parsed = CronExpr::parse(&self.cron_expr)?;
        self.next_run_at = Some(parsed.next_tick(&now)?);
        Ok(())
    }

    pub fn is_due(&self, now: &DateTime<Utc>) -> bool {
        self.enabled && self.next_run_at.map_or(false, |next| *now >= next)
    }
}

pub fn collect_due_jobs<'a>(
    jobs: &'a [ScheduledJob],
    now: &DateTime<Utc>,
) -> Vec<&'a ScheduledJob> {
    jobs.iter().filter(|j| j.is_due(now)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    #[test]
    fn parse_every_minute() {
        let expr = CronExpr::parse("* * * * *").unwrap();
        assert!(expr.matches(&utc(2026, 1, 1, 0, 0)));
        assert!(expr.matches(&utc(2026, 6, 15, 12, 30)));
    }

    #[test]
    fn parse_specific_values() {
        let expr = CronExpr::parse("30 9 * * *").unwrap();
        assert!(expr.matches(&utc(2026, 4, 16, 9, 30)));
        assert!(!expr.matches(&utc(2026, 4, 16, 9, 31)));
        assert!(!expr.matches(&utc(2026, 4, 16, 10, 30)));
    }

    #[test]
    fn parse_range() {
        let expr = CronExpr::parse("0 9-17 * * *").unwrap();
        assert!(expr.matches(&utc(2026, 1, 1, 9, 0)));
        assert!(expr.matches(&utc(2026, 1, 1, 17, 0)));
        assert!(!expr.matches(&utc(2026, 1, 1, 8, 0)));
        assert!(!expr.matches(&utc(2026, 1, 1, 18, 0)));
    }

    #[test]
    fn parse_step() {
        let expr = CronExpr::parse("*/15 * * * *").unwrap();
        assert!(expr.matches(&utc(2026, 1, 1, 0, 0)));
        assert!(expr.matches(&utc(2026, 1, 1, 0, 15)));
        assert!(expr.matches(&utc(2026, 1, 1, 0, 30)));
        assert!(expr.matches(&utc(2026, 1, 1, 0, 45)));
        assert!(!expr.matches(&utc(2026, 1, 1, 0, 10)));
    }

    #[test]
    fn parse_range_with_step() {
        let expr = CronExpr::parse("0-30/10 * * * *").unwrap();
        assert!(expr.matches(&utc(2026, 1, 1, 0, 0)));
        assert!(expr.matches(&utc(2026, 1, 1, 0, 10)));
        assert!(expr.matches(&utc(2026, 1, 1, 0, 20)));
        assert!(expr.matches(&utc(2026, 1, 1, 0, 30)));
        assert!(!expr.matches(&utc(2026, 1, 1, 0, 40)));
    }

    #[test]
    fn parse_list() {
        let expr = CronExpr::parse("0 8,12,18 * * *").unwrap();
        assert!(expr.matches(&utc(2026, 1, 1, 8, 0)));
        assert!(expr.matches(&utc(2026, 1, 1, 12, 0)));
        assert!(expr.matches(&utc(2026, 1, 1, 18, 0)));
        assert!(!expr.matches(&utc(2026, 1, 1, 10, 0)));
    }

    #[test]
    fn parse_day_of_week() {
        // 2026-04-16 is a Thursday (4)
        let expr = CronExpr::parse("0 9 * * 1-5").unwrap();
        assert!(expr.matches(&utc(2026, 4, 16, 9, 0))); // Thursday
        // 2026-04-19 is a Sunday (0)
        assert!(!expr.matches(&utc(2026, 4, 19, 9, 0)));
    }

    #[test]
    fn parse_rejects_too_few_fields() {
        assert!(matches!(
            CronExpr::parse("* * *"),
            Err(CronError::TooFewFields(3))
        ));
    }

    #[test]
    fn parse_rejects_too_many_fields() {
        assert!(matches!(
            CronExpr::parse("* * * * * *"),
            Err(CronError::TooManyFields(6))
        ));
    }

    #[test]
    fn parse_rejects_out_of_range() {
        assert!(matches!(
            CronExpr::parse("60 * * * *"),
            Err(CronError::ValueOutOfRange { field: "minute", value: 60, .. })
        ));
        assert!(matches!(
            CronExpr::parse("* 25 * * *"),
            Err(CronError::ValueOutOfRange { field: "hour", value: 25, .. })
        ));
    }

    #[test]
    fn parse_rejects_invalid_token() {
        assert!(matches!(
            CronExpr::parse("abc * * * *"),
            Err(CronError::InvalidField { field: "minute", .. })
        ));
    }

    #[test]
    fn next_tick_every_minute() {
        let expr = CronExpr::parse("* * * * *").unwrap();
        let after = utc(2026, 4, 16, 12, 30);
        let next = expr.next_tick(&after).unwrap();
        assert_eq!(next, utc(2026, 4, 16, 12, 31));
    }

    #[test]
    fn next_tick_specific_time() {
        let expr = CronExpr::parse("0 9 * * *").unwrap();
        let after = utc(2026, 4, 16, 10, 0);
        let next = expr.next_tick(&after).unwrap();
        assert_eq!(next, utc(2026, 4, 17, 9, 0));
    }

    #[test]
    fn next_tick_same_day_future() {
        let expr = CronExpr::parse("0 18 * * *").unwrap();
        let after = utc(2026, 4, 16, 10, 0);
        let next = expr.next_tick(&after).unwrap();
        assert_eq!(next, utc(2026, 4, 16, 18, 0));
    }

    #[test]
    fn next_tick_crosses_month() {
        let expr = CronExpr::parse("0 0 1 * *").unwrap();
        let after = utc(2026, 4, 16, 0, 0);
        let next = expr.next_tick(&after).unwrap();
        assert_eq!(next, utc(2026, 5, 1, 0, 0));
    }

    #[test]
    fn next_tick_crosses_year() {
        let expr = CronExpr::parse("0 0 1 1 *").unwrap();
        let after = utc(2026, 6, 1, 0, 0);
        let next = expr.next_tick(&after).unwrap();
        assert_eq!(next, utc(2027, 1, 1, 0, 0));
    }

    #[test]
    fn next_tick_step_minutes() {
        let expr = CronExpr::parse("*/15 * * * *").unwrap();
        let after = utc(2026, 4, 16, 12, 16);
        let next = expr.next_tick(&after).unwrap();
        assert_eq!(next, utc(2026, 4, 16, 12, 30));
    }

    #[test]
    fn display_roundtrips() {
        let cases = ["* * * * *", "30 9 * * *", "*/15 * * * *", "0 9-17 * * 1-5"];
        for case in cases {
            let expr = CronExpr::parse(case).unwrap();
            let displayed = expr.to_string();
            let reparsed = CronExpr::parse(&displayed).unwrap();
            assert_eq!(expr, reparsed, "roundtrip failed for {case:?}");
        }
    }

    #[test]
    fn scheduled_job_creation_computes_next_run() {
        let job = ScheduledJob::new(
            "j1".into(),
            "Daily lint".into(),
            "0 9 * * *".into(),
            AgentType::Pi,
            "Run lint".into(),
            "ws-1".into(),
        )
        .unwrap();
        assert!(job.next_run_at.is_some());
        assert!(job.enabled);
        assert!(job.last_run_at.is_none());
    }

    #[test]
    fn scheduled_job_rejects_invalid_cron() {
        let err = ScheduledJob::new(
            "j2".into(),
            "Bad".into(),
            "bad expr".into(),
            AgentType::Pi,
            "".into(),
            "ws-1".into(),
        )
        .unwrap_err();
        assert!(matches!(err, CronError::TooFewFields(_)));
    }

    #[test]
    fn is_due_checks_enabled_and_time() {
        let mut job = ScheduledJob::new(
            "j3".into(),
            "Test".into(),
            "* * * * *".into(),
            AgentType::Pi,
            "test".into(),
            "ws-1".into(),
        )
        .unwrap();
        let future = Utc::now() + chrono::Duration::hours(1);
        assert!(job.is_due(&future));

        job.enabled = false;
        assert!(!job.is_due(&future));
    }

    #[test]
    fn advance_schedule_updates_last_and_next() {
        let mut job = ScheduledJob::new(
            "j4".into(),
            "Frequent".into(),
            "*/5 * * * *".into(),
            AgentType::ClaudeCode,
            "check".into(),
            "ws-1".into(),
        )
        .unwrap();
        assert!(job.last_run_at.is_none());
        assert!(job.next_run_at.is_some());
        job.advance_schedule().unwrap();
        assert!(job.last_run_at.is_some());
        assert!(job.next_run_at.is_some());
        assert!(job.next_run_at.unwrap() > job.last_run_at.unwrap());
    }

    #[test]
    fn collect_due_jobs_filters_correctly() {
        let j1 = ScheduledJob {
            id: "a".into(),
            name: "A".into(),
            cron_expr: "* * * * *".into(),
            agent_type: AgentType::Pi,
            prompt: "a".into(),
            workspace_id: "ws".into(),
            enabled: true,
            last_run_at: None,
            next_run_at: Some(utc(2026, 1, 1, 0, 0)),
            created_at: utc(2026, 1, 1, 0, 0),
        };
        let j2 = ScheduledJob {
            id: "b".into(),
            name: "B".into(),
            cron_expr: "* * * * *".into(),
            agent_type: AgentType::Pi,
            prompt: "b".into(),
            workspace_id: "ws".into(),
            enabled: true,
            last_run_at: None,
            next_run_at: Some(utc(2028, 1, 1, 0, 0)),
            created_at: utc(2026, 1, 1, 0, 0),
        };
        let jobs = vec![j1, j2];
        let due = collect_due_jobs(&jobs, &utc(2026, 6, 1, 0, 0));
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, "a");
    }
}
