use std::io;

use super::frame::{Frame, MAX_PAYLOAD, read_frame, write_frame};

async fn round(frame: &Frame) -> Frame {
    let mut wire = Vec::new();
    write_frame(&mut wire, frame).await.unwrap();
    read_frame(&mut wire.as_slice()).await.unwrap()
}

#[tokio::test]
async fn maintenance_frames_round_trip() {
    for frame in [
        Frame::Hello(b"abc".to_vec()),
        Frame::HelloOk(b"def".to_vec()),
        Frame::Output(b"out\x1b[0m".to_vec()),
        Frame::SnapshotEnd,
        Frame::Input(b"in\n".to_vec()),
        Frame::Resize {
            cols: 100,
            rows: 30,
        },
        Frame::Exit(-1),
        Frame::Exit(1),
    ] {
        assert_eq!(round(&frame).await, frame);
    }
}

#[tokio::test]
async fn maintenance_frame_wire_shape_is_length_type_payload() {
    let mut wire = Vec::new();
    write_frame(&mut wire, &Frame::Exit(258)).await.unwrap();
    assert_eq!(wire, [0, 0, 0, 5, 7, 0, 0, 1, 2]);
    let mut wire = Vec::new();
    write_frame(&mut wire, &Frame::Resize { cols: 1, rows: 2 })
        .await
        .unwrap();
    assert_eq!(wire, [0, 0, 0, 5, 6, 0, 1, 0, 2]);
}

#[tokio::test]
async fn maintenance_frame_with_a_length_of_zero_is_refused() {
    let err = read_frame(&mut [0_u8, 0, 0, 0].as_slice())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[tokio::test]
async fn maintenance_frame_over_the_limit_is_refused() {
    let mut wire = u32::try_from(MAX_PAYLOAD + 2)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    wire.extend(vec![3_u8; MAX_PAYLOAD + 2]);
    let err = read_frame(&mut wire.as_slice()).await.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[tokio::test]
async fn maintenance_frame_at_the_limit_is_read() {
    let mut wire = u32::try_from(MAX_PAYLOAD + 1)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    wire.push(3);
    wire.extend(vec![9_u8; MAX_PAYLOAD]);
    let Frame::Output(bytes) = read_frame(&mut wire.as_slice()).await.unwrap() else {
        panic!("not an output frame");
    };
    assert_eq!(bytes.len(), MAX_PAYLOAD);
}

#[tokio::test]
async fn maintenance_frame_of_an_unknown_type_is_refused() {
    for kind in [0_u8, 8, 255] {
        let err = read_frame(&mut [0_u8, 0, 0, 1, kind].as_slice())
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}

#[tokio::test]
async fn maintenance_frame_with_a_bad_payload_size_is_refused() {
    // SnapshotEnd with a payload, Resize with 3 bytes, Exit with 5 bytes.
    for wire in [
        vec![0_u8, 0, 0, 2, 4, 0],
        vec![0, 0, 0, 4, 6, 0, 1, 0],
        vec![0, 0, 0, 6, 7, 0, 0, 0, 0, 1],
    ] {
        let err = read_frame(&mut wire.as_slice()).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}

#[tokio::test]
async fn maintenance_frame_that_is_cut_short_is_an_error() {
    let err = read_frame(&mut [0_u8, 0, 0, 9, 3, 1].as_slice())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

#[tokio::test]
async fn maintenance_output_over_64_kib_is_split_by_the_writer() {
    let bytes: Vec<u8> = (0..MAX_PAYLOAD * 2 + 5).map(|i| (i % 251) as u8).collect();
    let mut wire = Vec::new();
    write_frame(&mut wire, &Frame::Output(bytes.clone()))
        .await
        .unwrap();
    let mut reader = wire.as_slice();
    let mut got = Vec::new();
    let mut frames = 0;
    while !reader.is_empty() {
        let Frame::Output(part) = read_frame(&mut reader).await.unwrap() else {
            panic!("not an output frame");
        };
        assert!(part.len() <= MAX_PAYLOAD);
        got.extend(part);
        frames += 1;
    }
    assert_eq!(frames, 3);
    assert_eq!(got, bytes);
}

#[tokio::test]
async fn maintenance_input_over_64_kib_is_split_by_the_writer() {
    let mut wire = Vec::new();
    write_frame(&mut wire, &Frame::Input(vec![1_u8; MAX_PAYLOAD + 1]))
        .await
        .unwrap();
    let mut reader = wire.as_slice();
    assert!(matches!(
        read_frame(&mut reader).await.unwrap(),
        Frame::Input(p) if p.len() == MAX_PAYLOAD
    ));
    assert!(matches!(
        read_frame(&mut reader).await.unwrap(),
        Frame::Input(p) if p.len() == 1
    ));
}

#[tokio::test]
async fn maintenance_hello_over_64_kib_is_not_written() {
    let mut wire = Vec::new();
    let err = write_frame(&mut wire, &Frame::Hello(vec![1_u8; MAX_PAYLOAD + 1]))
        .await
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert!(wire.is_empty());
}
