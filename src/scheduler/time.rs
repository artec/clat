//! Pure civil-time calculation. Fold selects the earlier instant; gaps skip.
use super::*;
use jiff::{Timestamp, ToSpan, tz::TimeZone};

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TimeRule {
    At {
        at_ms: i64,
    },
    Interval {
        anchor_ms: i64,
        every_ms: u64,
    },
    Daily {
        hour: u8,
        minute: u8,
        timezone: String,
    },
    Weekly {
        weekday: u8,
        hour: u8,
        minute: u8,
        timezone: String,
    },
}
impl TimeRule {
    pub(crate) fn validate(&self) -> Result<(), ScheduleError> {
        match self {
            Self::At { at_ms } => {
                instant(*at_ms)?;
            }
            Self::Interval {
                anchor_ms,
                every_ms,
            } => {
                instant(*anchor_ms)?;
                if !(1000..=366 * 86400000).contains(every_ms) {
                    return Err(invalid());
                }
            }
            Self::Daily {
                hour,
                minute,
                timezone,
            } => {
                zone(*hour, *minute, timezone)?;
            }
            Self::Weekly {
                weekday,
                hour,
                minute,
                timezone,
            } => {
                if !(1..=7).contains(weekday) {
                    return Err(invalid());
                }
                zone(*hour, *minute, timezone)?;
            }
        }
        Ok(())
    }
    pub(crate) fn first(&self, now: i64) -> Result<Option<i64>, ScheduleError> {
        self.validate()?;
        match self {
            Self::At { at_ms } => Ok(Some(*at_ms)),
            Self::Interval { anchor_ms, .. } if *anchor_ms >= now => Ok(Some(*anchor_ms)),
            _ => self.after(now.saturating_sub(1)),
        }
    }
    pub(crate) fn latest(&self, now: i64) -> Result<Option<i64>, ScheduleError> {
        self.validate()?;
        match self {
            Self::At { at_ms } => Ok((*at_ms <= now).then_some(*at_ms)),
            Self::Interval {
                anchor_ms,
                every_ms,
            } => {
                if now < *anchor_ms {
                    return Ok(None);
                }
                let steps = (i128::from(now) - i128::from(*anchor_ms)) / i128::from(*every_ms);
                checked_ms(i128::from(*anchor_ms) + steps * i128::from(*every_ms)).map(Some)
            }
            _ => self.civil_candidate(now, false),
        }
    }
    pub(crate) fn after(&self, now: i64) -> Result<Option<i64>, ScheduleError> {
        self.validate()?;
        match self {
            Self::At { at_ms } => Ok((*at_ms > now).then_some(*at_ms)),
            Self::Interval {
                anchor_ms,
                every_ms,
            } => {
                let steps = if now < *anchor_ms {
                    0
                } else {
                    (i128::from(now) - i128::from(*anchor_ms)) / i128::from(*every_ms) + 1
                };
                checked_ms(i128::from(*anchor_ms) + steps * i128::from(*every_ms)).map(Some)
            }
            _ => self.civil_candidate(now, true),
        }
    }
    fn civil_candidate(&self, now: i64, forward: bool) -> Result<Option<i64>, ScheduleError> {
        let (weekday, hour, minute, name) = match self {
            Self::Daily {
                hour,
                minute,
                timezone,
            } => (None, *hour, *minute, timezone),
            Self::Weekly {
                weekday,
                hour,
                minute,
                timezone,
            } => (Some(*weekday), *hour, *minute, timezone),
            _ => return Err(invalid()),
        };
        let tz = zone(hour, minute, name)?;
        let start = instant(now)?.to_zoned(tz.clone()).date();
        // Weekly + one skipped civil date remains within this bounded window.
        for distance in 0i64..=15 {
            let days = if forward { distance } else { -distance };
            let date = start.checked_add(days.days()).map_err(|_| invalid())?;
            if weekday.is_some_and(|d| date.weekday().to_monday_one_offset() != d as i8) {
                continue;
            }
            let civil = date.at(hour as i8, minute as i8, 0, 0);
            let timestamp = tz
                .to_ambiguous_timestamp(civil)
                .earlier()
                .map_err(|_| invalid())?;
            if timestamp.to_zoned(tz.clone()).datetime() != civil {
                continue;
            }
            let ms = timestamp.as_millisecond();
            if forward && ms > now || !forward && ms <= now {
                return Ok(Some(ms));
            }
        }
        Err(invalid())
    }
}
fn invalid() -> ScheduleError {
    ScheduleError::new("invalid_time")
}
fn instant(ms: i64) -> Result<Timestamp, ScheduleError> {
    Timestamp::from_millisecond(ms).map_err(|_| invalid())
}
fn checked_ms(ms: i128) -> Result<i64, ScheduleError> {
    let ms = i64::try_from(ms).map_err(|_| invalid())?;
    instant(ms)?;
    Ok(ms)
}
fn zone(hour: u8, minute: u8, name: &str) -> Result<TimeZone, ScheduleError> {
    if hour > 23 || minute > 59 || name.len() > 128 {
        return Err(invalid());
    }
    let zone = TimeZone::get(name).map_err(|_| invalid())?;
    if zone.iana_name().is_none() && name != "UTC" {
        return Err(invalid());
    }
    Ok(zone)
}
