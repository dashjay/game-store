//! Property-based round-trip and robustness tests for the sans-IO codec.
//!
//! These cover the I-02 exit criteria "fuzz / boundary cases pass":
//! - encode → decode is the identity for every frame in each protocol version;
//! - the same holds when the encoded bytes are delivered one byte at a time
//!   (fragmented reads);
//! - client requests survive a multibulk encode → `decode_command` round-trip;
//! - feeding arbitrary bytes never panics (only `Ok`/`Err`).

use bytes::{Bytes, BytesMut};
use gamestore_protocol::{decode, decode_command, encode, Frame, Limits, RespVersion};
use proptest::prelude::*;

/// Strings safe to carry in a `+`/`-`/`(` line: no CR/LF, valid UTF-8.
fn line_text() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 ]{0,24}"
}

/// Arbitrary binary payload for bulk strings.
fn blob() -> impl Strategy<Value = Bytes> {
    proptest::collection::vec(any::<u8>(), 0..64).prop_map(Bytes::from)
}

/// RESP2 frames (the subset a RESP2 connection can carry).
fn resp2_frame() -> impl Strategy<Value = Frame> {
    let leaf = prop_oneof![
        line_text().prop_map(Frame::Simple),
        line_text().prop_map(Frame::Error),
        any::<i64>().prop_map(Frame::Integer),
        blob().prop_map(Frame::Bulk),
        Just(Frame::Null),
    ];
    leaf.prop_recursive(4, 32, 8, |inner| {
        proptest::collection::vec(inner, 0..6).prop_map(Frame::Array)
    })
}

/// RESP3 frames (adds the typed replies).
fn resp3_frame() -> impl Strategy<Value = Frame> {
    let leaf = prop_oneof![
        line_text().prop_map(Frame::Simple),
        line_text().prop_map(Frame::Error),
        any::<i64>().prop_map(Frame::Integer),
        blob().prop_map(Frame::Bulk),
        Just(Frame::Null),
        any::<bool>().prop_map(Frame::Boolean),
        any::<f64>()
            .prop_filter("finite", |x| x.is_finite())
            .prop_map(Frame::Double),
        "-?[0-9]{1,40}".prop_map(Frame::BigNumber),
        line_text().prop_map(Frame::BulkError),
        ("[a-z]{3}", blob()).prop_map(|(f, data)| {
            let fb = f.as_bytes();
            Frame::Verbatim {
                format: [fb[0], fb[1], fb[2]],
                data,
            }
        }),
    ];
    leaf.prop_recursive(4, 48, 8, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..6).prop_map(Frame::Array),
            proptest::collection::vec(inner.clone(), 0..6).prop_map(Frame::Set),
            proptest::collection::vec(inner.clone(), 0..6).prop_map(Frame::Push),
            proptest::collection::vec((inner.clone(), inner), 0..5).prop_map(Frame::Map),
        ]
    })
}

fn assert_roundtrip(frame: &Frame, version: RespVersion) {
    let mut buf = BytesMut::new();
    encode(frame, version, &mut buf);
    let decoded = decode(&mut buf, &Limits::default())
        .expect("decode ok")
        .expect("frame present");
    assert_eq!(&decoded, frame, "round-trip mismatch for {frame:?}");
    assert!(buf.is_empty(), "decoder left trailing bytes for {frame:?}");
}

fn assert_fragmented_roundtrip(frame: &Frame, version: RespVersion) {
    let mut encoded = BytesMut::new();
    encode(frame, version, &mut encoded);
    let bytes = encoded.to_vec();
    let limits = Limits::default();

    let mut buf = BytesMut::new();
    for (i, b) in bytes.iter().enumerate() {
        buf.extend_from_slice(&[*b]);
        let last = i + 1 == bytes.len();
        match decode(&mut buf, &limits).expect("decode ok") {
            None => assert!(!last, "still None after all bytes for {frame:?}"),
            Some(got) => {
                assert!(last, "decoded early for {frame:?}");
                assert_eq!(&got, frame);
            }
        }
    }
}

proptest! {
    #[test]
    fn resp2_roundtrip(frame in resp2_frame()) {
        assert_roundtrip(&frame, RespVersion::V2);
    }

    #[test]
    fn resp3_roundtrip(frame in resp3_frame()) {
        assert_roundtrip(&frame, RespVersion::V3);
    }

    #[test]
    fn resp2_fragmented_roundtrip(frame in resp2_frame()) {
        assert_fragmented_roundtrip(&frame, RespVersion::V2);
    }

    #[test]
    fn resp3_fragmented_roundtrip(frame in resp3_frame()) {
        assert_fragmented_roundtrip(&frame, RespVersion::V3);
    }

    #[test]
    fn multibulk_command_roundtrip(args in proptest::collection::vec(blob(), 0..8)) {
        // Build the RESP request the way a client would.
        let mut buf = BytesMut::new();
        buf.extend_from_slice(format!("*{}\r\n", args.len()).as_bytes());
        for a in &args {
            buf.extend_from_slice(format!("${}\r\n", a.len()).as_bytes());
            buf.extend_from_slice(a);
            buf.extend_from_slice(b"\r\n");
        }
        let decoded = decode_command(&mut buf, &Limits::default())
            .expect("decode ok")
            .expect("command present");
        prop_assert_eq!(decoded, args);
        prop_assert!(buf.is_empty());
    }

    #[test]
    fn arbitrary_bytes_never_panic(data in proptest::collection::vec(any::<u8>(), 0..256)) {
        let mut buf = BytesMut::from(&data[..]);
        // Must return Ok(_) or Err(_) — never panic / hang.
        let _ = decode(&mut buf, &Limits::default());
        let mut buf2 = BytesMut::from(&data[..]);
        let _ = decode_command(&mut buf2, &Limits::default());
    }
}
