//! A form POST: the query, a NUL, then the body, which the server decodes lossily and adds to
//! the query's parameters.
#![no_main]

use elbert_subsonic_core::api::Params;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let (query, body) = match data.iter().position(|&b| b == 0) {
        Some(at) => (&data[..at], &data[at + 1..]),
        None => (&data[..0], data),
    };
    let mut pairs = Params::parse(&String::from_utf8_lossy(query));
    pairs.extend(Params::parse(&String::from_utf8_lossy(body)));
    let params = Params(pairs);
    let _ = (params.get("t"), params.all("songId"), params.int("rating"));
    let _ = params.truthy("public");
});
