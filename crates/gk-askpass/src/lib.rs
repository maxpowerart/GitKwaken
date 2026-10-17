//! Lets git and ssh ask the GUI for credentials, key passphrases and host-key
//! confirmations.
//!
//! The GUI executable doubles as the askpass helper: git/ssh run
//! `$GIT_ASKPASS "<prompt>"` / `$SSH_ASKPASS "<prompt>"`, the helper (see
//! [`client_main`]) forwards the prompt over loopback TCP to the [`Server`]
//! living in the GUI process, and prints the user's answer to stdout.

mod client;
mod protocol;
mod server;

pub use client::client_main;
pub use server::Server;

/// Env var carrying `<port>:<token>` from the GUI to the helper process.
pub const ENV_VAR: &str = "GITKWAKEN_ASKPASS";

/// A question from git/ssh waiting for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub id: u64,
    /// Prompt text exactly as git/ssh sent it (may span several lines).
    pub text: String,
    pub kind: PromptKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// Plain-text user name (echo input).
    Username,
    /// HTTP or ssh password / PIN (mask input).
    Password,
    /// Private key passphrase (mask input).
    Passphrase,
    /// ssh host-key confirmation; answer `Some("yes")` or cancel.
    YesNo,
    /// Anything else (show text, echo input).
    Other,
}

impl PromptKind {
    /// Classifies a prompt; `hint` is ssh's `SSH_ASKPASS_PROMPT` value.
    pub fn detect(text: &str, hint: &str) -> Self {
        let t = text.to_ascii_lowercase();
        if hint == "confirm" || t.contains("(yes/no") || t.contains("continue connecting") {
            PromptKind::YesNo
        } else if t.contains("passphrase") {
            PromptKind::Passphrase
        } else if t.contains("username") {
            PromptKind::Username
        } else if t.contains("password") || t.contains("pin for") || t.contains("enter pin") {
            PromptKind::Password
        } else {
            PromptKind::Other
        }
    }

    /// Whether the input should be masked in the UI.
    pub fn is_secret(self) -> bool {
        matches!(self, PromptKind::Password | PromptKind::Passphrase)
    }
}

#[cfg(test)]
mod tests {
    use super::PromptKind::{self, *};

    #[test]
    fn detects_real_prompts() {
        let cases = [
            ("Username for 'https://github.com': ", Username),
            ("Password for 'https://user@github.com': ", Password),
            (
                "Enter passphrase for key '/c/Users/x/.ssh/id_ed25519': ",
                Passphrase,
            ),
            ("git@github.com's password: ", Password),
            (
                "Enter PIN for ECDSA-SK key /c/Users/x/.ssh/id_ecdsa_sk: ",
                Password,
            ),
            (
                "The authenticity of host 'github.com (140.82.121.4)' can't be established.\n\
                 ED25519 key fingerprint is SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU.\n\
                 This key is not known by any other names.\n\
                 Are you sure you want to continue connecting (yes/no/[fingerprint])? ",
                YesNo,
            ),
            ("Confirm user presence for key ED25519-SK SHA256:abc", Other),
        ];
        for (text, kind) in cases {
            assert_eq!(PromptKind::detect(text, ""), kind, "{text}");
        }
        assert_eq!(PromptKind::detect("Allow use of key?", "confirm"), YesNo);
        assert!(Password.is_secret() && Passphrase.is_secret() && !Username.is_secret());
    }
}
