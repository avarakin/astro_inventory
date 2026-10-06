use std::net::IpAddr;
use std::path::PathBuf;

/// Detect observer (latitude, longitude) using GeoLite2 + public IP.
/// Cached in `.location` (format: "lat lon") so we only hit the network once.
///
/// NOTE: uses `reqwest::blocking` — call it *before* starting the tokio
/// runtime, or it panics ("Cannot drop a runtime in an async context").
pub fn detect_location() -> (f64, f64) {
    let base_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let location_cache = base_dir.join(".location");
    let geoip_db = base_dir.join("GeoLite2-City.mmdb");

    // Try cache first
    if let Ok(val) = std::fs::read_to_string(&location_cache) {
        let mut it = val.split_whitespace();
        let lat = it.next().and_then(|s| s.parse::<f64>().ok());
        let lon = it.next().and_then(|s| s.parse::<f64>().ok());
        if let (Some(lat), Some(lon)) = (lat, lon) {
            return (lat, lon);
        }
    }

    // Try GeoLite2
    let result = (|| -> anyhow::Result<(f64, f64)> {
        let ip_str = reqwest::blocking::get("https://api.ipify.org")
            .map_err(|e| anyhow::anyhow!("IP lookup failed: {e}"))?
            .text()
            .map_err(|e| anyhow::anyhow!("IP read failed: {e}"))?
            .trim()
            .to_string();

        let ip: IpAddr = ip_str
            .parse()
            .map_err(|e| anyhow::anyhow!("IP parse failed: {e}"))?;

        let reader = maxminddb::Reader::open_readfile(&geoip_db)
            .map_err(|e| anyhow::anyhow!("GeoIP DB open failed: {e}"))?;
        let lookup = reader
            .lookup(ip)
            .map_err(|e| anyhow::anyhow!("GeoIP lookup failed: {e}"))?;
        let city: Option<maxminddb::geoip2::City> = lookup
            .decode()
            .map_err(|e| anyhow::anyhow!("GeoIP decode failed: {e}"))?;
        let city = city.ok_or_else(|| anyhow::anyhow!("No GeoIP result"))?;
        let lat = city
            .location
            .latitude
            .ok_or_else(|| anyhow::anyhow!("No latitude in GeoIP result"))?;
        let lon = city
            .location
            .longitude
            .ok_or_else(|| anyhow::anyhow!("No longitude in GeoIP result"))?;

        std::fs::write(&location_cache, format!("{lat} {lon}"))
            .map_err(|e| anyhow::anyhow!("Cache write failed: {e}"))?;

        Ok((lat, lon))
    })();

    match result {
        Ok(v) => v,
        Err(e) => {
            eprintln!("WARNING: GeoIP lookup failed: {e}");
            (0.0, 0.0)
        }
    }
}
