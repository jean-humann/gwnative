//! RFC 6455 WebSocket, just enough of it to carry game packets.
//!
//! The game's TCP sockets need a binary, bidirectional, message-framed channel
//! between the WASM client and the host. `WKScriptMessageHandlerWithReply` is
//! the obvious route but its payloads are restricted to the JSON-ish types
//! (NSNumber/NSString/NSDate/NSArray/NSDictionary/NSNull), so every packet would
//! have to be base64'd — a third more bytes plus an encode and a decode on the
//! hot path. A WebSocket to the loopback origin carries binary frames as they
//! are, and needs no Objective-C at all.
//!
//! Only what the harness actually uses is implemented: a server-side handshake,
//! single-frame text and binary messages, continuation frames on receive, ping,
//! and close. No extensions, no compression.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use sha1::Digest as _;

/// The RFC 6455 handshake constant. Cross-checked against a live server's
/// answer for the RFC's own example key: get one character of this wrong and
/// every `Sec-WebSocket-Accept` is wrong, which CFNetwork reports only as a
/// handshake failure with no detail.
const MAGIC: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Largest message accepted from the page. Game packets are kilobytes; this is
/// only here so a malformed length header cannot make the host allocate wildly.
const MAX_MESSAGE: usize = 1 << 20;

/// How long to wait for the peer's own close frame once we have sent ours.
const CLOSING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

pub const TEXT: u8 = 1;
pub const BINARY: u8 = 2;
const CLOSE: u8 = 8;
const PING: u8 = 9;
const PONG: u8 = 10;

pub enum Message {
    Text(String),
    Binary(Vec<u8>),
    Close,
}

/// The write half, shared because both pump directions send on it — one relays
/// TCP payloads, the other answers pings. Frames must not interleave, so every
/// send takes the lock for the whole frame.
#[derive(Clone)]
pub struct Sink(Arc<Mutex<TcpStream>>);

impl Sink {
    pub fn new(stream: TcpStream) -> Self {
        Self(Arc::new(Mutex::new(stream)))
    }

    pub fn send(&self, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
        let mut stream = self.0.lock().unwrap_or_else(|e| e.into_inner());
        write_frame(&mut *stream, opcode, payload)
    }

    fn pong(&self, payload: &[u8]) -> std::io::Result<()> {
        self.send(PONG, payload)
    }

    /// Begin a clean close: send the close frame, then half-close so the peer
    /// sees a FIN.
    ///
    /// Shutting down *both* directions here would be a serious bug rather than
    /// a tidiness issue. Closing a socket that still has unread bytes in its
    /// receive buffer makes the kernel send an RST, and an RST tells the peer
    /// to discard everything it has buffered but not yet delivered to the
    /// application — including frames written moments earlier. The page would
    /// then see the connection fail with no message ever arriving.
    ///
    /// The read side stays open so the peer's own close frame can arrive. The
    /// timeout bounds a peer that never sends one; failures are ignored,
    /// because this only runs on a connection already going away.
    pub fn close(&self) {
        let _ = self.send(CLOSE, &1000u16.to_be_bytes());
        if let Ok(stream) = self.0.lock() {
            let _ = stream.shutdown(std::net::Shutdown::Write);
            let _ = stream.set_read_timeout(Some(CLOSING_TIMEOUT));
        }
    }
}

/// `base64(sha1(key + MAGIC))` — the value the client checks to confirm it is
/// talking to something that understood the upgrade rather than a naive proxy.
fn accept_key(key: &str) -> String {
    let mut hasher = sha1::Sha1::new();
    hasher.update(key.as_bytes());
    hasher.update(MAGIC.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(hasher.finalize())
}

/// Complete the upgrade. The caller has already parsed the request and checked
/// that it asked for a WebSocket.
pub fn accept(stream: &mut TcpStream, key: &str) -> std::io::Result<()> {
    stream.write_all(handshake(key).as_bytes())?;
    stream.flush()
}

fn handshake(key: &str) -> String {
    format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {}\r\n\
         Sec-WebSocket-Protocol: gwnative\r\n\r\n",
        accept_key(key)
    )
}

fn read_exact(reader: &mut impl Read, n: usize) -> std::io::Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    match reader.read_exact(&mut buf) {
        Ok(()) => Ok(buf),
        Err(error) => {
            crate::log::wipe(&mut buf);
            Err(error)
        }
    }
}

#[derive(Default)]
struct WipingBuffer(Vec<u8>);

impl Drop for WipingBuffer {
    fn drop(&mut self) {
        crate::log::wipe(&mut self.0);
    }
}

fn text_message(mut bytes: Vec<u8>) -> String {
    match String::from_utf8(std::mem::take(&mut bytes)) {
        Ok(text) => text,
        Err(error) => {
            let mut bytes = error.into_bytes();
            let text = String::from_utf8_lossy(&bytes).into_owned();
            crate::log::wipe(&mut bytes);
            text
        }
    }
}

/// Read one message from `reader`, reassembling continuation frames and
/// answering pings through `pong`. Returns `Ok(None)` when the peer has gone
/// away.
pub fn read_message(reader: &mut impl Read, out: &Sink) -> std::io::Result<Option<Message>> {
    read_message_with(reader, &mut |payload| out.pong(payload))
}

fn read_message_with(
    reader: &mut impl Read,
    pong: &mut dyn FnMut(&[u8]) -> std::io::Result<()>,
) -> std::io::Result<Option<Message>> {
    let mut payload = WipingBuffer::default();
    let mut kind = 0u8;

    loop {
        let mut head = [0u8; 2];
        match reader.read_exact(&mut head) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }

        let fin = head[0] & 0x80 != 0;
        let opcode = head[0] & 0x0f;
        let masked = head[1] & 0x80 != 0;
        let len = match head[1] & 0x7f {
            126 => u16::from_be_bytes(read_exact(reader, 2)?.try_into().unwrap()) as usize,
            127 => {
                let n = u64::from_be_bytes(read_exact(reader, 8)?.try_into().unwrap());
                usize::try_from(n).unwrap_or(usize::MAX)
            }
            n => n as usize,
        };

        // The spec requires client frames to be masked, and an unmasked one
        // means we are not talking to a browser — fail rather than guess.
        if !masked {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unmasked client frame",
            ));
        }
        if len > MAX_MESSAGE || payload.0.len().saturating_add(len) > MAX_MESSAGE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "websocket message too large",
            ));
        }

        let mask: [u8; 4] = read_exact(reader, 4)?.try_into().unwrap();
        let mut body = read_exact(reader, len)?;
        for (i, byte) in body.iter_mut().enumerate() {
            *byte ^= mask[i & 3];
        }

        match opcode {
            CLOSE => {
                crate::log::wipe(&mut body);
                return Ok(Some(Message::Close));
            }
            // Control frames may be interleaved with a fragmented message, so
            // handle them without disturbing the payload being assembled.
            PING => {
                let result = pong(&body);
                crate::log::wipe(&mut body);
                result?;
                continue;
            }
            PONG => {
                crate::log::wipe(&mut body);
                continue;
            }
            0 => {}
            other => kind = other,
        }

        payload.0.extend_from_slice(&body);
        crate::log::wipe(&mut body);
        if fin {
            return Ok(Some(match kind {
                TEXT => Message::Text(text_message(std::mem::take(&mut payload.0))),
                _ => Message::Binary(std::mem::take(&mut payload.0)),
            }));
        }
    }
}

/// Write one unfragmented server frame. Server frames are never masked.
fn write_frame(stream: &mut impl Write, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut head = Vec::with_capacity(10);
    head.push(0x80 | opcode);
    match payload.len() {
        n if n < 126 => head.push(n as u8),
        n if n <= u16::MAX as usize => {
            head.push(126);
            head.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            head.push(127);
            head.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    // One write_all per frame: two would let a concurrent writer interleave a
    // frame between this header and its payload.
    head.extend_from_slice(payload);
    let result = stream.write_all(&head).and_then(|()| stream.flush());
    crate::log::wipe(&mut head);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first vector is the one printed in RFC 6455 itself, and was checked
    /// against what a live WebSocket server answers for that key. Deriving the
    /// expected value from our own MAGIC would only prove the code agrees with
    /// itself — which it did while MAGIC was wrong, and every real client
    /// refused the upgrade with no diagnosis beyond "handshake failed".
    #[test]
    fn computes_the_accept_key() {
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
        assert_eq!(
            accept_key("x3JJHMbDL1EzLkh9GBhXDw=="),
            "HSmrc0sMlYUkAGmm5OPpG2HaGWk="
        );
    }

    #[test]
    fn handshake_selects_only_the_non_secret_protocol() {
        let response = handshake("dGhlIHNhbXBsZSBub25jZQ==");
        assert!(response.contains("Sec-WebSocket-Protocol: gwnative\r\n"));
        assert!(!response.contains("gwnative-token."));
    }

    /// Build a client frame the way a browser does: masked, with the length in
    /// whichever of the three encodings fits.
    fn client_frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mask = [0xa1, 0xb2, 0xc3, 0xd4];
        let mut out = vec![if fin { 0x80 | opcode } else { opcode }];
        match payload.len() {
            n if n < 126 => out.push(0x80 | n as u8),
            n if n <= u16::MAX as usize => {
                out.push(0x80 | 126);
                out.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                out.push(0x80 | 127);
                out.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        out.extend_from_slice(&mask);
        out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i & 3]));
        out
    }

    fn read_one(bytes: &[u8]) -> std::io::Result<Option<Message>> {
        read_message_with(&mut std::io::Cursor::new(bytes.to_vec()), &mut |_| Ok(()))
    }

    #[test]
    fn unmasks_a_binary_frame() {
        let payload: Vec<u8> = (0..200u32).map(|i| i as u8).collect();
        let frame = client_frame(true, BINARY, &payload);
        let Ok(Some(Message::Binary(got))) = read_one(&frame) else {
            panic!("expected a binary message");
        };
        assert_eq!(got, payload);
    }

    #[test]
    fn reassembles_continuation_frames() {
        let mut bytes = client_frame(false, BINARY, b"first ");
        bytes.extend(client_frame(false, 0, b"second "));
        bytes.extend(client_frame(true, 0, b"third"));
        let Ok(Some(Message::Binary(got))) = read_one(&bytes) else {
            panic!("expected a binary message");
        };
        assert_eq!(got, b"first second third");
    }

    #[test]
    fn answers_a_ping_without_disturbing_the_message() {
        let mut bytes = client_frame(false, TEXT, b"half ");
        bytes.extend(client_frame(true, PING, b"ping payload"));
        bytes.extend(client_frame(true, 0, b"way"));

        let mut pongs: Vec<Vec<u8>> = Vec::new();
        let message = read_message_with(&mut std::io::Cursor::new(bytes), &mut |payload| {
            pongs.push(payload.to_vec());
            Ok(())
        });
        let Ok(Some(Message::Text(text))) = message else {
            panic!("expected a text message");
        };
        assert_eq!(text, "half way");
        assert_eq!(pongs, vec![b"ping payload".to_vec()]);
    }

    #[test]
    fn reads_the_extended_length_encodings() {
        for size in [125usize, 126, 4096, 70_000] {
            let payload = vec![0x5a; size];
            let Ok(Some(Message::Binary(got))) = read_one(&client_frame(true, BINARY, &payload))
            else {
                panic!("expected a binary message at {size} bytes");
            };
            assert_eq!(got.len(), size);
        }
    }

    #[test]
    fn refuses_an_unmasked_client_frame() {
        // Server frames are unmasked, so one arriving inbound means this is not
        // a browser on the other end.
        let mut server_style = Vec::new();
        write_frame(&mut server_style, BINARY, b"hello").unwrap();
        assert!(read_one(&server_style).is_err());
    }

    #[test]
    fn refuses_an_oversized_message() {
        let mut frame = vec![0x80 | BINARY, 0x80 | 127];
        frame.extend_from_slice(&(MAX_MESSAGE as u64 + 1).to_be_bytes());
        frame.extend_from_slice(&[0; 4]);
        assert!(read_one(&frame).is_err());
    }

    #[test]
    fn reports_a_closed_peer_as_end_of_stream() {
        assert!(matches!(read_one(&[]), Ok(None)));
        assert!(matches!(
            read_one(&client_frame(true, CLOSE, &[])),
            Ok(Some(Message::Close))
        ));
    }

    #[test]
    fn writes_unmasked_server_frames() {
        let mut out = Vec::new();
        write_frame(&mut out, BINARY, b"hello").unwrap();
        assert_eq!(out[0], 0x82, "FIN + binary opcode");
        assert_eq!(out[1], 5, "length, mask bit clear");
        assert_eq!(&out[2..], b"hello");
    }
}
