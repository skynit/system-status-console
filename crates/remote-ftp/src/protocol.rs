use curl::easy::InfoType;

use crate::FtpError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Command {
    AuthTls,
    AuthSsl,
    Credential,
    PbszZero,
    PbszOther,
    ProtPrivate,
    ProtOther,
    DataTransfer,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Event {
    Reply(u16),
    Command(Command),
}

#[derive(Debug, Default)]
pub(crate) struct ProtocolTrace {
    events: Vec<Event>,
}

impl ProtocolTrace {
    pub(crate) fn observe(&mut self, info: InfoType, bytes: &[u8]) {
        if !matches!(info, InfoType::HeaderIn | InfoType::HeaderOut) {
            return;
        }
        for line in bytes.split(|byte| *byte == b'\n') {
            let line = trim_ascii(line);
            if line.is_empty() {
                continue;
            }
            if matches!(info, InfoType::HeaderIn) {
                if let Some(code) = reply_code(line) {
                    self.events.push(Event::Reply(code));
                }
            } else {
                self.events.push(Event::Command(classify_command(line)));
            }
        }
    }

    pub(crate) fn verify_explicit_ftps(&self) -> Result<(), FtpError> {
        self.verify_partial_safety()?;

        let banner = self
            .events
            .iter()
            .position(|event| *event == Event::Reply(220))
            .ok_or_else(|| FtpError::Protocol("missing FTP 220 service banner".into()))?;
        let auth = self.next_command(banner).ok_or_else(|| {
            FtpError::Protocol("missing outbound AUTH TLS after FTP 220 banner".into())
        })?;
        match self.events[auth] {
            Event::Command(Command::AuthTls) => {}
            Event::Command(Command::Credential) => {
                return Err(FtpError::Protocol(
                    "FTP credentials were sent before AUTH TLS completed".into(),
                ));
            }
            _ => {
                return Err(FtpError::Protocol(
                    "AUTH TLS was not the first command after FTP 220 banner".into(),
                ));
            }
        }
        let auth_reply = self.require_immediate_reply(auth, 234, "AUTH TLS")?;

        let pbsz = self.require_next_protection_command(
            auth_reply,
            Command::PbszZero,
            "PBSZ 0",
            "unexpected FTP command before PBSZ 0",
        )?;
        let pbsz_reply = self.require_immediate_reply(pbsz, 200, "PBSZ 0")?;
        let prot = self.require_next_protection_command(
            pbsz_reply,
            Command::ProtPrivate,
            "PROT P",
            "unexpected FTP command before PROT P",
        )?;
        self.require_immediate_reply(prot, 200, "PROT P")?;
        Ok(())
    }

    pub(crate) fn verify_for_result(&self, transfer_succeeded: bool) -> Result<(), FtpError> {
        self.verify_partial_safety()?;
        if transfer_succeeded
            || self.events.iter().any(|event| {
                matches!(
                    event,
                    Event::Command(
                        Command::PbszZero
                            | Command::PbszOther
                            | Command::ProtPrivate
                            | Command::ProtOther
                            | Command::DataTransfer
                    )
                )
            })
        {
            self.verify_explicit_ftps()?;
        }
        Ok(())
    }

    fn verify_partial_safety(&self) -> Result<(), FtpError> {
        if self.events.contains(&Event::Command(Command::AuthSsl)) {
            return Err(FtpError::Protocol(
                "server negotiation used AUTH SSL instead of AUTH TLS".into(),
            ));
        }
        if self.events.contains(&Event::Command(Command::PbszOther)) {
            return Err(FtpError::Protocol(
                "FTP protection buffer size was not PBSZ 0".into(),
            ));
        }
        if self.events.contains(&Event::Command(Command::ProtOther)) {
            return Err(FtpError::Protocol(
                "FTP data channel protection was not PROT P".into(),
            ));
        }

        let auth = self
            .events
            .iter()
            .position(|event| *event == Event::Command(Command::AuthTls));
        let Some(auth) = auth else {
            if self.events.contains(&Event::Command(Command::Credential)) {
                return Err(FtpError::Protocol(
                    "FTP credentials were sent before AUTH TLS completed".into(),
                ));
            }
            return Ok(());
        };
        if self.events[..auth].contains(&Event::Command(Command::Credential)) {
            return Err(FtpError::Protocol(
                "FTP credentials were sent before AUTH TLS completed".into(),
            ));
        }

        if let Some((reply_position, code)) = self.next_reply(auth) {
            if code != 234 {
                return Err(FtpError::Protocol(format!(
                    "AUTH TLS received {code}, expected 234"
                )));
            }
            if self.events[auth + 1..reply_position]
                .iter()
                .any(|event| matches!(event, Event::Command(_)))
            {
                return Err(FtpError::Protocol(
                    "AUTH TLS was not immediately followed by reply 234".into(),
                ));
            }
            if self.events[..reply_position].contains(&Event::Command(Command::Credential)) {
                return Err(FtpError::Protocol(
                    "FTP credentials were sent before AUTH TLS completed".into(),
                ));
            }
        } else if self.events[auth + 1..].contains(&Event::Command(Command::Credential)) {
            return Err(FtpError::Protocol(
                "FTP credentials were sent before AUTH TLS completed".into(),
            ));
        }
        Ok(())
    }

    fn next_command(&self, after: usize) -> Option<usize> {
        self.events[after + 1..]
            .iter()
            .position(|event| matches!(event, Event::Command(_)))
            .map(|position| after + 1 + position)
    }

    fn next_reply(&self, after: usize) -> Option<(usize, u16)> {
        self.events[after + 1..]
            .iter()
            .enumerate()
            .find_map(|(position, event)| match event {
                Event::Reply(code) => Some((after + 1 + position, *code)),
                Event::Command(_) => None,
            })
    }

    fn require_immediate_reply(
        &self,
        command: usize,
        expected: u16,
        name: &str,
    ) -> Result<usize, FtpError> {
        match self.events.get(command + 1) {
            Some(Event::Reply(code)) if *code == expected => Ok(command + 1),
            Some(Event::Reply(code)) => Err(FtpError::Protocol(format!(
                "{name} received {code}, expected {expected}"
            ))),
            _ => Err(FtpError::Protocol(format!(
                "{name} was not immediately followed by reply {expected}"
            ))),
        }
    }

    fn require_next_protection_command(
        &self,
        after: usize,
        expected: Command,
        name: &str,
        unexpected: &str,
    ) -> Result<usize, FtpError> {
        for (position, event) in self.events[after + 1..].iter().enumerate() {
            let absolute = after + 1 + position;
            match event {
                Event::Reply(_) | Event::Command(Command::Credential) => {}
                Event::Command(command) if *command == expected => return Ok(absolute),
                Event::Command(_) => return Err(FtpError::Protocol(unexpected.into())),
            }
        }
        Err(FtpError::Protocol(format!(
            "missing {name} protection command"
        )))
    }

    #[cfg(test)]
    fn from_fixture(lines: &[(InfoType, &[u8])]) -> Self {
        let mut trace = Self::default();
        for (info, bytes) in lines {
            trace.observe(*info, bytes);
        }
        trace
    }
}

fn classify_command(line: &[u8]) -> Command {
    let mut fields = line.split(u8::is_ascii_whitespace);
    let verb = fields.next().unwrap_or_default();
    let argument = fields.find(|field| !field.is_empty()).unwrap_or_default();
    if verb.eq_ignore_ascii_case(b"AUTH") {
        if argument.eq_ignore_ascii_case(b"TLS") {
            Command::AuthTls
        } else if argument.eq_ignore_ascii_case(b"SSL") {
            Command::AuthSsl
        } else {
            Command::Other
        }
    } else if matches_ignore_ascii_case(verb, &[b"USER", b"PASS", b"ACCT"]) {
        Command::Credential
    } else if verb.eq_ignore_ascii_case(b"PBSZ") {
        if argument == b"0" {
            Command::PbszZero
        } else {
            Command::PbszOther
        }
    } else if verb.eq_ignore_ascii_case(b"PROT") {
        if argument.eq_ignore_ascii_case(b"P") {
            Command::ProtPrivate
        } else {
            Command::ProtOther
        }
    } else if matches_ignore_ascii_case(
        verb,
        &[b"LIST", b"NLST", b"MLSD", b"RETR", b"STOR", b"APPE"],
    ) {
        Command::DataTransfer
    } else {
        Command::Other
    }
}

fn matches_ignore_ascii_case(value: &[u8], candidates: &[&[u8]]) -> bool {
    candidates
        .iter()
        .any(|candidate| value.eq_ignore_ascii_case(candidate))
}

fn reply_code(line: &[u8]) -> Option<u16> {
    if line.len() < 4 || !matches!(line[3], b' ' | b'-') {
        return None;
    }
    std::str::from_utf8(&line[..3]).ok()?.parse().ok()
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD_EXPLICIT_FTPS: &[(InfoType, &[u8])] = &[
        (InfoType::HeaderIn, b"220 fake ready\r\n"),
        (InfoType::HeaderOut, b"AUTH TLS\r\n"),
        (InfoType::HeaderIn, b"234 begin TLS\r\n"),
        (InfoType::HeaderOut, b"USER operator\r\n"),
        (InfoType::HeaderIn, b"331 password required\r\n"),
        (InfoType::HeaderOut, b"PASS fixture-secret\r\n"),
        (InfoType::HeaderIn, b"230 logged in\r\n"),
        (InfoType::HeaderOut, b"PBSZ 0\r\n"),
        (InfoType::HeaderIn, b"200 PBSZ=0\r\n"),
        (InfoType::HeaderOut, b"PROT P\r\n"),
        (InfoType::HeaderIn, b"200 protected\r\n"),
        (InfoType::HeaderOut, b"RETR /private/report.bin\r\n"),
    ];

    #[test]
    fn accepts_complete_ordered_explicit_ftps_fixture() {
        ProtocolTrace::from_fixture(GOOD_EXPLICIT_FTPS)
            .verify_explicit_ftps()
            .unwrap();
    }

    #[test]
    fn trace_debug_redacts_credentials_and_paths() {
        let debug = format!("{:?}", ProtocolTrace::from_fixture(GOOD_EXPLICIT_FTPS));
        assert!(!debug.contains("operator"));
        assert!(!debug.contains("fixture-secret"));
        assert!(!debug.contains("report.bin"));
    }

    #[test]
    fn rejects_credentials_before_tls_fixture() {
        let fixture = &[
            (InfoType::HeaderIn, b"220 ready".as_slice()),
            (InfoType::HeaderOut, b"USER operator".as_slice()),
            (InfoType::HeaderIn, b"331 password".as_slice()),
            (InfoType::HeaderOut, b"AUTH TLS".as_slice()),
            (InfoType::HeaderIn, b"234 accepted".as_slice()),
        ];
        assert!(
            ProtocolTrace::from_fixture(fixture)
                .verify_explicit_ftps()
                .is_err()
        );
    }

    #[test]
    fn rejects_auth_ssl_fallback_fixture() {
        let fixture = &[
            (InfoType::HeaderIn, b"220 ready".as_slice()),
            (InfoType::HeaderOut, b"AUTH TLS".as_slice()),
            (InfoType::HeaderIn, b"534 rejected".as_slice()),
            (InfoType::HeaderOut, b"AUTH SSL".as_slice()),
            (InfoType::HeaderIn, b"234 accepted".as_slice()),
        ];
        assert!(
            ProtocolTrace::from_fixture(fixture)
                .verify_explicit_ftps()
                .is_err()
        );
    }

    #[test]
    fn rejects_out_of_order_protection_fixture() {
        let fixture = &[
            (InfoType::HeaderIn, b"220 ready".as_slice()),
            (InfoType::HeaderOut, b"AUTH TLS".as_slice()),
            (InfoType::HeaderIn, b"234 accepted".as_slice()),
            (InfoType::HeaderOut, b"PROT P".as_slice()),
            (InfoType::HeaderIn, b"200 protected".as_slice()),
            (InfoType::HeaderOut, b"PBSZ 0".as_slice()),
            (InfoType::HeaderIn, b"200 accepted".as_slice()),
        ];
        assert!(
            ProtocolTrace::from_fixture(fixture)
                .verify_explicit_ftps()
                .is_err()
        );
    }

    #[test]
    fn rejects_data_command_before_private_protection_fixture() {
        let fixture = &[
            (InfoType::HeaderIn, b"220 ready".as_slice()),
            (InfoType::HeaderOut, b"AUTH TLS".as_slice()),
            (InfoType::HeaderIn, b"234 accepted".as_slice()),
            (InfoType::HeaderOut, b"PBSZ 0".as_slice()),
            (InfoType::HeaderIn, b"200 accepted".as_slice()),
            (InfoType::HeaderOut, b"RETR /private".as_slice()),
        ];
        assert!(
            ProtocolTrace::from_fixture(fixture)
                .verify_explicit_ftps()
                .is_err()
        );
    }

    #[test]
    fn rejects_clear_data_channel_fixture() {
        let fixture = &[
            (InfoType::HeaderIn, b"220 ready".as_slice()),
            (InfoType::HeaderOut, b"AUTH TLS".as_slice()),
            (InfoType::HeaderIn, b"234 accepted".as_slice()),
            (InfoType::HeaderOut, b"PBSZ 0".as_slice()),
            (InfoType::HeaderIn, b"200 accepted".as_slice()),
            (InfoType::HeaderOut, b"PROT C".as_slice()),
            (InfoType::HeaderIn, b"200 clear".as_slice()),
        ];
        assert!(
            ProtocolTrace::from_fixture(fixture)
                .verify_explicit_ftps()
                .is_err()
        );
    }

    #[test]
    fn rejects_later_downgrade_after_private_protection() {
        let mut fixture = GOOD_EXPLICIT_FTPS.to_vec();
        fixture.extend_from_slice(&[
            (InfoType::HeaderOut, b"PROT C".as_slice()),
            (InfoType::HeaderIn, b"200 clear".as_slice()),
        ]);

        assert!(
            ProtocolTrace::from_fixture(&fixture)
                .verify_explicit_ftps()
                .is_err()
        );
    }

    #[test]
    fn incomplete_tls_handshake_does_not_mask_transport_certificate_error() {
        let fixture = &[
            (InfoType::HeaderIn, b"220 ready".as_slice()),
            (InfoType::HeaderOut, b"AUTH TLS".as_slice()),
            (InfoType::HeaderIn, b"234 accepted".as_slice()),
        ];
        ProtocolTrace::from_fixture(fixture)
            .verify_for_result(false)
            .unwrap();
    }
}
