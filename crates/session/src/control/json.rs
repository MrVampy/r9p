pub fn push_string(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => {
                out.push_str("\\u");
                out.push_str(&format!("{:04x}", ch as u32));
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
}

#[cfg(test)]
fn string(value: &str) -> String {
    let mut out = String::new();
    push_string(&mut out, value);
    out
}

pub fn bytes_lossy(value: &[u8]) -> String {
    String::from_utf8_lossy(value).to_string()
}

pub fn error_response(code: &str, message: &str) -> String {
    let mut out = String::from("{\"ok\":false,\"error\":{\"code\":");
    push_string(&mut out, code);
    out.push_str(",\"message\":");
    push_string(&mut out, message);
    out.push_str("}}");
    out
}

#[cfg(test)]
mod tests {
    use super::string;

    #[test]
    fn escapes_json_string_control_characters() {
        assert_eq!(string("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
    }
}
