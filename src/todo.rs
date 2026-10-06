use anyhow::{Context, Result, bail};
use chrono::{DateTime, Days, FixedOffset, NaiveDate, Utc};
use serde::Serialize;

use crate::domain::{Status, WorkItem, parse_date};

pub const TIMEZONE: &str = "Asia/Seoul";

/// Contemporary Seoul civil time is UTC+09:00, independent of the host's TZ.
pub fn seoul_offset() -> FixedOffset {
    FixedOffset::east_opt(9 * 3600).expect("valid Seoul offset")
}

#[derive(Debug, Serialize)]
pub struct TodoWindow {
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub days: u32,
    pub timezone: &'static str,
}

impl TodoWindow {
    pub fn new(now: DateTime<Utc>, days: u32) -> Result<Self> {
        if days == 0 {
            bail!("days must be positive");
        }
        let start = now.with_timezone(&seoul_offset()).date_naive();
        let end = start
            .checked_add_days(Days::new(u64::from(days - 1)))
            .context("planning horizon exceeds the supported date range")?;
        parse_date(&end.to_string()).map_err(anyhow::Error::msg)?;
        Ok(Self {
            start,
            end,
            days,
            timezone: TIMEZONE,
        })
    }
}

#[derive(Debug, Serialize)]
pub struct TodoItem {
    #[serde(flatten)]
    pub item: WorkItem,
    pub overdue: bool,
    pub carried_over: bool,
}

#[derive(Debug, Serialize)]
pub struct TodoView {
    pub window: TodoWindow,
    pub actions: Vec<TodoItem>,
    pub blocked_waiting: Vec<TodoItem>,
    pub finished: Vec<TodoItem>,
}

impl TodoView {
    pub fn new(window: TodoWindow, items: impl IntoIterator<Item = WorkItem>, all: bool) -> Self {
        let mut selected: Vec<_> = items
            .into_iter()
            .filter(|item| {
                let actionable = item.status.is_actionable();
                (actionable || (all && matches!(item.status, Status::Done | Status::Cancelled)))
                    && [item.schedule.planned_date, item.schedule.due_date]
                        .into_iter()
                        .flatten()
                        .any(|date| date <= window.end && (actionable || date >= window.start))
            })
            .collect();
        selected.sort_by_key(|item| {
            let s = item.schedule;
            (
                s.priority,
                s.due_date.is_none(),
                s.due_date,
                s.planned_date.is_none(),
                s.planned_date,
                item.id,
            )
        });
        let mut view = Self {
            window,
            actions: Vec::new(),
            blocked_waiting: Vec::new(),
            finished: Vec::new(),
        };
        for item in selected {
            let blocked = matches!(item.status, Status::Blocked | Status::Waiting);
            let actionable = item.status.is_actionable();
            let row = TodoItem {
                overdue: actionable
                    && item
                        .schedule
                        .due_date
                        .is_some_and(|date| date < view.window.start),
                carried_over: actionable
                    && item
                        .schedule
                        .planned_date
                        .is_some_and(|date| date < view.window.start),
                item,
            };
            if !actionable {
                view.finished.push(row);
            } else if blocked {
                view.blocked_waiting.push(row);
            } else {
                view.actions.push(row);
            }
        }
        view
    }
}
