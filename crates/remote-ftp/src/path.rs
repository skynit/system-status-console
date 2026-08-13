use std::fmt;

use crate::FtpError;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RemotePath(String);

impl RemotePath {
    /// Creates an absolute remote path safe for FTP URLs and quote commands.
    ///
    /// # Errors
    ///
    /// Returns an error for relative paths or command-injection control characters.
    pub fn new(path: impl Into<String>) -> Result<Self, FtpError> {
        let path = path.into();
        if !path.starts_with('/') {
            return Err(FtpError::Configuration(
                "remote path must be absolute".into(),
            ));
        }
        if path.as_bytes().contains(&0) || path.contains(['\r', '\n']) {
            return Err(FtpError::Configuration(
                "remote path must not contain NUL, CR, or LF".into(),
            ));
        }
        Ok(Self(path))
    }

    #[must_use]
    pub fn root() -> Self {
        Self("/".into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn part_path(&self) -> Result<Self, FtpError> {
        if self.0 == "/" || self.0.ends_with('/') {
            return Err(FtpError::Configuration(
                "a file path is required for a .part transfer".into(),
            ));
        }
        Self::new(format!("{}.part", self.0))
    }

    pub(crate) fn encoded(&self) -> String {
        let mut encoded = String::with_capacity(self.0.len());
        for byte in self.0.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                    encoded.push(char::from(byte));
                }
                _ => {
                    use fmt::Write;
                    write!(encoded, "%{byte:02X}").expect("writing to String cannot fail");
                }
            }
        }
        encoded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_is_encoded_without_losing_hierarchy() {
        let path = RemotePath::new("/目录/a b;%").unwrap();
        assert_eq!(path.encoded(), "/%E7%9B%AE%E5%BD%95/a%20b%3B%25");
    }

    #[test]
    fn command_injection_characters_are_rejected() {
        assert!(RemotePath::new("/safe\r\nDELE /other").is_err());
    }
}
