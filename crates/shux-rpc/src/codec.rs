//! Framing codec for the JSON-RPC transport.
//!
//! Uses length-prefixed framing: 4-byte big-endian length + JSON payload.
//! Same codec is shared between the UDS/TCP JSON-RPC server and the
//! process plugin protocol (PRD §8.1).

use tokio_util::codec::LengthDelimitedCodec;

/// Maximum frame size: 16 MB (PRD §8.1).
///
/// Frames exceeding this limit are rejected immediately with error code
/// -32001 (frame_too_large) and the connection is closed.
pub const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024; // 16 MB

/// Length prefix size: 4 bytes big-endian.
pub const LENGTH_PREFIX_SIZE: usize = 4;

/// Create the framing codec for JSON-RPC transport.
///
/// Configuration:
/// - 4-byte big-endian length prefix
/// - Maximum frame length: 16 MB
/// - Length field does NOT include itself (just the payload length)
pub fn create_codec() -> LengthDelimitedCodec {
    LengthDelimitedCodec::builder()
        .big_endian()
        .length_field_length(LENGTH_PREFIX_SIZE)
        .max_frame_length(MAX_FRAME_SIZE)
        .new_codec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::{BufMut, BytesMut};
    use tokio_util::codec::{Decoder, Encoder};

    #[test]
    fn test_codec_roundtrip() {
        let mut codec = create_codec();
        let payload = b"hello world";

        // Encode.
        let mut buf = BytesMut::new();
        codec
            .encode(bytes::Bytes::from_static(payload), &mut buf)
            .unwrap();

        // Should have 4-byte length prefix + payload.
        assert_eq!(buf.len(), LENGTH_PREFIX_SIZE + payload.len());

        // Decode.
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(&decoded[..], payload);
    }

    #[test]
    fn test_codec_max_frame_size() {
        let mut codec = create_codec();
        let mut buf = BytesMut::new();

        // Write a length prefix indicating a frame larger than MAX_FRAME_SIZE.
        let oversized_len = (MAX_FRAME_SIZE + 1) as u32;
        buf.put_u32(oversized_len);
        // Add some dummy data.
        buf.extend_from_slice(&[0u8; 100]);

        // Decoding should fail.
        let result = codec.decode(&mut buf);
        assert!(result.is_err(), "expected error for oversized frame");
    }

    #[test]
    fn test_codec_empty_frame() {
        let mut codec = create_codec();
        let payload = b"";

        let mut buf = BytesMut::new();
        codec
            .encode(bytes::Bytes::from_static(payload), &mut buf)
            .unwrap();

        // 4-byte prefix + 0 bytes payload.
        assert_eq!(buf.len(), LENGTH_PREFIX_SIZE);

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn test_codec_json_payload() {
        let mut codec = create_codec();
        let json = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "system.version"
        });
        let payload = serde_json::to_vec(&json).unwrap();

        let mut buf = BytesMut::new();
        codec
            .encode(bytes::Bytes::from(payload.clone()), &mut buf)
            .unwrap();

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(parsed["method"], "system.version");
    }
}
