use std::{fmt, str::FromStr};

use anyhow::{Result, bail};
use chrono::{DateTime, NaiveDate, Utc};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Active,
    Waiting,
    Blocked,
    Done,
    Cancelled,
    Deleted,
}

impl Status {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Waiting => "waiting",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
            Self::Deleted => "deleted",
        }
    }

    pub const fn is_actionable(self) -> bool {
        matches!(
            self,
            Self::Pending | Self::Active | Self::Waiting | Self::Blocked
        )
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

impl FromStr for Status {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "active" => Ok(Self::Active),
            "waiting" => Ok(Self::Waiting),
            "blocked" => Ok(Self::Blocked),
            "done" => Ok(Self::Done),
            "cancelled" => Ok(Self::Cancelled),
            "deleted" => Ok(Self::Deleted),
            _ => bail!("invalid status: {value}"),
        }
    }
}

#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ValueEnum,
)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    High,
    #[default]
    Normal,
    Low,
}

impl Priority {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Normal => "normal",
            Self::Low => "low",
        }
    }
}

impl fmt::Display for Priority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

impl FromStr for Priority {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "high" => Ok(Self::High),
            "normal" => Ok(Self::Normal),
            "low" => Ok(Self::Low),
            _ => bail!("invalid priority: {value}"),
        }
    }
}

/// Dates are optional independently; planning after a deadline is valid.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    pub planned_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
    pub priority: Priority,
}

impl Schedule {
    pub fn validate(self) -> Result<()> {
        for date in [self.planned_date, self.due_date].into_iter().flatten() {
            parse_date(&date.to_string()).map_err(anyhow::Error::msg)?;
        }
        Ok(())
    }
}

/// None means unchanged; Some(None) explicitly clears a date.
#[derive(Debug, Default, Clone, Copy)]
pub struct ScheduleUpdate {
    pub planned_date: Option<Option<NaiveDate>>,
    pub due_date: Option<Option<NaiveDate>>,
    pub priority: Option<Priority>,
}

impl ScheduleUpdate {
    pub fn apply(self, current: Schedule) -> Schedule {
        Schedule {
            planned_date: self.planned_date.unwrap_or(current.planned_date),
            due_date: self.due_date.unwrap_or(current.due_date),
            priority: self.priority.unwrap_or(current.priority),
        }
    }
}

pub fn parse_date(value: &str) -> std::result::Result<NaiveDate, String> {
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| "expected a valid YYYY-MM-DD date (years 0001–9999)".to_owned())?;
    if value.len() != 10 || date.to_string() != value || value.starts_with("0000") {
        return Err("expected a valid YYYY-MM-DD date (years 0001–9999)".to_owned());
    }
    Ok(date)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItem {
    pub id: i64,
    pub title: String,
    pub description: Option<String>,
    pub status: Status,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub purge_after: Option<DateTime<Utc>>,
    #[serde(flatten, default)]
    pub schedule: Schedule,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: i64,
    pub work_item_id: i64,
    pub kind: String,
    pub actor: String,
    pub note: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub changes: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_display_honours_column_width() {
        assert_eq!(format!("{:<10}|", Status::Done), "done      |");
        assert_eq!(format!("{}", Status::Cancelled), "cancelled");
    }
}
