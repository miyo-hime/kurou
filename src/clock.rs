use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

pub(crate) fn house_time(raw: &str) -> String {
    let normalized = if raw.len() == 19 && raw.as_bytes().get(10) == Some(&b' ') {
        Some(format!("{}T{}Z", &raw[..10], &raw[11..]))
    } else {
        None
    };
    let source = normalized.as_deref().unwrap_or(raw);
    let Ok(timestamp) = OffsetDateTime::parse(source, &Rfc3339) else {
        return raw.to_owned();
    };
    let offset = UtcOffset::from_hms(7, 0, 0).expect("UTC+7 is a valid offset");
    timestamp.to_offset(offset).format(&Rfc3339).unwrap_or_else(|_| raw.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_wire_and_sqlite_times_in_house_time() {
        assert_eq!(house_time("2026-09-07T04:15:18.932Z"), "2026-09-07T11:15:18.932+07:00");
        assert_eq!(house_time("2026-09-07 04:15:18"), "2026-09-07T11:15:18+07:00");
    }

    #[test]
    fn leaves_unknown_timestamp_shapes_alone() {
        assert_eq!(house_time("sometime after tea"), "sometime after tea");
    }
}
