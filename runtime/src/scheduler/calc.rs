use chrono::{DateTime, Datelike, Duration, Local, TimeZone, Utc, Weekday};
use super::store::ScheduleSpec;

const ONCE_GRACE_HOURS: i64 = 24;

pub fn validate_schedule(spec: &ScheduleSpec) -> Result<(), String> {
    match spec {
        ScheduleSpec::Once { at } => {
            if *at <= Utc::now() {
                return Err("执行时间不能早于当前时间".into());
            }
        }
        ScheduleSpec::Daily { hour, minute } => validate_hm(*hour, *minute)?,
        ScheduleSpec::Weekly {
            weekday,
            hour,
            minute,
        } => {
            if *weekday > 6 {
                return Err("星期应为 0（周一）到 6（周日）".into());
            }
            validate_hm(*hour, *minute)?;
        }
        ScheduleSpec::Monthly { day, hour, minute } => {
            if *day < 1 || *day > 31 {
                return Err("每月日期应为 1–31".into());
            }
            validate_hm(*hour, *minute)?;
        }
        ScheduleSpec::Cron { expr } => {
            if expr.trim().is_empty() {
                return Err("Cron 表达式不能为空".into());
            }
            return Err("Cron 表达式调度尚未启用，请使用每天、每周或每月".into());
        }
    }
    Ok(())
}

fn validate_hm(hour: u8, minute: u8) -> Result<(), String> {
    if hour > 23 {
        return Err("小时应为 0–23".into());
    }
    if minute > 59 {
        return Err("分钟应为 0–59".into());
    }
    Ok(())
}

fn weekday_to_chrono(weekday: u8) -> Weekday {
    match weekday {
        0 => Weekday::Mon,
        1 => Weekday::Tue,
        2 => Weekday::Wed,
        3 => Weekday::Thu,
        4 => Weekday::Fri,
        5 => Weekday::Sat,
        _ => Weekday::Sun,
    }
}

fn local_naive_at(hour: u8, minute: u8, year: i32, month: u32, day: u32) -> DateTime<Utc> {
    let local = Local
        .with_ymd_and_hms(year, month, day, hour as u32, minute as u32, 0)
        .single()
        .expect("valid local datetime");
    local.with_timezone(&Utc)
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let (ny, nm) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let first_next = chrono::NaiveDate::from_ymd_opt(ny, nm, 1).expect("valid date");
    first_next
        .pred_opt()
        .expect("previous day")
        .day()
}

fn clamp_day(year: i32, month: u32, day: u8) -> u32 {
    let dim = days_in_month(year, month);
    (day as u32).min(dim)
}

pub fn compute_next_run_at(spec: &ScheduleSpec, from: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    match spec {
        ScheduleSpec::Once { at } => {
            if *at > from {
                Ok(*at)
            } else if from - *at <= Duration::hours(ONCE_GRACE_HOURS) {
                Ok(*at)
            } else {
                Err("一次性任务时间已过期超过 24 小时".into())
            }
        }
        ScheduleSpec::Daily { hour, minute } => {
            validate_hm(*hour, *minute)?;
            Ok(next_daily(*hour, *minute, from))
        }
        ScheduleSpec::Weekly {
            weekday,
            hour,
            minute,
        } => {
            validate_hm(*hour, *minute)?;
            Ok(next_weekly(*weekday, *hour, *minute, from))
        }
        ScheduleSpec::Monthly { day, hour, minute } => {
            if *day < 1 || *day > 31 {
                return Err("每月日期应为 1–31".into());
            }
            validate_hm(*hour, *minute)?;
            Ok(next_monthly(*day, *hour, *minute, from))
        }
        ScheduleSpec::Cron { .. } => Err("Cron 表达式调度尚未启用".into()),
    }
}

pub fn compute_next_run_at_after(
    spec: &ScheduleSpec,
    after: DateTime<Utc>,
) -> Result<DateTime<Utc>, String> {
    let probe = after + Duration::seconds(1);
    compute_next_run_at(spec, probe)
}

fn next_daily(hour: u8, minute: u8, from: DateTime<Utc>) -> DateTime<Utc> {
    let local = from.with_timezone(&Local);
    let mut candidate =
        local_naive_at(hour, minute, local.year(), local.month(), local.day());
    if candidate <= from {
        let next_day = local.date_naive() + Duration::days(1);
        candidate = local_naive_at(
            hour,
            minute,
            next_day.year(),
            next_day.month(),
            next_day.day(),
        );
    }
    candidate
}

fn next_weekly(weekday: u8, hour: u8, minute: u8, from: DateTime<Utc>) -> DateTime<Utc> {
    let target = weekday_to_chrono(weekday);
    let local = from.with_timezone(&Local);
    let mut date = local.date_naive();
    for _ in 0..14 {
        let candidate = local_naive_at(hour, minute, date.year(), date.month(), date.day());
        let wd = date.weekday();
        if wd == target && candidate > from {
            return candidate;
        }
        date += Duration::days(1);
    }
    next_daily(hour, minute, from)
}

fn next_monthly(day: u8, hour: u8, minute: u8, from: DateTime<Utc>) -> DateTime<Utc> {
    let local = from.with_timezone(&Local);
    let mut year = local.year();
    let mut month = local.month();
    for _ in 0..14 {
        let d = clamp_day(year, month, day);
        let candidate = local_naive_at(hour, minute, year, month, d);
        if candidate > from {
            return candidate;
        }
        if month == 12 {
            year += 1;
            month = 1;
        } else {
            month += 1;
        }
    }
    next_daily(hour, minute, from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Timelike};

    #[test]
    fn daily_next_today_if_future() {
        let from = Local.with_ymd_and_hms(2026, 3, 15, 8, 0, 0).unwrap().with_timezone(&Utc);
        let next = compute_next_run_at(
            &ScheduleSpec::Daily {
                hour: 9,
                minute: 30,
            },
            from,
        )
        .unwrap();
        let local = next.with_timezone(&Local);
        assert_eq!(local.hour(), 9);
        assert_eq!(local.minute(), 30);
        assert_eq!(local.day(), 15);
    }

    #[test]
    fn daily_next_tomorrow_if_past() {
        let from = Local.with_ymd_and_hms(2026, 3, 15, 10, 0, 0).unwrap().with_timezone(&Utc);
        let next = compute_next_run_at(
            &ScheduleSpec::Daily {
                hour: 9,
                minute: 30,
            },
            from,
        )
        .unwrap();
        let local = next.with_timezone(&Local);
        assert_eq!(local.day(), 16);
    }

    #[test]
    fn weekly_finds_next_monday() {
        // 2026-03-15 is Sunday (local)
        let from = Local.with_ymd_and_hms(2026, 3, 15, 12, 0, 0).unwrap().with_timezone(&Utc);
        let next = compute_next_run_at(
            &ScheduleSpec::Weekly {
                weekday: 0,
                hour: 8,
                minute: 0,
            },
            from,
        )
        .unwrap();
        let local = next.with_timezone(&Local);
        assert_eq!(local.weekday(), Weekday::Mon);
    }

    #[test]
    fn validate_once_rejects_past() {
        let at = Utc.with_ymd_and_hms(2020, 1, 1, 8, 0, 0).unwrap();
        let err = validate_schedule(&ScheduleSpec::Once { at }).unwrap_err();
        assert!(err.contains("不能早于当前时间"));
    }

    #[test]
    fn once_within_grace() {
        let at = Utc.with_ymd_and_hms(2026, 3, 15, 8, 0, 0).unwrap();
        let from = at + Duration::hours(2);
        let next = compute_next_run_at(&ScheduleSpec::Once { at }, from).unwrap();
        assert_eq!(next, at);
    }

    #[test]
    fn monthly_same_month_if_future() {
        let from = Local.with_ymd_and_hms(2026, 3, 10, 8, 0, 0).unwrap().with_timezone(&Utc);
        let next = compute_next_run_at(
            &ScheduleSpec::Monthly {
                day: 15,
                hour: 9,
                minute: 0,
            },
            from,
        )
        .unwrap();
        let local = next.with_timezone(&Local);
        assert_eq!(local.month(), 3);
        assert_eq!(local.day(), 15);
        assert_eq!(local.hour(), 9);
    }

    #[test]
    fn monthly_next_month_if_past() {
        let from = Local.with_ymd_and_hms(2026, 3, 20, 10, 0, 0).unwrap().with_timezone(&Utc);
        let next = compute_next_run_at(
            &ScheduleSpec::Monthly {
                day: 15,
                hour: 9,
                minute: 0,
            },
            from,
        )
        .unwrap();
        let local = next.with_timezone(&Local);
        assert_eq!(local.month(), 4);
        assert_eq!(local.day(), 15);
    }

    #[test]
    fn monthly_clamps_to_last_day() {
        // Feb 2026 has 28 days; day=31 → Feb 28
        let from = Local.with_ymd_and_hms(2026, 2, 1, 8, 0, 0).unwrap().with_timezone(&Utc);
        let next = compute_next_run_at(
            &ScheduleSpec::Monthly {
                day: 31,
                hour: 10,
                minute: 0,
            },
            from,
        )
        .unwrap();
        let local = next.with_timezone(&Local);
        assert_eq!(local.month(), 2);
        assert_eq!(local.day(), 28);
    }
}
