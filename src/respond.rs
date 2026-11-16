//! A Subsonic answer in whichever format the client asked for.
//!
//! Answers are built once as JSON. XML — what the API defaults to, and what
//! older clients still ask for — follows mechanically: a scalar is an
//! attribute, an object or array a child element, and a `value` key the text.

use serde_json::{Map, Value};

/// The Subsonic API version the server speaks.
pub const API_VERSION: &str = "1.16.1";
/// The `16` in it: a client's minor version may be no newer.
pub const MINOR_VERSION: u32 = 16;

/// `error.code`s from the API.
pub const ERR_GENERIC: u32 = 0;
/// A required parameter is missing.
pub const ERR_MISSING: u32 = 10;
/// The client must upgrade to speak to this server.
pub const ERR_CLIENT_OLD: u32 = 20;
/// The server must upgrade to speak to this client.
pub const ERR_SERVER_OLD: u32 = 30;
/// Wrong username or password.
pub const ERR_CREDENTIALS: u32 = 40;
/// More than one authentication mechanism was given.
pub const ERR_AUTH_CONFLICT: u32 = 43;
/// The API key is not valid.
pub const ERR_BAD_API_KEY: u32 = 44;
/// The account is not authorized for the call.
pub const ERR_NOT_ALLOWED: u32 = 50;
/// The thing asked for was not found.
pub const ERR_NOT_FOUND: u32 = 70;

#[derive(Debug)]
/// An API error: its `error.code` and message.
pub struct ApiError(pub u32, pub String);

impl ApiError {
    /// The "required parameter `name` is missing" error.
    pub fn missing(name: &str) -> Self {
        ApiError(
            ERR_MISSING,
            format!("Required parameter is missing: {name}"),
        )
    }

    /// The "`what` not found" error.
    pub fn not_found(what: &str) -> Self {
        ApiError(ERR_NOT_FOUND, format!("{what} not found"))
    }

    /// The "not authorized to `what`" error.
    pub fn not_allowed(what: &str) -> Self {
        ApiError(ERR_NOT_ALLOWED, format!("{what} is not allowed here"))
    }
}

/// How a response is encoded.
pub enum Format {
    /// XML (the default).
    Xml,
    /// JSON, as `f=json`.
    Json,
    /// JSONP with the given callback name, as `f=jsonp`.
    Jsonp(String),
}

impl Format {
    /// The format `f=` and `callback=` ask for.
    pub fn from_params(f: Option<&str>, callback: Option<&str>) -> Format {
        match f {
            Some("json") => Format::Json,
            Some("jsonp") => {
                // It goes into a script unescaped, so only a name will do.
                let valid = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '$';
                match callback.filter(|c| !c.is_empty() && c.chars().all(valid)) {
                    Some(c) => Format::Jsonp(c.to_string()),
                    None => Format::Json,
                }
            }
            _ => Format::Xml,
        }
    }
}

/// An answer ready to send: the content type and the body. A host sends it with
/// `Cache-Control: no-store`.
pub struct Rendered {
    /// The `Content-Type` header to send.
    pub content_type: &'static str,
    /// The response body.
    pub body: String,
}

/// A successful response around `payload`.
pub fn ok(format: &Format, payload: Value) -> Rendered {
    respond(format, "ok", payload)
}

/// A failed response for `error`.
pub fn failed(format: &Format, error: &ApiError) -> Rendered {
    let payload = serde_json::json!({ "error": { "code": error.0, "message": error.1 } });
    respond(format, "failed", payload)
}

fn respond(format: &Format, status: &str, payload: Value) -> Rendered {
    let mut root = Map::new();
    root.insert("status".into(), status.into());
    root.insert("version".into(), API_VERSION.into());
    root.insert("type".into(), "elbert".into());
    root.insert("serverVersion".into(), env!("CARGO_PKG_VERSION").into());
    root.insert("openSubsonic".into(), true.into());
    if let Value::Object(fields) = payload {
        root.extend(fields);
    }
    let (body, mime) = match format {
        Format::Xml => (to_xml(&root), "text/xml; charset=utf-8"),
        Format::Json => (wrap_json(root).to_string(), "application/json"),
        Format::Jsonp(callback) => (
            format!("{callback}({});", wrap_json(root)),
            "text/javascript",
        ),
    };
    Rendered {
        content_type: mime,
        body,
    }
}

fn wrap_json(root: Map<String, Value>) -> Value {
    let mut wrapped = Map::new();
    wrapped.insert("subsonic-response".into(), Value::Object(root));
    Value::Object(wrapped)
}

fn to_xml(root: &Map<String, Value>) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    out.push_str(r#"<subsonic-response xmlns="http://subsonic.org/restapi""#);
    write_body(&mut out, "subsonic-response", root);
    out
}

fn write_element(out: &mut String, name: &str, value: &Value) {
    match value {
        Value::Null => {}
        Value::Array(items) => {
            for item in items {
                write_element(out, name, item);
            }
        }
        Value::Object(fields) => {
            out.push('<');
            out.push_str(name);
            write_body(out, name, fields);
        }
        scalar => {
            out.push('<');
            out.push_str(name);
            out.push('>');
            escape_into(out, &scalar_text(scalar));
            out.push_str("</");
            out.push_str(name);
            out.push('>');
        }
    }
}

/// Everything after an element's name: attributes, then content.
fn write_body(out: &mut String, name: &str, fields: &Map<String, Value>) {
    for (key, value) in fields {
        if key == "value" || value.is_null() || value.is_array() || value.is_object() {
            continue;
        }
        out.push(' ');
        out.push_str(key);
        out.push_str("=\"");
        escape_into(out, &scalar_text(value));
        out.push('"');
    }
    let text = fields
        .get("value")
        .filter(|v| !v.is_null() && !v.is_object() && !v.is_array());
    let children = fields.iter().filter(|(_, v)| v.is_array() || v.is_object());
    let has_children = fields
        .values()
        .any(|v| v.is_object() || matches!(v, Value::Array(a) if !a.is_empty()));
    if text.is_none() && !has_children {
        out.push_str("/>");
        return;
    }
    out.push('>');
    if let Some(text) = text {
        escape_into(out, &scalar_text(text));
    }
    for (key, value) in children {
        write_element(out, key, value);
    }
    out.push_str("</");
    out.push_str(name);
    out.push('>');
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn escape_into(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0 has no way to write the other control characters at all.
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
}

/// `2026-11-05T12:30:15.000Z` for milliseconds since the epoch.
pub fn iso8601(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dates() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso8601(1_709_210_096_789), "2024-02-29T12:34:56.789Z");
    }

    #[test]
    fn xml_shape() {
        let Value::Object(root) = json!({
            "status": "ok",
            "lyrics": { "artist": "A & B", "value": "line <1>" },
            "album": { "id": "x", "song": [{ "id": "1" }, { "id": "2" }], "isrc": ["CODE"] },
            "empty": { "item": [] },
        }) else {
            unreachable!()
        };
        let xml = to_xml(&root);
        assert!(
            xml.contains(r#"<lyrics artist="A &amp; B">line &lt;1&gt;</lyrics>"#),
            "{xml}"
        );
        assert!(xml.contains(r#"<song id="1"/><song id="2"/>"#), "{xml}");
        assert!(xml.contains("<isrc>CODE</isrc>"), "{xml}");
        assert!(xml.contains("<empty/>"), "{xml}");
        assert!(xml.ends_with("</subsonic-response>"), "{xml}");
    }

    #[test]
    fn a_callback_cannot_be_a_script() {
        assert!(matches!(
            Format::from_params(Some("jsonp"), Some("cb_1")),
            Format::Jsonp(_)
        ));
        assert!(matches!(
            Format::from_params(Some("jsonp"), Some("x;alert(1)")),
            Format::Json
        ));
    }
}
