//! UTC timestamps for manifests (RFC 3339) and ZIP entries (DOS date/time),
//! without a date library.

use std::time::{SystemTime, UNIX_EPOCH};

/// A UTC civil date and time, to the second.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Utc {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl Utc {
    pub fn from_system_time(t: SystemTime) -> Self {
        let secs = match t.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_secs() as i64,
            Err(e) => -(e.duration().as_secs_f64().ceil() as i64),
        };
        Self::from_unix(secs)
    }

    /// Days-from-civil inverse (Howard Hinnant's algorithm).
    pub fn from_unix(secs: i64) -> Self {
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400) as u32;
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        let year = yoe + era * 400 + i64::from(month <= 2);
        Utc {
            year,
            month,
            day,
            hour: rem / 3600,
            minute: rem / 60 % 60,
            second: rem % 60,
        }
    }

    /// `2026-10-10T12:00:00Z`.
    pub fn rfc3339(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    /// `(date, time)` in MS-DOS format, clamped to 1980–2107.
    pub fn dos(&self) -> (u16, u16) {
        if self.year < 1980 {
            return ((1 << 5) | 1, 0);
        }
        let year = self.year.min(2107) as u32;
        let date = ((year - 1980) << 9) | (self.month << 5) | self.day;
        let time = (self.hour << 11) | (self.minute << 5) | (self.second / 2);
        (date as u16, time as u16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_dates() {
        assert_eq!(Utc::from_unix(0).rfc3339(), "1970-01-01T00:00:00Z");
        assert_eq!(Utc::from_unix(1_791_633_600).rfc3339(), "2026-10-10T12:00:00Z");
        assert_eq!(Utc::from_unix(951_782_400).rfc3339(), "2000-02-29T00:00:00Z");
    }
}
