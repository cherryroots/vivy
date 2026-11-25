//! Server-sent events: the streaming format OpenAI and Claude both use.
//!
//! The response body is text made of events separated by a blank line. Each event has
//! `field: value` lines; only `event` (the name) and `data` (the JSON) matter here:
//!
//! ```text
//! event: response.output_text.delta
//! data: {"type":"response.output_text.delta","delta":"Hel"}
//!
//! ```
//!
//! The network hands over the body in chunks that can end anywhere, even inside a
//! character, so [`SseParser`] keeps the unfinished end until the next chunk arrives.

/// Maximum bytes kept in [`SseParser`]'s buffer before failing: guards against a
/// runaway or malformed stream that never sends a blank-line separator.
const MAX_BUFFER: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Default)]
pub struct SseParser {
    buffer: Vec<u8>,
    /// Bytes already scanned without finding a blank-line separator.
    scanned: usize,
}

impl SseParser {
    /// Adds a chunk of the body and returns the events it completed. Fails if an
    /// unfinished event grows past [`MAX_BUFFER`].
    pub fn push(&mut self, chunk: &[u8]) -> anyhow::Result<Vec<SseEvent>> {
        if self.buffer.len().saturating_add(chunk.len()) > MAX_BUFFER {
            anyhow::bail!(
                "SSE event exceeded {} MiB buffer limit",
                MAX_BUFFER / (1024 * 1024)
            );
        }
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some((end, separator)) = find_blank_line(&self.buffer, self.scanned) {
            let block: Vec<u8> = self.buffer.drain(..end + separator).collect();
            self.scanned = 0;
            if let Some(event) = parse_block(&String::from_utf8_lossy(&block[..end])) {
                events.push(event);
            }
        }
        // Keep the last 3 bytes unscanned so a "\r\n\r\n" split across chunks is found.
        self.scanned = self.buffer.len().saturating_sub(3);
        Ok(events)
    }
}

/// Where the first blank line is at or after `start`, and how many bytes it takes
/// ("\n\n" or "\r\n\r\n").
fn find_blank_line(buffer: &[u8], start: usize) -> Option<(usize, usize)> {
    for i in start..buffer.len() {
        if buffer[i..].starts_with(b"\n\n") {
            return Some((i, 2));
        }
        if buffer[i..].starts_with(b"\r\n\r\n") {
            return Some((i, 4));
        }
    }
    None
}

fn parse_block(block: &str) -> Option<SseEvent> {
    let mut event = None;
    let mut data = Vec::new();
    for line in block.lines() {
        // Lines starting with ":" are comments (keep-alives).
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => event = Some(value.to_string()),
            "data" => data.push(value),
            _ => {}
        }
    }
    if data.is_empty() {
        return None;
    }
    Some(SseEvent {
        event,
        data: data.join("\n"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_split_across_chunks() {
        let mut parser = SseParser::default();
        assert!(parser.push(b"event: a\ndata: {\"x\"").unwrap().is_empty());
        let events = parser
            .push(b":1}\n\n: keep-alive\n\ndata: two\ndata: lines\r\n\r\n")
            .unwrap();
        assert_eq!(
            events,
            [
                SseEvent {
                    event: Some("a".into()),
                    data: "{\"x\":1}".into()
                },
                SseEvent {
                    event: None,
                    data: "two\nlines".into()
                },
            ]
        );
    }

    #[test]
    fn characters_split_across_chunks() {
        let mut parser = SseParser::default();
        let text = "data: 🦀\n\n".as_bytes();
        assert!(parser.push(&text[..8]).unwrap().is_empty());
        assert_eq!(parser.push(&text[8..]).unwrap()[0].data, "🦀");
    }

    #[test]
    fn crlf_separator_split_at_every_byte() {
        let raw = b"data: hello\r\n\r\ndata: world\r\n\r\n";
        for split in 1..raw.len() {
            let mut parser = SseParser::default();
            let mut got = parser.push(&raw[..split]).unwrap();
            got.extend(parser.push(&raw[split..]).unwrap());
            assert_eq!(
                got,
                [
                    SseEvent {
                        event: None,
                        data: "hello".into(),
                    },
                    SseEvent {
                        event: None,
                        data: "world".into(),
                    },
                ],
                "split at {split}"
            );
        }
    }

    #[test]
    fn scanned_cursor_advances_and_resets() {
        let mut parser = SseParser::default();
        assert!(parser.push(b"data: 0123456789").unwrap().is_empty());
        assert_eq!(parser.scanned, 13);
        let events = parser.push(b"\n\n").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "0123456789");
        assert_eq!(parser.scanned, 0);
    }

    #[test]
    fn rejects_events_over_buffer_limit() {
        let mut parser = SseParser::default();
        let big = vec![b'a'; MAX_BUFFER];
        assert!(parser.push(&big).unwrap().is_empty());
        let err = parser.push(b"b").unwrap_err();
        assert!(err.to_string().contains("32 MiB"), "{err}");
    }
}
