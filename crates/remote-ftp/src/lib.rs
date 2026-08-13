#![doc = include_str!("../README.md")]

mod adapter;
mod config;
mod error;
mod path;
mod protocol;
mod remote_core;
mod transport;

pub use adapter::FtpAdapter;
pub use config::{
    Credentials, DataMode, FtpConfig, PLAIN_FTP_ACKNOWLEDGEMENT, PlainFtpConfirmation, SecurityMode,
};
pub use error::{FtpError, FtpFailureKind};
pub use path::RemotePath;
pub use remote_core::RemoteFtpAdapter;
