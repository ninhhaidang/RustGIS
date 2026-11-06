//! Spreadsheets: `.xlsx`, `.xlsb`, `.xls` and `.ods` workbooks, read by calamine behind RustGIS's
//! own checks (zip expansion, sheet size, an `.xls` file's sector tables). In the browser a panic
//! can't be caught, so those checks are all that stands between a damaged file and a crash: under
//! libFuzzer a panic aborts before `catch_unwind` can catch it, so any it reports got past them.
#![no_main]

use libfuzzer_sys::fuzz_target;
use rustgis_data::io::CsvOptions;
use rustgis_data::sheets;

fuzz_target!(|data: &[u8]| {
    let _ = sheets::sheet_names(data);
    let _ = sheets::columns(data, None);
    let _ = sheets::read(data, "fuzz", None, &CsvOptions::default());
});
