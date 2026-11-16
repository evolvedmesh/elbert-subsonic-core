//! A `Range` header against a body length: the first 8 bytes are the length, the rest the header.
#![no_main]

use elbert_subsonic_core::util::{parse_range, RangeAsk};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((len, header)) = data.split_first_chunk::<8>() else {
        return;
    };
    let len = u64::from_le_bytes(*len);
    let Ok(header) = std::str::from_utf8(header) else {
        return;
    };
    // Whatever is asked for lies inside the body.
    if let RangeAsk::Part(start, end) = parse_range(Some(header), len) {
        assert!(start <= end && end < len);
    }
    assert_eq!(parse_range(None, len), RangeAsk::Whole);
});
