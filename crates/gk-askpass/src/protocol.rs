//! Wire format between the askpass helper process and the GUI.
//!
//! Request:  `GKAP` + version byte + field(token) + field(prompt) + field(hint)
//! Response: status byte (0 = answered, 1 = cancelled) + field(reply)
//! field  =  u32 little-endian length + UTF-8 bytes (max [`MAX_FIELD`]).

use anyhow::{Context as _, Result, bail};
use std::io::{Read, Write};

pub const MAGIC: &[u8; 4] = b"GKAP";
pub const VERSION: u8 = 1;
pub const MAX_FIELD: usize = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub token: String,
    pub prompt: String,
    /// Value of `SSH_ASKPASS_PROMPT` ("confirm", "none" or empty).
    pub hint: String,
}

fn write_field(w: &mut impl Write, s: &str) -> Result<()> {
    if s.len() > MAX_FIELD {
        bail!("askpass field too long");
    }
    w.write_all(&(s.len() as u32).to_le_bytes())?;
    w.write_all(s.as_bytes())?;
    Ok(())
}

fn read_field(r: &mut impl Read) -> Result<String> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FIELD {
        bail!("askpass field too long");
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    String::from_utf8(buf).context("askpass field is not UTF-8")
}

pub fn write_request(w: &mut impl Write, req: &Request) -> Result<()> {
    // Assemble first so the request goes out in a single write.
    let mut buf = Vec::with_capacity(16 + req.token.len() + req.prompt.len() + req.hint.len());
    buf.extend_from_slice(MAGIC);
    buf.push(VERSION);
    write_field(&mut buf, &req.token)?;
    write_field(&mut buf, &req.prompt)?;
    write_field(&mut buf, &req.hint)?;
    w.write_all(&buf)?;
    w.flush()?;
    Ok(())
}

pub fn read_request(r: &mut impl Read) -> Result<Request> {
    let mut head = [0u8; 5];
    r.read_exact(&mut head)?;
    if &head[..4] != MAGIC || head[4] != VERSION {
        bail!("not an askpass request");
    }
    Ok(Request {
        token: read_field(r)?,
        prompt: read_field(r)?,
        hint: read_field(r)?,
    })
}

/// `None` = cancelled.
pub fn write_reply(w: &mut impl Write, reply: Option<&str>) -> Result<()> {
    let mut buf = Vec::with_capacity(8 + reply.map_or(0, str::len));
    buf.push(if reply.is_some() { 0 } else { 1 });
    write_field(&mut buf, reply.unwrap_or(""))?;
    w.write_all(&buf)?;
    w.flush()?;
    Ok(())
}

pub fn read_reply(r: &mut impl Read) -> Result<Option<String>> {
    let mut status = [0u8; 1];
    r.read_exact(&mut status)?;
    let value = read_field(r)?;
    match status[0] {
        0 => Ok(Some(value)),
        1 => Ok(None),
        s => bail!("bad askpass reply status {s}"),
    }
}

/// Compares secrets without an early exit on the first mismatching byte.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let diff = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(diff) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let req = Request {
            token: "ab".repeat(32),
            prompt: "Password for 'https://user@github.com': ".into(),
            hint: "confirm".into(),
        };
        let mut buf = Vec::new();
        write_request(&mut buf, &req).unwrap();
        assert_eq!(read_request(&mut buf.as_slice()).unwrap(), req);
    }

    #[test]
    fn reply_roundtrip() {
        for reply in [Some("s3cr\u{e9}t"), Some(""), None] {
            let mut buf = Vec::new();
            write_reply(&mut buf, reply).unwrap();
            assert_eq!(read_reply(&mut buf.as_slice()).unwrap().as_deref(), reply);
        }
    }

    #[test]
    fn rejects_garbage_and_oversized() {
        assert!(read_request(&mut &b"GET / HTTP/1.1\r\n\r\n"[..]).is_err());
        let mut buf = Vec::from(&MAGIC[..]);
        buf.push(VERSION);
        buf.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(read_request(&mut buf.as_slice()).is_err());
        // Truncated input.
        assert!(read_reply(&mut &[0u8, 5, 0, 0, 0, b'a'][..]).is_err());
    }

    #[test]
    fn constant_time_eq() {
        assert!(ct_eq(b"abcd", b"abcd"));
        assert!(!ct_eq(b"abcd", b"abce"));
        assert!(!ct_eq(b"abcd", b"abc"));
    }
}
