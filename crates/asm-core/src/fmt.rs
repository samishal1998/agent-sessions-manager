//! Formatting helpers shared by the frontends that render to text.

/// Bytes as something readable at a glance: at most three significant
/// digits, and no decimal point on a plain byte count.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_and_keeps_it_short() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(882), "882 B");
        assert_eq!(human_bytes(2_867), "2.8 KB");
        assert_eq!(human_bytes(28_738_453), "27 MB");
        assert_eq!(human_bytes(1024 * 1024 * 1024 * 3), "3.0 GB");
    }

    #[test]
    fn never_reports_1024_of_a_unit() {
        // 1024 KB is 1 MB; rolling over is what the loop is for.
        assert_eq!(human_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(human_bytes(1024 * 1024 - 1), "1024 KB");
    }
}
