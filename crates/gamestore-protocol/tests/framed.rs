//! Integration test for the tokio `Framed` adapter.
//!
//! Drives [`CommandCodec`] over an in-memory duplex pipe to prove the async
//! framing path decodes real requests and encodes replies end to end, including
//! a RESP3 (`HELLO 3`) style version switch on the encode side.

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use gamestore_protocol::{CommandCodec, Frame, RespVersion};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::codec::Framed;

fn run<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

#[test]
fn framed_decodes_command_and_encodes_reply() {
    run(async {
        let (mut client, server) = tokio::io::duplex(1024);
        let mut framed = Framed::new(server, CommandCodec::new());

        // Client sends a multibulk PING split across two writes (fragmentation).
        client.write_all(b"*1\r\n$4\r\n").await.unwrap();
        client.write_all(b"PING\r\n").await.unwrap();

        let cmd = framed.next().await.unwrap().unwrap();
        assert_eq!(cmd, vec![Bytes::from_static(b"PING")]);

        // Server replies +PONG.
        framed.send(Frame::simple("PONG")).await.unwrap();

        let mut buf = [0u8; 16];
        let n = client.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"+PONG\r\n");
    });
}

#[test]
fn framed_resp3_null_after_version_switch() {
    run(async {
        let (mut client, server) = tokio::io::duplex(1024);
        let mut framed = Framed::new(server, CommandCodec::new());

        // Inline HELLO 3.
        client.write_all(b"HELLO 3\r\n").await.unwrap();

        let cmd = framed.next().await.unwrap().unwrap();
        assert_eq!(
            cmd,
            vec![Bytes::from_static(b"HELLO"), Bytes::from_static(b"3")]
        );

        // Emulate the server upgrading the connection to RESP3.
        framed.codec_mut().set_version(RespVersion::V3);
        assert_eq!(framed.codec().version(), RespVersion::V3);

        framed.send(Frame::Null).await.unwrap();

        let mut buf = [0u8; 16];
        let n = client.read(&mut buf).await.unwrap();
        // RESP3 null is `_\r\n`, not the RESP2 `$-1\r\n`.
        assert_eq!(&buf[..n], b"_\r\n");
    });
}
