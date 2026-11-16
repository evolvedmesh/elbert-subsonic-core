//! The `p=` password parameter (`enc:` and hex) and the constant-time comparison it feeds.
#![no_main]

use elbert_subsonic_core::auth::decode_password;
use elbert_subsonic_core::util::ct_eq;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|given: &str| {
    match (given.strip_prefix("enc:"), decode_password(given)) {
        // Not hex-encoded: taken as it is.
        (None, decoded) => assert_eq!(decoded.as_deref(), Some(given)),
        // Hex-encoded: what came out is exactly what the hex said.
        (Some(hex), Some(decoded)) => {
            let again: String = decoded.bytes().map(|b| format!("{b:02x}")).collect();
            assert!(again.eq_ignore_ascii_case(hex));
            assert!(ct_eq(&decoded, &decoded));
        }
        (Some(_), None) => {}
    }
});
