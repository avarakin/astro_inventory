use std::net::IpAddr;
use std::path::PathBuf;

/// Detect local longitude using GeoLite2 + public IP.
/// Cached in .location file so we only hit the network once.
pub fn detect_longitude() -> f64 {
    let base_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let location_cache = base_dir.join(".location");
    let geoip_db = base_dir.join("GeoLite2-City.mmdb");

    // Try cache first
    if let Ok(val) = std::fs::read_to_string(&location_cache) {
        let val = val.trim();
        if let Ok(lon) = val.parse::<f64>() {
            return lon;
        }
    }

    // Try GeoLite2
    let result = (|| -> anyhow::Result<f64> {
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
        let lon = city
            .location
            .longitude
            .ok_or_else(|| anyhow::anyhow!("No longitude in GeoIP result"))?;

        std::fs::write(&location_cache, lon.to_string())
            .map_err(|e| anyhow::anyhow!("Cache write failed: {e}"))?;

        Ok(lon)
    })();

    match result {
        Ok(lon) => lon,
        Err(e) => {
            eprintln!("WARNING: GeoIP lookup failed: {e}");
            0.0
        }
    }
}
