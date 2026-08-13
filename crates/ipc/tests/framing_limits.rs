use localdesk_ipc::{
    FrameError, MAX_FRAME_PAYLOAD_BYTES, MAX_RESPONSE_WIRE_BYTES, WireBudget, read_frame,
    read_frame_with_idle_timeout, read_json, write_frame,
};
use std::time::Duration;
use tokio::{
    io::{AsyncWriteExt, duplex},
    time::Instant,
};

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(1)
}

#[tokio::test]
async fn partial_frame_is_reassembled() {
    let (mut writer, mut reader) = duplex(256);
    let writer_task = tokio::spawn(async move {
        writer.write_all(&[0, 0]).await.expect("partial header");
        tokio::task::yield_now().await;
        writer.write_all(&[0, 5]).await.expect("remaining header");
        writer.write_all(b"hello").await.expect("payload");
    });
    let mut budget = WireBudget::new(1, 9);

    let frame = read_frame(&mut reader, deadline(), &mut budget)
        .await
        .expect("read frame");
    assert_eq!(&frame[..], b"hello");
    assert_eq!(budget.observed_frames(), 1);
    assert_eq!(budget.observed_wire_bytes(), 9);
    writer_task.await.expect("writer task");
}

#[tokio::test]
async fn exact_frame_limit_is_accepted_and_oversize_header_is_rejected_before_payload() {
    let (mut writer, mut reader) = duplex(MAX_FRAME_PAYLOAD_BYTES + 4);
    let payload = vec![b'x'; MAX_FRAME_PAYLOAD_BYTES];
    let writer_task = tokio::spawn(async move {
        writer
            .write_all(&(MAX_FRAME_PAYLOAD_BYTES as u32).to_be_bytes())
            .await
            .expect("header");
        writer.write_all(&payload).await.expect("payload");
    });
    let mut budget = WireBudget::new(1, MAX_FRAME_PAYLOAD_BYTES + 4);
    assert_eq!(
        read_frame(&mut reader, deadline(), &mut budget)
            .await
            .expect("exact frame")
            .len(),
        MAX_FRAME_PAYLOAD_BYTES
    );
    writer_task.await.expect("writer");

    let (mut writer, mut reader) = duplex(8);
    writer
        .write_all(&((MAX_FRAME_PAYLOAD_BYTES as u32 + 1).to_be_bytes()))
        .await
        .expect("oversize header");
    let mut budget = WireBudget::new(1, MAX_FRAME_PAYLOAD_BYTES + 5);
    assert!(matches!(
        read_frame(&mut reader, deadline(), &mut budget).await,
        Err(FrameError::Oversize)
    ));
}

#[tokio::test]
async fn empty_truncated_and_invalid_json_frames_are_rejected() {
    let (mut writer, mut reader) = duplex(16);
    writer.write_all(&[0, 0, 0, 0]).await.expect("empty header");
    let mut budget = WireBudget::new(1, 4);
    assert!(matches!(
        read_frame(&mut reader, deadline(), &mut budget).await,
        Err(FrameError::Empty)
    ));

    let (mut writer, mut reader) = duplex(16);
    writer
        .write_all(&[0, 0, 0, 3, b'a'])
        .await
        .expect("truncated frame");
    writer.shutdown().await.expect("close writer");
    let mut budget = WireBudget::new(1, 7);
    assert!(matches!(
        read_frame(&mut reader, deadline(), &mut budget).await,
        Err(FrameError::UnexpectedEof)
    ));

    let (mut writer, mut reader) = duplex(16);
    writer
        .write_all(&[0, 0, 0, 1, b'{'])
        .await
        .expect("invalid json");
    let mut budget = WireBudget::new(1, 5);
    let result: Result<serde_json::Value, FrameError> =
        read_json(&mut reader, deadline(), &mut budget).await;
    assert!(matches!(result, Err(FrameError::InvalidJson(_))));
}

#[test]
fn prefix_bytes_and_frame_count_are_checked_at_exact_boundaries() {
    let mut budget = WireBudget::new(2, 11);
    budget.observe_payload_len(1).expect("5 wire bytes");
    budget.observe_payload_len(2).expect("11 wire bytes");
    assert_eq!(budget.observed_frames(), 2);
    assert_eq!(budget.observed_wire_bytes(), 11);
    assert!(matches!(
        budget.observe_payload_len(1),
        Err(FrameError::FrameLimitExceeded)
    ));

    let mut budget = WireBudget::new(2, 10);
    budget.observe_payload_len(1).expect("first frame");
    assert!(matches!(
        budget.observe_payload_len(2),
        Err(FrameError::WireBytesExceeded)
    ));

    let mut budget = WireBudget::new(130, usize::MAX);
    for _ in 0..130 {
        budget.observe_payload_len(1).expect("frame within cap");
    }
    assert!(matches!(
        budget.observe_payload_len(1),
        Err(FrameError::FrameLimitExceeded)
    ));
}

#[test]
fn cumulative_response_wire_bytes_are_checked_at_exact_boundary() {
    let mut budget = WireBudget::new(usize::MAX, MAX_RESPONSE_WIRE_BYTES);
    for _ in 0..143 {
        budget
            .observe_payload_len(MAX_FRAME_PAYLOAD_BYTES)
            .expect("full frame within wire budget");
    }
    budget
        .observe_payload_len(64_960)
        .expect("exact response wire budget");
    assert_eq!(budget.observed_wire_bytes(), MAX_RESPONSE_WIRE_BYTES);
    assert!(matches!(
        budget.observe_payload_len(1),
        Err(FrameError::WireBytesExceeded)
    ));
}

#[tokio::test]
async fn idle_and_absolute_deadlines_are_independently_bounded() {
    let (_writer, mut reader) = duplex(16);
    let mut budget = WireBudget::new(1, 16);
    let result = read_frame_with_idle_timeout(
        &mut reader,
        Instant::now() + Duration::from_secs(1),
        Duration::from_millis(20),
        &mut budget,
    )
    .await;
    assert!(matches!(result, Err(FrameError::IdleTimeout)));

    let (_writer, mut reader) = duplex(16);
    let mut budget = WireBudget::new(1, 16);
    let result = read_frame_with_idle_timeout(
        &mut reader,
        Instant::now() + Duration::from_millis(20),
        Duration::from_secs(1),
        &mut budget,
    )
    .await;
    assert!(matches!(result, Err(FrameError::DeadlineExceeded)));
}

#[tokio::test]
async fn writer_rejects_empty_and_oversize_payloads() {
    let (mut writer, _reader) = duplex(16);
    assert!(matches!(
        write_frame(&mut writer, &[], deadline()).await,
        Err(FrameError::Empty)
    ));
    assert!(matches!(
        write_frame(
            &mut writer,
            &vec![0_u8; MAX_FRAME_PAYLOAD_BYTES + 1],
            deadline(),
        )
        .await,
        Err(FrameError::Oversize)
    ));
}
