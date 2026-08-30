//! Minimal multipart/form-data field extraction from a raw body.
//!
//! Go's artifact service parses multipart manually (Swift's publish body uses
//! `Content-Transfer-Encoding: binary`, which chokes some parsers); this
//! module replicates that byte-exact behavior for the adapters that need it
//! (nuget push, swift publish, helm push, pypi upload fallback).

/// Extract the boundary parameter from a `Content-Type` header value.
pub fn boundary_from_content_type(ct: &str) -> Option<String> {
    for part in ct.split(';') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix("boundary=") {
            return Some(rest.trim_matches('"').to_string());
        }
    }
    None
}

/// Extract the first part's body whose Content-Disposition names `field`.
/// Returns the raw bytes (trailing CRLF trimmed).
pub fn extract_field(body: &[u8], content_type: &str, field: &str) -> Option<Vec<u8>> {
    let boundary = boundary_from_content_type(content_type)?;
    extract_field_with_boundary(body, &boundary, field)
}

/// As [`extract_field`], with an explicit boundary.
pub fn extract_field_with_boundary(body: &[u8], boundary: &str, field: &str) -> Option<Vec<u8>> {
    let delim = format!("--{boundary}");
    let delim = delim.as_bytes();
    let mut rest = body;

    loop {
        let idx = find(rest, delim)?;
        rest = &rest[idx + delim.len()..];
        // Terminal boundary "--" right after the delimiter.
        if rest.starts_with(b"--") {
            return None;
        }
        // Skip past this part's headers (blank line).
        let hdr_end = find(rest, b"\r\n\r\n")?;
        let headers = &rest[..hdr_end];
        let content = &rest[hdr_end + 4..];
        // Content ends at the next boundary.
        let next = find(content, delim).unwrap_or(content.len());
        let mut data = &content[..next];
        let want = find_header(headers, field);
        if want {
            while data.ends_with(b"\n") || data.ends_with(b"\r") {
                data = &data[..data.len() - 1];
            }
            return Some(data.to_vec());
        }
        if next == 0 {
            return None;
        }
        rest = &content[next..];
    }
}

/// Extract the filename of the first file part (Content-Disposition filename=),
/// alongside its body. Used by upload endpoints that need the original name.
pub fn extract_first_file(body: &[u8], content_type: &str) -> Option<(Option<String>, Vec<u8>)> {
    let boundary = boundary_from_content_type(content_type)?;
    let delim = format!("--{boundary}");
    let delim = delim.as_bytes();
    let mut rest = body;
    loop {
        let idx = find(rest, delim)?;
        rest = &rest[idx + delim.len()..];
        if rest.starts_with(b"--") {
            return None;
        }
        let hdr_end = find(rest, b"\r\n\r\n")?;
        let headers = &rest[..hdr_end];
        let content = &rest[hdr_end + 4..];
        let next = find(content, delim).unwrap_or(content.len());
        let mut data = &content[..next];
        let filename = filename_from_headers(headers);
        while data.ends_with(b"\n") || data.ends_with(b"\r") {
            data = &data[..data.len() - 1];
        }
        if filename.is_some() {
            return Some((filename, data.to_vec()));
        }
        if next == 0 {
            return None;
        }
        rest = &content[next..];
    }
}

/// Extract a simple (non-file) form field by name, trimmed of whitespace.
pub fn extract_text_field(body: &[u8], content_type: &str, field: &str) -> Option<String> {
    let data = extract_field(body, content_type, field)?;
    Some(String::from_utf8_lossy(&data).trim().to_string())
}

fn find_header(headers: &[u8], field: &str) -> bool {
    let needle = format!("name=\"{field}\"");
    if find(headers, needle.as_bytes()).is_some() {
        return true;
    }
    // Some clients omit the quotes.
    let needle2 = format!("name={field}");
    find(headers, needle2.as_bytes()).is_some()
}

fn filename_from_headers(headers: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(headers);
    for line in s.split("\r\n") {
        if !line.to_ascii_lowercase().starts_with("content-disposition") {
            continue;
        }
        for part in line.split(';') {
            let part = part.trim();
            if let Some(rest) = part.strip_prefix("filename=") {
                let f = rest.trim_matches('"');
                if !f.is_empty() {
                    return Some(f.to_string());
                }
            }
        }
    }
    None
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CT: &str = "multipart/form-data; boundary=XYZ";

    fn body() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"--XYZ\r\n");
        b.extend_from_slice(b"Content-Disposition: form-data; name=\"version\"\r\n\r\n");
        b.extend_from_slice(b"1.2.3\r\n");
        b.extend_from_slice(b"--XYZ\r\n");
        b.extend_from_slice(
            b"Content-Disposition: form-data; name=\"content\"; filename=\"pkg.whl\"\r\n",
        );
        b.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
        b.extend_from_slice(b"\x00\x01BINARY\r\nDATA\x00\r\n");
        b.extend_from_slice(b"--XYZ--\r\n");
        b
    }

    #[test]
    fn extracts_text_field() {
        assert_eq!(extract_text_field(&body(), CT, "version").unwrap(), "1.2.3");
    }

    #[test]
    fn extracts_file_field() {
        let data = extract_field(&body(), CT, "content").unwrap();
        assert_eq!(data, b"\x00\x01BINARY\r\nDATA\x00");
    }

    #[test]
    fn extracts_filename() {
        let (fname, data) = extract_first_file(&body(), CT).unwrap();
        assert_eq!(fname.unwrap(), "pkg.whl");
        assert_eq!(data, b"\x00\x01BINARY\r\nDATA\x00");
    }

    #[test]
    fn missing_field() {
        assert!(extract_field(&body(), CT, "nope").is_none());
    }
}
