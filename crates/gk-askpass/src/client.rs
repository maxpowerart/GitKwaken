//! Helper side: runs inside the short-lived process git/ssh spawn.

use crate::ENV_VAR;
use crate::protocol::{Request, read_reply, write_request};
use anyhow::{Context as _, Result};
use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

/// Call first thing in the GUI's `main()`. Returns `None` for a normal GUI
/// start; otherwise acts as the askpass helper and returns the exit code
/// (0 = answered, 1 = cancelled or failed). Never opens a window.
pub fn client_main() -> Option<i32> {
    let spec = std::env::var(ENV_VAR).ok()?;
    let prompt = std::env::args_os()
        .nth(1)
        .map(|a| a.to_string_lossy().into_owned());
    let hint = std::env::var("SSH_ASKPASS_PROMPT").unwrap_or_default();
    Some(match run(&spec, prompt.unwrap_or_default(), hint) {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(e) => {
            // Shows up in git's stderr; never contains the secret.
            eprintln!("gitkwaken askpass: {e:#}");
            1
        }
    })
}

fn run(spec: &str, prompt: String, hint: String) -> Result<bool> {
    let (port, token) = spec
        .split_once(':')
        .context("malformed GITKWAKEN_ASKPASS")?;
    let port: u16 = port.parse().context("malformed GITKWAKEN_ASKPASS port")?;
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
        .context("GitKwaken is not reachable")?;
    // The server cancels after 5 minutes; this only guards against a hung GUI.
    stream.set_read_timeout(Some(Duration::from_secs(6 * 60)))?;
    write_request(
        &mut stream,
        &Request {
            token: token.to_owned(),
            prompt,
            hint,
        },
    )?;
    let Some(reply) = read_reply(&mut stream)? else {
        return Ok(false);
    };
    // Works for a pipe even in a `windows_subsystem = "windows"` binary: the
    // inherited std handles are used as-is.
    let mut out = std::io::stdout().lock();
    out.write_all(reply.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(true)
}
