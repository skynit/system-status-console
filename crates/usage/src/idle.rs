use std::collections::VecDeque;
use std::io;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use thiserror::Error;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1, ext_idle_notifier_v1,
};

pub const INPUT_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IdleEvent {
    Idled,
    Resumed,
}

#[derive(Debug)]
struct IdleState {
    idle: bool,
    pending: VecDeque<IdleEvent>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for IdleState {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(IdleState: ignore wl_seat::WlSeat);
delegate_noop!(IdleState: ext_idle_notifier_v1::ExtIdleNotifierV1);

impl Dispatch<ext_idle_notification_v1::ExtIdleNotificationV1, ()> for IdleState {
    fn event(
        state: &mut Self,
        _: &ext_idle_notification_v1::ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let next = match event {
            ext_idle_notification_v1::Event::Idled => (true, IdleEvent::Idled),
            ext_idle_notification_v1::Event::Resumed => (false, IdleEvent::Resumed),
            _ => return,
        };
        if state.idle != next.0 {
            state.idle = next.0;
            state.pending.push_back(next.1);
        }
    }
}

#[derive(Debug)]
pub struct WaylandIdleEventStream {
    queue: EventQueue<IdleState>,
    state: IdleState,
    _seat: wl_seat::WlSeat,
    _notifier: ext_idle_notifier_v1::ExtIdleNotifierV1,
    _notification: ext_idle_notification_v1::ExtIdleNotificationV1,
}

impl WaylandIdleEventStream {
    pub fn connect() -> Result<Self, WaylandIdleError> {
        Self::connect_with_timeout(INPUT_IDLE_TIMEOUT)
    }

    fn connect_with_timeout(timeout: Duration) -> Result<Self, WaylandIdleError> {
        let timeout_ms =
            u32::try_from(timeout.as_millis()).map_err(|_| WaylandIdleError::InvalidTimeout)?;
        let connection = Connection::connect_to_env().map_err(|_| WaylandIdleError::Connect)?;
        let (globals, mut queue) = registry_queue_init::<IdleState>(&connection)
            .map_err(|_| WaylandIdleError::Registry)?;
        let handle = queue.handle();
        let seat = globals
            .bind::<wl_seat::WlSeat, _, _>(&handle, 1..=9, ())
            .map_err(|_| WaylandIdleError::SeatUnavailable)?;
        let notifier = globals
            .bind::<ext_idle_notifier_v1::ExtIdleNotifierV1, _, _>(&handle, 2..=2, ())
            .map_err(|_| WaylandIdleError::ProtocolUnavailable)?;
        let notification = notifier.get_input_idle_notification(timeout_ms, &seat, &handle, ());
        let mut state = IdleState {
            idle: false,
            pending: VecDeque::new(),
        };
        queue
            .roundtrip(&mut state)
            .map_err(|_| WaylandIdleError::Disconnected)?;
        Ok(Self {
            queue,
            state,
            _seat: seat,
            _notifier: notifier,
            _notification: notification,
        })
    }

    pub fn is_idle(&self) -> bool {
        self.state.idle
    }

    pub fn poll_changed(&mut self, timeout: Duration) -> Result<Option<bool>, WaylandIdleError> {
        self.queue
            .dispatch_pending(&mut self.state)
            .map_err(|_| WaylandIdleError::Disconnected)?;
        if let Some(event) = self.state.pending.pop_front() {
            return Ok(Some(matches!(event, IdleEvent::Idled)));
        }
        self.queue
            .flush()
            .map_err(|_| WaylandIdleError::Disconnected)?;

        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(Instant::now);
        loop {
            let Some(guard) = self.queue.prepare_read() else {
                self.queue
                    .dispatch_pending(&mut self.state)
                    .map_err(|_| WaylandIdleError::Disconnected)?;
                if let Some(event) = self.state.pending.pop_front() {
                    return Ok(Some(matches!(event, IdleEvent::Idled)));
                }
                continue;
            };
            if !poll_readable_until(guard.connection_fd().as_raw_fd(), deadline)? {
                drop(guard);
                return Ok(None);
            }
            guard.read().map_err(|_| WaylandIdleError::Disconnected)?;
            self.queue
                .dispatch_pending(&mut self.state)
                .map_err(|_| WaylandIdleError::Disconnected)?;
            if let Some(event) = self.state.pending.pop_front() {
                return Ok(Some(matches!(event, IdleEvent::Idled)));
            }
        }
    }
}

fn poll_readable_until(fd: i32, deadline: Instant) -> io::Result<bool> {
    let mut descriptor = libc::pollfd {
        fd,
        events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
        revents: 0,
    };
    loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Ok(false);
        };
        let timeout_ms = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
        // SAFETY: descriptor is a valid one-element pollfd array for this call.
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if result >= 0 {
            return Ok(result > 0);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[derive(Debug, Error)]
pub enum WaylandIdleError {
    #[error("Wayland connection is unavailable")]
    Connect,
    #[error("Wayland global registry is unavailable")]
    Registry,
    #[error("Wayland wl_seat is unavailable")]
    SeatUnavailable,
    #[error("Wayland ext-idle-notify-v1 version 2 is unavailable")]
    ProtocolUnavailable,
    #[error("idle timeout cannot be represented in protocol milliseconds")]
    InvalidTimeout,
    #[error("Wayland idle event stream disconnected")]
    Disconnected,
    #[error("failed to poll Wayland idle event stream: {0}")]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a live ext-idle-notify-v1 v2 Wayland session"]
    fn live_compositor_emits_idled_after_zero_timeout() {
        let mut stream = WaylandIdleEventStream::connect_with_timeout(Duration::ZERO)
            .expect("connect to compositor idle protocol");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .expect("compositor did not emit idled");
            if stream
                .poll_changed(remaining.min(Duration::from_millis(250)))
                .expect("poll compositor idle protocol")
                == Some(true)
            {
                break;
            }
        }
    }
}
