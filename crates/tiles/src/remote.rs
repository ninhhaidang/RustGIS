//! Fetches parts of files read in place by their web address (FlatGeobuf and GeoParquet, see
//! [`rustgis_data::source::remote`]) through the tile cache's fetchers: threads natively, the
//! browser's fetch on the web. Every address is checked first.

use rustgis_data::source::remote::{self, Failed, Want};

use crate::cache::{DOWNLOAD, DROPPED, NO_RANGES, RANGE, TAIL};

/// Installs the fetcher (once; later calls do nothing).
pub fn install() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        remote::set_fetcher(|url, want, done| {
            if let Err(e) = crate::address::check_reach(url, crate::cache::ceiling_for(url)) {
                done(Err(Failed::Other(e)));
                return;
            }
            // A data download's size limit, not a tile's: a column chunk can be large.
            let marked = match want {
                Want::Range(a, b) if b > a => format!("{DOWNLOAD}{url}{RANGE}{a}-{}", b - 1),
                Want::Range(..) => {
                    done(Ok(Vec::new()));
                    return;
                }
                Want::Tail(n) => format!("{DOWNLOAD}{url}{TAIL}{n}"),
                // Up to the download limit, like a file added by its address.
                Want::Whole => format!("{DOWNLOAD}{url}"),
                // A query's answer, up to its own limit.
                Want::Upto(n) => crate::cache::download_address(url, usize::try_from(n).unwrap_or(usize::MAX)),
            };
            crate::cache::fetch_counted(marked, Box::new(move |r| done(r.map_err(|e| failed(&e)))));
        });
    });
}

/// Why a fetch failed, from the fetcher's error: its exact message, never a word found inside
/// another (an address can hold "416" or "dropped").
fn failed(e: &str) -> Failed {
    if e == DROPPED {
        Failed::Dropped
    } else if crate::cache::http_status(e) == Some(416) {
        Failed::PastEnd
    } else if e.ends_with(&format!(": {NO_RANGES}")) {
        Failed::NoRanges
    } else if is_larger_than(e) {
        Failed::TooLarge(e.to_string())
    } else {
        Failed::Other(e.to_string())
    }
}

/// Whether a fetch failed because the download passed its limit: the cache's exact message
/// (`larger than N bytes`), alone or after its address.
fn is_larger_than(e: &str) -> bool {
    let tail = e.rsplit_once(": ").map_or(e, |(_, t)| t);
    tail.strip_prefix("larger than ")
        .and_then(|t| t.strip_suffix(" bytes"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_errors_are_told_apart_exactly() {
        assert_eq!(failed(DROPPED), Failed::Dropped);
        assert_eq!(failed("HTTP 416"), Failed::PastEnd);
        assert_eq!(failed("http status: 416"), Failed::PastEnd);
        assert_eq!(failed(&format!("https://x.org/a.parquet: {NO_RANGES}")), Failed::NoRanges);
        // Its message is kept, for a file's error.
        for e in ["larger than 8388608 bytes", "https://x.org/query?f=pbf: larger than 3 bytes"] {
            assert_eq!(failed(e), Failed::TooLarge(e.into()), "{e}");
        }
        for e in ["larger than many bytes", "https://x.org/larger than 3 bytes/a.fgb: timed out", "larger than  bytes"] {
            assert_eq!(failed(e), Failed::Other(e.into()), "{e}");
        }
        // Words inside an address or another message are not the error.
        for e in ["https://x.org/data416.fgb: timed out", "https://x.org/dropped/a.fgb: timed out", "HTTP 404"] {
            assert_eq!(failed(e), Failed::Other(e.into()), "{e}");
        }
    }
}
