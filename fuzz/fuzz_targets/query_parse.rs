//! The query string of `/rest/<call>`: split and decode, then read the way the dispatcher
//! and the `f=` / `callback=` rule read it.
#![no_main]

use elbert_subsonic_core::api::Params;
use elbert_subsonic_core::respond::Format;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|query: &str| {
    let params = Params(Params::parse(query));
    let _ = (params.get("id"), params.all("id"), params.need("u"));
    let _ = (params.int("size"), params.truthy("submission"));
    let _ = (params.count("count", 50, 500), params.offset("offset"));
    // A JSONP callback goes into a script unescaped: it must only ever be a plain name.
    if let Format::Jsonp(callback) = Format::from_params(params.get("f"), params.get("callback")) {
        assert!(!callback.is_empty());
        assert!(callback
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '$')));
    }
});
