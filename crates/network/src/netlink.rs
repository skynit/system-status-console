use crate::{CounterWidth, InterfaceId, InterfaceKind, LinkCounters, RawInterface};
use std::{
    ffi::CStr,
    fmt, io, mem,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const RTM_NEWLINK: u16 = 16;
const RTM_GETLINK: u16 = 18;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const NLMSG_OVERRUN: u16 = 4;
const NLM_F_REQUEST: u16 = 0x01;
const NLM_F_DUMP_INTR: u16 = 0x10;
const NLM_F_DUMP: u16 = 0x300;
const IFLA_IFNAME: u16 = 3;
const IFLA_STATS: u16 = 7;
const IFLA_LINKINFO: u16 = 18;
const IFLA_STATS64: u16 = 23;
const IFLA_INFO_KIND: u16 = 1;
const IFF_UP: u32 = 0x1;
const IFF_LOOPBACK: u32 = 0x8;
const IFF_LOWER_UP: u32 = 0x1_0000;
const ARPHRD_LOOPBACK: u16 = 772;
pub const MAX_DUMP_DATAGRAM_BYTES: usize = 64 * 1024;
pub const MAX_RAW_INTERFACES: usize = 256;
pub const MAX_DUMP_DATAGRAMS: usize = 256;
pub const MAX_DUMP_BYTES: usize = 8 * 1024 * 1024;
pub const RTNETLINK_RECEIVE_DEADLINE: Duration = Duration::from_secs(2);
const MAX_DUMP_ATTEMPTS: usize = 2;

#[repr(C)]
#[derive(Clone, Copy)]
struct NetlinkHeader {
    length: u32,
    message_type: u16,
    flags: u16,
    sequence: u32,
    port_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct InterfaceInfo {
    family: u8,
    padding: u8,
    hardware_type: u16,
    index: i32,
    flags: u32,
    change: u32,
}

#[repr(C)]
struct LinkDumpRequest {
    header: NetlinkHeader,
    interface: InterfaceInfo,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DumpLimit {
    DatagramBytes,
    Datagrams,
    Bytes,
    Interfaces,
}

#[derive(Debug)]
pub enum CollectError {
    Io(io::Error),
    Protocol(&'static str),
    Kernel(io::Error),
    ReceiveTimeout {
        deadline: Duration,
    },
    DumpInterrupted,
    DumpOverrun,
    NonKernelSender {
        port_id: u32,
    },
    DumpLimitExceeded {
        limit: DumpLimit,
        maximum: usize,
        observed: usize,
    },
}

impl fmt::Display for CollectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "rtnetlink I/O failed: {error}"),
            Self::Protocol(reason) => write!(formatter, "rtnetlink response invalid: {reason}"),
            Self::Kernel(error) => write!(formatter, "rtnetlink kernel error: {error}"),
            Self::ReceiveTimeout { deadline } => write!(
                formatter,
                "rtnetlink dump did not complete within {} ms",
                deadline.as_millis()
            ),
            Self::DumpInterrupted => {
                write!(
                    formatter,
                    "rtnetlink dump was interrupted by concurrent link changes"
                )
            }
            Self::DumpOverrun => write!(formatter, "rtnetlink reported lost dump data"),
            Self::NonKernelSender { port_id } => write!(
                formatter,
                "rtnetlink response came from non-kernel port {port_id}"
            ),
            Self::DumpLimitExceeded {
                limit,
                maximum,
                observed,
            } => write!(
                formatter,
                "rtnetlink dump exceeded {limit:?} limit: observed {observed}, maximum {maximum}"
            ),
        }
    }
}

impl std::error::Error for CollectError {}

impl CollectError {
    pub const fn is_retryable(&self) -> bool {
        matches!(self, Self::DumpInterrupted | Self::DumpOverrun)
    }
}

impl From<io::Error> for CollectError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub struct RtnetlinkCollector {
    sys_class_net: PathBuf,
    sequence: u32,
}

impl RtnetlinkCollector {
    pub fn new(sys_class_net: PathBuf) -> Self {
        Self {
            sys_class_net,
            sequence: 0,
        }
    }

    pub fn collect(&mut self) -> Result<Vec<RawInterface>, CollectError> {
        let deadline = Instant::now()
            .checked_add(RTNETLINK_RECEIVE_DEADLINE)
            .ok_or(CollectError::Protocol("rtnetlink deadline overflow"))?;
        collect_dump_with_retry(|| {
            self.sequence = self.sequence.wrapping_add(1).max(1);
            let socket = open_route_socket()?;
            send_link_dump(socket.as_raw_fd(), self.sequence)?;
            receive_link_dump(
                socket.as_raw_fd(),
                self.sequence,
                &self.sys_class_net,
                deadline,
            )
        })
    }
}

fn collect_dump_with_retry<F>(mut collect_once: F) -> Result<Vec<RawInterface>, CollectError>
where
    F: FnMut() -> Result<Vec<RawInterface>, CollectError>,
{
    for attempt in 0..MAX_DUMP_ATTEMPTS {
        match collect_once() {
            Err(error) if error.is_retryable() && attempt + 1 < MAX_DUMP_ATTEMPTS => continue,
            result => return result,
        }
    }
    unreachable!("MAX_DUMP_ATTEMPTS is non-zero")
}

pub fn boottime_now() -> Result<Duration, CollectError> {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: value points to writable storage for the duration of the call.
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut value) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    if value.tv_sec < 0 || value.tv_nsec < 0 {
        return Err(CollectError::Protocol(
            "CLOCK_BOOTTIME returned a negative value",
        ));
    }
    Ok(Duration::new(value.tv_sec as u64, value.tv_nsec as u32))
}

fn open_route_socket() -> Result<OwnedFd, CollectError> {
    // SAFETY: socket has no pointer arguments. A successful descriptor is owned below.
    let raw_fd = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            libc::NETLINK_ROUTE,
        )
    };
    if raw_fd < 0 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: raw_fd was returned uniquely by socket and is not otherwise owned.
    let socket = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    // SAFETY: zero is a valid initial representation for sockaddr_nl.
    let mut address: libc::sockaddr_nl = unsafe { mem::zeroed() };
    address.nl_family = libc::AF_NETLINK as u16;
    // SAFETY: address has the type and length passed to bind.
    let result = unsafe {
        libc::bind(
            socket.as_raw_fd(),
            (&raw const address).cast::<libc::sockaddr>(),
            mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(socket)
}

fn send_link_dump(fd: libc::c_int, sequence: u32) -> Result<(), CollectError> {
    let request = LinkDumpRequest {
        header: NetlinkHeader {
            length: mem::size_of::<LinkDumpRequest>() as u32,
            message_type: RTM_GETLINK,
            flags: NLM_F_REQUEST | NLM_F_DUMP,
            sequence,
            port_id: 0,
        },
        interface: InterfaceInfo {
            family: libc::AF_UNSPEC as u8,
            padding: 0,
            hardware_type: 0,
            index: 0,
            flags: 0,
            change: 0,
        },
    };
    // SAFETY: zero is a valid initial representation for sockaddr_nl.
    let mut kernel: libc::sockaddr_nl = unsafe { mem::zeroed() };
    kernel.nl_family = libc::AF_NETLINK as u16;
    // SAFETY: request and kernel are valid for their supplied byte lengths.
    let sent = unsafe {
        libc::sendto(
            fd,
            (&raw const request).cast(),
            mem::size_of::<LinkDumpRequest>(),
            0,
            (&raw const kernel).cast::<libc::sockaddr>(),
            mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        )
    };
    if sent < 0 {
        return Err(io::Error::last_os_error().into());
    }
    if sent as usize != mem::size_of::<LinkDumpRequest>() {
        return Err(CollectError::Protocol("short rtnetlink request write"));
    }
    Ok(())
}

fn receive_link_dump(
    fd: libc::c_int,
    sequence: u32,
    sys_class_net: &Path,
    deadline: Instant,
) -> Result<Vec<RawInterface>, CollectError> {
    let mut interfaces = Vec::new();
    let mut buffer = vec![0_u8; MAX_DUMP_DATAGRAM_BYTES];
    let mut budget = DumpBudget::default();
    loop {
        wait_until_readable(fd, deadline)?;
        // MSG_TRUNC makes an oversized datagram observable instead of silently partial.
        // recvfrom also supplies the sender address so userspace injections cannot be
        // mistaken for kernel telemetry.
        // SAFETY: buffer and source point to writable storage of the supplied lengths.
        let mut source: libc::sockaddr_nl = unsafe { mem::zeroed() };
        let mut source_len = mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t;
        let received = unsafe {
            libc::recvfrom(
                fd,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                libc::MSG_TRUNC,
                (&raw mut source).cast::<libc::sockaddr>(),
                &mut source_len,
            )
        };
        if received < 0 {
            let error = io::Error::last_os_error();
            if matches!(
                error.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            ) {
                continue;
            }
            return Err(error.into());
        }
        let received = received as usize;
        if received > buffer.len() {
            return Err(CollectError::DumpLimitExceeded {
                limit: DumpLimit::DatagramBytes,
                maximum: MAX_DUMP_DATAGRAM_BYTES,
                observed: received,
            });
        }
        if received == 0 {
            return Err(CollectError::Protocol(
                "rtnetlink dump ended without NLMSG_DONE",
            ));
        }
        validate_kernel_sender(&source, source_len)?;
        budget.record_datagram(received)?;

        let mut offset = 0;
        while offset + mem::size_of::<NetlinkHeader>() <= received {
            let header = read_header(&buffer[offset..received])?;
            let message_len = header.length as usize;
            if message_len < mem::size_of::<NetlinkHeader>() || offset + message_len > received {
                return Err(CollectError::Protocol("invalid netlink message length"));
            }
            let payload = &buffer[offset + mem::size_of::<NetlinkHeader>()..offset + message_len];
            if validate_dump_header(header, sequence)? {
                match header.message_type {
                    NLMSG_DONE => return Ok(interfaces),
                    NLMSG_ERROR => parse_netlink_error(payload)?,
                    RTM_NEWLINK => {
                        budget.record_interface()?;
                        interfaces.push(parse_link(payload, sys_class_net)?);
                    }
                    _ => {}
                }
            }
            offset = offset
                .checked_add(align4(message_len))
                .ok_or(CollectError::Protocol("netlink offset overflow"))?;
        }
    }
}

fn validate_dump_header(header: NetlinkHeader, sequence: u32) -> Result<bool, CollectError> {
    if header.message_type == NLMSG_OVERRUN {
        return Err(CollectError::DumpOverrun);
    }
    if header.sequence != sequence {
        return Ok(false);
    }
    if header.flags & NLM_F_DUMP_INTR != 0 {
        return Err(CollectError::DumpInterrupted);
    }
    Ok(true)
}

fn validate_kernel_sender(
    source: &libc::sockaddr_nl,
    source_len: libc::socklen_t,
) -> Result<(), CollectError> {
    if (source_len as usize) < mem::size_of::<libc::sockaddr_nl>()
        || source.nl_family != libc::AF_NETLINK as u16
    {
        return Err(CollectError::Protocol(
            "rtnetlink sender address was truncated or invalid",
        ));
    }
    if source.nl_pid != 0 {
        return Err(CollectError::NonKernelSender {
            port_id: source.nl_pid,
        });
    }
    Ok(())
}

#[derive(Default)]
struct DumpBudget {
    datagrams: usize,
    bytes: usize,
    interfaces: usize,
}

impl DumpBudget {
    fn record_datagram(&mut self, bytes: usize) -> Result<(), CollectError> {
        self.datagrams = self
            .datagrams
            .checked_add(1)
            .ok_or(CollectError::DumpLimitExceeded {
                limit: DumpLimit::Datagrams,
                maximum: MAX_DUMP_DATAGRAMS,
                observed: usize::MAX,
            })?;
        enforce_dump_limit(DumpLimit::Datagrams, self.datagrams, MAX_DUMP_DATAGRAMS)?;
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .ok_or(CollectError::DumpLimitExceeded {
                limit: DumpLimit::Bytes,
                maximum: MAX_DUMP_BYTES,
                observed: usize::MAX,
            })?;
        enforce_dump_limit(DumpLimit::Bytes, self.bytes, MAX_DUMP_BYTES)
    }

    fn record_interface(&mut self) -> Result<(), CollectError> {
        self.interfaces =
            self.interfaces
                .checked_add(1)
                .ok_or(CollectError::DumpLimitExceeded {
                    limit: DumpLimit::Interfaces,
                    maximum: MAX_RAW_INTERFACES,
                    observed: usize::MAX,
                })?;
        enforce_dump_limit(DumpLimit::Interfaces, self.interfaces, MAX_RAW_INTERFACES)
    }
}

fn enforce_dump_limit(
    limit: DumpLimit,
    observed: usize,
    maximum: usize,
) -> Result<(), CollectError> {
    if observed > maximum {
        return Err(CollectError::DumpLimitExceeded {
            limit,
            maximum,
            observed,
        });
    }
    Ok(())
}

fn wait_until_readable(fd: libc::c_int, deadline: Instant) -> Result<(), CollectError> {
    loop {
        let now = Instant::now();
        let Some(remaining) = deadline.checked_duration_since(now) else {
            return Err(CollectError::ReceiveTimeout {
                deadline: RTNETLINK_RECEIVE_DEADLINE,
            });
        };
        if remaining.is_zero() {
            return Err(CollectError::ReceiveTimeout {
                deadline: RTNETLINK_RECEIVE_DEADLINE,
            });
        }
        let timeout_ms = duration_to_poll_timeout(remaining);
        let mut descriptor = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: descriptor points to one initialized pollfd for the duration of poll.
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if result > 0 {
            if descriptor.revents & libc::POLLNVAL != 0 {
                return Err(io::Error::from_raw_os_error(libc::EBADF).into());
            }
            return Ok(());
        }
        if result == 0 {
            return Err(CollectError::ReceiveTimeout {
                deadline: RTNETLINK_RECEIVE_DEADLINE,
            });
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error.into());
        }
    }
}

fn duration_to_poll_timeout(duration: Duration) -> libc::c_int {
    let milliseconds = duration.as_millis().saturating_add(u128::from(
        !duration.subsec_nanos().is_multiple_of(1_000_000),
    ));
    milliseconds.clamp(1, libc::c_int::MAX as u128) as libc::c_int
}

fn read_header(bytes: &[u8]) -> Result<NetlinkHeader, CollectError> {
    if bytes.len() < mem::size_of::<NetlinkHeader>() {
        return Err(CollectError::Protocol("truncated netlink header"));
    }
    // SAFETY: length was checked and read_unaligned does not require alignment.
    Ok(unsafe { (bytes.as_ptr().cast::<NetlinkHeader>()).read_unaligned() })
}

fn parse_netlink_error(payload: &[u8]) -> Result<(), CollectError> {
    let Some(code) = read_i32(payload) else {
        return Err(CollectError::Protocol("truncated NLMSG_ERROR"));
    };
    if code == 0 {
        return Ok(());
    }
    Err(CollectError::Kernel(io::Error::from_raw_os_error(-code)))
}

fn parse_link(payload: &[u8], sys_class_net: &Path) -> Result<RawInterface, CollectError> {
    if payload.len() < mem::size_of::<InterfaceInfo>() {
        return Err(CollectError::Protocol("truncated ifinfomsg"));
    }
    // SAFETY: length was checked and read_unaligned does not require alignment.
    let info = unsafe { (payload.as_ptr().cast::<InterfaceInfo>()).read_unaligned() };
    if info.index <= 0 {
        return Err(CollectError::Protocol("invalid interface index"));
    }
    let attributes = &payload[align4(mem::size_of::<InterfaceInfo>())..];
    let mut name = None;
    let mut counters64 = None;
    let mut counters32 = None;
    let mut kernel_kind = None;
    for attribute in Attributes::new(attributes) {
        let (kind, value) = attribute?;
        match kind {
            IFLA_IFNAME => name = parse_nul_string(value),
            IFLA_STATS64 => counters64 = parse_stats64(value),
            IFLA_STATS => counters32 = parse_stats32(value),
            IFLA_LINKINFO => kernel_kind = parse_link_kind(value)?,
            _ => {}
        }
    }
    let name = name.ok_or(CollectError::Protocol("link is missing IFLA_IFNAME"))?;
    let counters = counters64.or(counters32);
    let kind = classify_interface(
        &name,
        info.hardware_type,
        info.flags,
        kernel_kind.as_deref(),
        sys_class_net,
    );
    Ok(RawInterface {
        id: InterfaceId {
            index: info.index as u32,
            name,
        },
        kind,
        kernel_kind,
        is_up: info.flags & IFF_UP != 0,
        carrier_up: info.flags & IFF_LOWER_UP != 0,
        counters,
    })
}

fn parse_link_kind(attributes: &[u8]) -> Result<Option<String>, CollectError> {
    for attribute in Attributes::new(attributes) {
        let (kind, value) = attribute?;
        if kind == IFLA_INFO_KIND {
            return Ok(parse_nul_string(value));
        }
    }
    Ok(None)
}

fn parse_stats64(bytes: &[u8]) -> Option<LinkCounters> {
    Some(LinkCounters {
        rx_bytes: read_u64(bytes.get(16..)?)?,
        tx_bytes: read_u64(bytes.get(24..)?)?,
        width: CounterWidth::Bits64,
    })
}

fn parse_stats32(bytes: &[u8]) -> Option<LinkCounters> {
    Some(LinkCounters {
        rx_bytes: read_u32(bytes.get(8..)?)? as u64,
        tx_bytes: read_u32(bytes.get(12..)?)? as u64,
        width: CounterWidth::Bits32,
    })
}

fn parse_nul_string(bytes: &[u8]) -> Option<String> {
    let terminated = CStr::from_bytes_until_nul(bytes).ok()?;
    terminated.to_str().ok().map(str::to_owned)
}

fn classify_interface(
    name: &str,
    hardware_type: u16,
    flags: u32,
    kernel_kind: Option<&str>,
    sys_class_net: &Path,
) -> InterfaceKind {
    if hardware_type == ARPHRD_LOOPBACK || flags & IFF_LOOPBACK != 0 {
        return InterfaceKind::Loopback;
    }
    if kernel_kind.is_some_and(is_tunnel_kind) {
        return InterfaceKind::Tunnel;
    }
    if !name.contains('/') && sys_class_net.join(name).join("device").exists() {
        return InterfaceKind::Physical;
    }
    InterfaceKind::Virtual
}

fn is_tunnel_kind(kind: &str) -> bool {
    matches!(
        kind,
        "tun"
            | "tap"
            | "wireguard"
            | "gre"
            | "gretap"
            | "ip6gre"
            | "ip6gretap"
            | "ipip"
            | "sit"
            | "vti"
            | "vti6"
            | "xfrm"
            | "erspan"
            | "ip6erspan"
            | "l2tp"
            | "ppp"
    )
}

struct Attributes<'a> {
    remaining: &'a [u8],
}

impl<'a> Attributes<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { remaining: bytes }
    }
}

impl<'a> Iterator for Attributes<'a> {
    type Item = Result<(u16, &'a [u8]), CollectError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining.is_empty() {
            return None;
        }
        if self.remaining.len() < 4 {
            self.remaining = &[];
            return Some(Err(CollectError::Protocol("truncated rtnetlink attribute")));
        }
        let length = read_u16(self.remaining).unwrap_or(0) as usize;
        let kind = read_u16(&self.remaining[2..]).unwrap_or(0) & 0x3fff;
        if length < 4 || length > self.remaining.len() {
            self.remaining = &[];
            return Some(Err(CollectError::Protocol(
                "invalid rtnetlink attribute length",
            )));
        }
        let value = &self.remaining[4..length];
        let aligned = align4(length);
        self.remaining = if aligned >= self.remaining.len() {
            &[]
        } else {
            &self.remaining[aligned..]
        };
        Some(Ok((kind, value)))
    }
}

const fn align4(value: usize) -> usize {
    (value + 3) & !3
}

fn read_u16(bytes: &[u8]) -> Option<u16> {
    Some(u16::from_ne_bytes(bytes.get(..2)?.try_into().ok()?))
}

fn read_u32(bytes: &[u8]) -> Option<u32> {
    Some(u32::from_ne_bytes(bytes.get(..4)?.try_into().ok()?))
}

fn read_i32(bytes: &[u8]) -> Option<i32> {
    Some(i32::from_ne_bytes(bytes.get(..4)?.try_into().ok()?))
}

fn read_u64(bytes: &[u8]) -> Option<u64> {
    Some(u64::from_ne_bytes(bytes.get(..8)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_limit(error: CollectError, expected: DumpLimit, maximum: usize) {
        assert!(matches!(
            error,
            CollectError::DumpLimitExceeded {
                limit,
                maximum: actual_maximum,
                observed,
            } if limit == expected && actual_maximum == maximum && observed == maximum + 1
        ));
    }

    #[test]
    fn stats64_uses_byte_fields_not_packet_fields() {
        let mut bytes = vec![0_u8; 32];
        bytes[0..8].copy_from_slice(&11_u64.to_ne_bytes());
        bytes[8..16].copy_from_slice(&12_u64.to_ne_bytes());
        bytes[16..24].copy_from_slice(&1_000_u64.to_ne_bytes());
        bytes[24..32].copy_from_slice(&2_000_u64.to_ne_bytes());
        assert_eq!(
            parse_stats64(&bytes),
            Some(LinkCounters {
                rx_bytes: 1_000,
                tx_bytes: 2_000,
                width: CounterWidth::Bits64,
            })
        );
    }

    #[test]
    fn tunnel_kinds_are_explicit_not_name_guesses() {
        assert!(is_tunnel_kind("tun"));
        assert!(is_tunnel_kind("wireguard"));
        assert!(!is_tunnel_kind("bridge"));
    }

    #[test]
    fn dump_budget_accepts_exact_limits_and_rejects_next_value() {
        assert_eq!(MAX_RAW_INTERFACES, 256);
        assert_limit(
            enforce_dump_limit(
                DumpLimit::DatagramBytes,
                MAX_DUMP_DATAGRAM_BYTES + 1,
                MAX_DUMP_DATAGRAM_BYTES,
            )
            .unwrap_err(),
            DumpLimit::DatagramBytes,
            MAX_DUMP_DATAGRAM_BYTES,
        );

        let mut datagrams = DumpBudget {
            datagrams: MAX_DUMP_DATAGRAMS - 1,
            ..DumpBudget::default()
        };
        datagrams.record_datagram(1).unwrap();
        assert_limit(
            datagrams.record_datagram(1).unwrap_err(),
            DumpLimit::Datagrams,
            MAX_DUMP_DATAGRAMS,
        );

        let mut bytes = DumpBudget::default();
        bytes.record_datagram(MAX_DUMP_BYTES).unwrap();
        assert_limit(
            bytes.record_datagram(1).unwrap_err(),
            DumpLimit::Bytes,
            MAX_DUMP_BYTES,
        );

        let mut interfaces = DumpBudget {
            interfaces: MAX_RAW_INTERFACES - 1,
            ..DumpBudget::default()
        };
        interfaces.record_interface().unwrap();
        assert_limit(
            interfaces.record_interface().unwrap_err(),
            DumpLimit::Interfaces,
            MAX_RAW_INTERFACES,
        );
    }

    #[test]
    fn expired_receive_deadline_is_a_typed_timeout_without_polling() {
        let expired = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        assert!(matches!(
            wait_until_readable(-1, expired),
            Err(CollectError::ReceiveTimeout { deadline })
                if deadline == RTNETLINK_RECEIVE_DEADLINE
        ));
    }

    #[test]
    fn poll_timeout_rounds_up_and_stays_bounded() {
        assert_eq!(duration_to_poll_timeout(Duration::from_nanos(1)), 1);
        assert_eq!(duration_to_poll_timeout(Duration::from_millis(2)), 2);
        assert_eq!(
            duration_to_poll_timeout(Duration::from_secs(u64::MAX)),
            libc::c_int::MAX
        );
    }

    #[test]
    fn sender_must_be_the_kernel_port() {
        // SAFETY: zero is a valid sockaddr_nl representation.
        let mut sender: libc::sockaddr_nl = unsafe { mem::zeroed() };
        sender.nl_family = libc::AF_NETLINK as u16;
        assert!(
            validate_kernel_sender(
                &sender,
                mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t
            )
            .is_ok()
        );
        sender.nl_pid = 42;
        assert!(matches!(
            validate_kernel_sender(
                &sender,
                mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t
            ),
            Err(CollectError::NonKernelSender { port_id: 42 })
        ));
    }

    #[test]
    fn interrupted_and_overrun_dump_states_are_typed_failures() {
        let header = NetlinkHeader {
            length: mem::size_of::<NetlinkHeader>() as u32,
            message_type: NLMSG_DONE,
            flags: NLM_F_DUMP_INTR,
            sequence: 7,
            port_id: 0,
        };
        assert!(matches!(
            validate_dump_header(header, 7),
            Err(CollectError::DumpInterrupted)
        ));

        let overrun = NetlinkHeader {
            message_type: NLMSG_OVERRUN,
            flags: 0,
            ..header
        };
        assert!(matches!(
            validate_dump_header(overrun, 7),
            Err(CollectError::DumpOverrun)
        ));
        assert!(!validate_dump_header(header, 8).unwrap());
    }

    #[test]
    fn interrupted_dump_retries_once_then_succeeds() {
        let mut attempts = 0;
        let result = collect_dump_with_retry(|| {
            attempts += 1;
            if attempts == 1 {
                Err(CollectError::DumpInterrupted)
            } else {
                Ok(Vec::new())
            }
        });

        assert!(result.is_ok());
        assert_eq!(attempts, MAX_DUMP_ATTEMPTS);
    }

    #[test]
    fn overrun_retry_is_bounded_and_non_retryable_errors_are_not_retried() {
        let mut overrun_attempts = 0;
        let overrun = collect_dump_with_retry(|| {
            overrun_attempts += 1;
            Err(CollectError::DumpOverrun)
        });
        assert!(matches!(overrun, Err(CollectError::DumpOverrun)));
        assert_eq!(overrun_attempts, MAX_DUMP_ATTEMPTS);

        let mut protocol_attempts = 0;
        let protocol = collect_dump_with_retry(|| {
            protocol_attempts += 1;
            Err(CollectError::Protocol("deterministic test failure"))
        });
        assert!(matches!(protocol, Err(CollectError::Protocol(_))));
        assert_eq!(protocol_attempts, 1);
    }
}
