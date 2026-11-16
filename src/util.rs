//! Small pure helpers the protocol needs: constant-time comparison, content types,
//! percent coding and `Range` parsing. No I/O.

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::path::Path;

/// Compares tokens without stopping at the first difference.
pub fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A content type for an audio file from its extension, for serving it as it
/// is (the player probes the bytes anyway).
pub fn audio_mime(path: &Path) -> &'static str {
    match path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .as_deref()
    {
        Some("m4a" | "mp4" | "m4b" | "alac" | "aac") => "audio/mp4",
        Some("flac") => "audio/flac",
        Some("mp3") => "audio/mpeg",
        Some("ogg" | "oga" | "opus") => "audio/ogg",
        Some("wav") => "audio/wav",
        Some("aif" | "aiff") => "audio/aiff",
        Some("wma") => "audio/x-ms-wma",
        _ => "application/octet-stream",
    }
}

/// The MIME type of an image file, from its extension.
pub fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .as_deref()
    {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript",
        Some("json") => "application/json",
        Some("wasm") => "application/wasm",
        Some("css") => "text/css",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("otf") => "font/otf",
        Some("ttf") => "font/ttf",
        Some("woff2") => "font/woff2",
        Some("frag") => "text/plain",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// `%XX` decoding for a path segment; invalid escapes pass through.
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `%XX`-encodes everything but what is safe in any part of a URL.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// An inclusive byte range from a `Range` header, against a body of `len`
/// bytes. None for a missing, multi-range or unsatisfiable header — the
/// caller then sends everything (or 416 when `unsatisfiable` says so).
pub fn parse_range(header: Option<&str>, len: u64) -> RangeAsk {
    let Some(h) = header else {
        return RangeAsk::Whole;
    };
    let Some(spec) = h.trim().strip_prefix("bytes=") else {
        return RangeAsk::Whole;
    };
    if spec.contains(',') || len == 0 {
        return RangeAsk::Whole;
    }
    let Some((a, b)) = spec.split_once('-') else {
        return RangeAsk::Whole;
    };
    let (a, b) = (a.trim(), b.trim());
    let range = if a.is_empty() {
        let Ok(n) = b.parse::<u64>() else {
            return RangeAsk::Whole;
        };
        if n == 0 {
            return RangeAsk::Unsatisfiable;
        }
        (len.saturating_sub(n), len - 1)
    } else {
        let Ok(start) = a.parse::<u64>() else {
            return RangeAsk::Whole;
        };
        let end = if b.is_empty() {
            len - 1
        } else {
            match b.parse::<u64>() {
                Ok(e) => e.min(len - 1),
                Err(_) => return RangeAsk::Whole,
            }
        };
        if start >= len || end < start {
            return RangeAsk::Unsatisfiable;
        }
        (start, end)
    };
    RangeAsk::Part(range.0, range.1)
}

#[derive(Debug, PartialEq, Eq)]
/// What a `Range` header asks for.
pub enum RangeAsk {
    /// No range: the whole file.
    Whole,
    /// The bytes from the first to the second, inclusive.
    Part(u64, u64),
    /// A range the file cannot satisfy (answer `416`).
    Unsatisfiable,
}

/// A map that forgets its oldest entries past a capacity.
pub struct Bounded<K, V> {
    map: HashMap<K, V>,
    order: VecDeque<K>,
    cap: usize,
}

impl<K: Eq + Hash + Clone, V> Bounded<K, V> {
    /// An empty map that keeps at most `cap` entries.
    pub fn new(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            cap,
        }
    }

    /// The value kept for `k`.
    pub fn get(&self, k: &K) -> Option<&V> {
        self.map.get(k)
    }

    /// Keeps `v` for `k`, forgetting the oldest entry when full.
    pub fn insert(&mut self, k: K, v: V) {
        if self.map.insert(k.clone(), v).is_none() {
            self.order.push_back(k);
            while self.order.len() > self.cap {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_range(None, 100), RangeAsk::Whole);
        assert_eq!(parse_range(Some("bytes=0-"), 100), RangeAsk::Part(0, 99));
        assert_eq!(
            parse_range(Some("bytes=10-19"), 100),
            RangeAsk::Part(10, 19)
        );
        assert_eq!(
            parse_range(Some("bytes=90-200"), 100),
            RangeAsk::Part(90, 99)
        );
        assert_eq!(parse_range(Some("bytes=-10"), 100), RangeAsk::Part(90, 99));
        assert_eq!(
            parse_range(Some("bytes=100-"), 100),
            RangeAsk::Unsatisfiable
        );
        assert_eq!(parse_range(Some("bytes=0-1,5-6"), 100), RangeAsk::Whole);
        assert_eq!(parse_range(Some("items=0-1"), 100), RangeAsk::Whole);
    }

    #[test]
    fn decoding() {
        assert_eq!(percent_decode("a%2Fb%3A1"), "a/b:1");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("x%zz"), "x%zz");
    }
}
