use chrono::{DateTime, Duration, Local, Timelike, TimeZone};

/// Compute GMST in hours for a given datetime.
fn gmst_hours(dt: &DateTime<Local>) -> f64 {
    let jd = dt.timestamp() as f64 / 86400.0 + 2440587.5;
    let d = jd - 2451545.0;
    let gmst = (18.697374558 + 24.06570982441908 * d) % 24.0;
    if gmst < 0.0 {
        gmst + 24.0
    } else {
        gmst
    }
}

/// Compute LST in hours for a given datetime and longitude.
fn lst_hours(dt: &DateTime<Local>, lon_deg: f64) -> f64 {
    let gmst = gmst_hours(dt);
    let lst = (gmst + lon_deg / 15.0) % 24.0;
    if lst < 0.0 {
        lst + 24.0
    } else {
        lst
    }
}

/// Circular difference in hours.
fn circ_diff(a: f64, b: f64) -> f64 {
    let d = (a - b).abs() % 24.0;
    d.min(24.0 - d)
}

/// Find the upcoming date when an object transits at local civil midnight.
/// Returns the local midnight datetime where LST is closest to RA_object.
pub fn transit_date(ra_hours: f64, lon_deg: f64) -> Option<DateTime<Local>> {
    #[allow(clippy::disallowed_names)]
    let _t0 = std::time::Instant::now();
    let result = transit_date_inner(ra_hours, lon_deg);
    eprintln!("[profile] transit_date: {:?}", _t0.elapsed());
    result
}

fn transit_date_inner(ra_hours: f64, lon_deg: f64) -> Option<DateTime<Local>> {
    let now = Local::now();
    let midnight_naive = now
        .naive_local()
        .with_hour(0)
        .and_then(|d| d.with_minute(0))
        .and_then(|d| d.with_second(0))
        .unwrap();
    let midnight = Local.from_local_datetime(&midnight_naive).single()?;

    // Coarse scan: 366 days
    let mut best: Option<DateTime<Local>> = None;
    let mut best_diff = 999.0f64;
    for day in 0..366 {
        let candidate = midnight + Duration::days(day as i64);
        let diff = circ_diff(lst_hours(&candidate, lon_deg), ra_hours);
        if diff < best_diff {
            best_diff = diff;
            best = Some(candidate);
        }
    }
    let best = best?;

    // Refine by bisection on [best - 1.5 days, best + 1.5 days]
    let mut lo = best - Duration::milliseconds((1.5 * 86400.0 * 1000.0) as i64);
    let mut hi = best + Duration::milliseconds((1.5 * 86400.0 * 1000.0) as i64);

    let f = |t: &DateTime<Local>| circ_diff(lst_hours(t, lon_deg), ra_hours);

    for _ in 0..50 {
        let mid = lo + (hi - lo) / 2;
        if f(&lo) <= f(&mid) {
            hi = mid;
        } else {
            lo = mid;
        }
        if (hi - lo) < Duration::milliseconds((0.01 * 86400.0 * 1000.0) as i64) {
            break;
        }
    }
    let refined = lo + (hi - lo) / 2;

    // Truncate to local midnight
    let result_naive = refined
        .naive_local()
        .with_hour(0)
        .and_then(|d| d.with_minute(0))
        .and_then(|d| d.with_second(0))
        .unwrap();
    let result = Local.from_local_datetime(&result_naive).single()?;
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LON: f64 = -74.0; // US NJ

    #[test]
    fn test_ra_21h42m_lon_minus74() {
        let got = transit_date(21.0 + 42.0 / 60.0, LON);
        assert!(got.is_some());
        let dt = got.unwrap();
        assert_eq!(dt.hour(), 0);
        assert_eq!(dt.minute(), 0);
    }

    #[test]
    fn test_ra_01h43m() {
        let got = transit_date(1.0 + 43.0 / 60.0, LON);
        assert!(got.is_some());
    }

    #[test]
    fn test_transit_date_is_midnight() {
        for ra in [0.0, 6.0, 12.0, 18.0, 23.0] {
            let got = transit_date(ra, LON);
            if let Some(dt) = got {
                assert_eq!(dt.hour(), 0, "RA={ra}h: expected midnight");
                assert_eq!(dt.minute(), 0, "RA={ra}h: expected midnight");
            }
        }
    }

    #[test]
    fn test_returns_datetime_or_none() {
        let got = transit_date(23.34, LON);
        assert!(got.is_some());
    }
}
