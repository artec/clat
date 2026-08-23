//! 控制面时间戳工具（MP-1）：workspace.json 的时间字段与 DSH 同为
//! ISO-8601 字符串（`2026-08-13T16:28:50.246Z`），不引入 chrono——
//! 零新增依赖（INV-MP8），历法换算用 Howard Hinnant 的
//! civil_from_days / days_from_civil 算法。

use std::time::{SystemTime, UNIX_EPOCH};

/// 当前时刻的 ISO-8601 UTC 字符串（毫秒精度）。
pub(crate) fn now_iso8601() -> String {
    let (secs, millis) = unix_now();
    format_iso8601(secs, millis)
}

/// 抢救改名/保尸改名用的本地日期戳（`20260823` 形态）。
pub(crate) fn date_stamp() -> String {
    let (secs, _) = unix_now();
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    format!("{year:04}{month:02}{day:02}")
}

/// ISO-8601 → Unix 秒（解析失败返回 `None`；只接受我们与 DSH 写出的
/// `…Z` 形态）。`ModelProfileSummary::updated_at`（i64，公开 DTO 不动）
/// 的换算用。
pub(crate) fn iso8601_to_unix_seconds(iso: &str) -> Option<i64> {
    let iso = iso.strip_suffix('Z')?;
    let (date, time) = iso.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() {
        return None;
    }
    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    // 秒段可带小数（我们写毫秒；解析截断到整秒）。
    let second: i64 = time_parts.next()?.split('.').next()?.parse().ok()?;
    if time_parts.next().is_some() {
        return None;
    }
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if !(0..=23).contains(&hour) || !(0..=59).contains(&minute) || !(0..=60).contains(&second) {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

fn unix_now() -> (i64, u32) {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => (duration.as_secs() as i64, duration.subsec_millis()),
        Err(_) => (0, 0),
    }
}

fn format_iso8601(unix_seconds: i64, millis: u32) -> String {
    let (year, month, day) = civil_from_days(unix_seconds.div_euclid(86_400));
    let seconds_of_day = unix_seconds.rem_euclid(86_400);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// 天数 → 公历（Hinnant civil_from_days；返回年/月/日）。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = (shifted - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    (if month <= 2 { year + 1 } else { year }, month as u32, day)
}

/// 公历 → 天数（Hinnant days_from_civil，上面的逆）。
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let adjusted_year = if month <= 2 { year - 1 } else { year };
    let era = if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    } / 400;
    let year_of_era = (adjusted_year - era * 400) as u64;
    let month_prime = if month > 2 { month - 3 } else { month + 9 } as u64;
    let day_of_year = (153 * month_prime + 2) / 5 + day as u64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era as i64 - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_parses_the_dsh_shape() {
        // DSH 实机样本：2026-08-13T16:28:50.246Z
        assert_eq!(
            format_iso8601(1_786_638_530, 246),
            "2026-08-13T16:28:50.246Z"
        );
        assert_eq!(
            iso8601_to_unix_seconds("2026-08-13T16:28:50.246Z"),
            Some(1_786_638_530)
        );
        // 负时间（1970 前）与闰年边界走一遍往返。
        assert_eq!(format_iso8601(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso8601_to_unix_seconds("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(format_iso8601(951_782_400, 0), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn rejects_malformed_timestamps() {
        assert_eq!(iso8601_to_unix_seconds("not a timestamp"), None);
        assert_eq!(iso8601_to_unix_seconds("2026-08-13"), None);
        assert_eq!(iso8601_to_unix_seconds("2026-13-13T00:00:00Z"), None);
        assert_eq!(iso8601_to_unix_seconds("2026-08-13T25:00:00Z"), None);
        // 无 Z 后缀（本地时间形态）不接受。
        assert_eq!(iso8601_to_unix_seconds("2026-08-13T16:28:50"), None);
    }

    #[test]
    fn date_stamp_is_compact_utc() {
        assert_eq!(date_stamp().len(), 8);
    }
}
