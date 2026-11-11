use serde_json::Value;

/// Incremental Server-Sent-Events parser yielding the JSON `data:` payload of each event.
#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
}

fn find_boundary(b: &[u8]) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    for (pat, len) in [(&b"\n\n"[..], 2usize), (&b"\r\n\r\n"[..], 4), (&b"\n\r\n"[..], 3)] {
        if let Some(p) = b.windows(pat.len()).position(|w| w == pat)
            && best.is_none_or(|(bp, _)| p < bp)
        {
            best = Some((p, len));
        }
    }
    best
}

impl SseParser {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Value> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some((pos, len)) = find_boundary(&self.buf) {
            let raw: Vec<u8> = self.buf.drain(..pos + len).collect();
            let text = String::from_utf8_lossy(&raw[..pos]);
            let mut data: Vec<&str> = Vec::new();
            for line in text.split('\n') {
                let line = line.strip_suffix('\r').unwrap_or(line);
                if let Some(d) = line.strip_prefix("data:") {
                    data.push(d.strip_prefix(' ').unwrap_or(d));
                }
            }
            if data.is_empty() {
                continue;
            }
            let joined = data.join("\n");
            if joined.trim() == "[DONE]" {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(&joined) {
                out.push(v);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn feed(chunks: &[&[u8]]) -> Vec<Value> {
        let mut p = SseParser::default();
        chunks.iter().flat_map(|c| p.push(c)).collect()
    }

    #[test]
    fn split_mid_event_and_mid_line() {
        let v = feed(&[b"event: a\ndata: {\"a\"", b":1}\n", b"\ndata: {\"b\":2}\n\n"]);
        assert_eq!(v, vec![json!({"a":1}), json!({"b":2})]);
    }

    #[test]
    fn crlf_and_split_crlf() {
        let v = feed(&[b"data: {\"a\":1}\r\n\r", b"\ndata: {\"b\":2}\r\n\r\n"]);
        assert_eq!(v, vec![json!({"a":1}), json!({"b":2})]);
    }

    #[test]
    fn multi_data_and_done() {
        let v = feed(&[b"data: {\"a\":\ndata: 1}\n\ndata: [DONE]\n\n"]);
        assert_eq!(v, vec![json!({"a":1})]);
    }

    #[test]
    fn split_inside_utf8() {
        let s = "data: {\"t\":\"é\"}\n\n".as_bytes();
        let i = s.iter().position(|&b| b == 0xC3).unwrap() + 1;
        let v = feed(&[&s[..i], &s[i..]]);
        assert_eq!(v, vec![json!({"t":"é"})]);
    }
}
