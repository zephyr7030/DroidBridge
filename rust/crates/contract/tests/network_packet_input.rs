//! R-NET-006: every public `network.packet` decode source reaches the typed input.

use contract::{NetworkPacketInput, PacketDecodeSource};
use serde_json::json;

fn parse(value: serde_json::Value) -> Result<NetworkPacketInput, serde_json::Error> {
    serde_json::from_value(value)
}

#[test]
fn every_decode_source_deserializes_to_its_typed_source() {
    let reference = "dbref:packet:30fa108d-cca4-4b49-abcc-bac3210b806a";
    assert_eq!(
        parse(json!({"operation": "decode", "packet_ref": reference})).unwrap(),
        NetworkPacketInput::Decode {
            source: PacketDecodeSource::PacketRef {
                packet_ref: reference.to_owned(),
            },
        }
    );
    assert_eq!(
        parse(json!({"operation": "decode", "raw_base64": "AAAA"})).unwrap(),
        NetworkPacketInput::Decode {
            source: PacketDecodeSource::Raw {
                raw_base64: "AAAA".to_owned(),
            },
        }
    );
    assert_eq!(
        parse(json!({"operation": "decode", "capture_ref": "dbref:capture:x", "index": 3}))
            .unwrap(),
        NetworkPacketInput::Decode {
            source: PacketDecodeSource::Capture {
                capture_ref: "dbref:capture:x".to_owned(),
                index: 3,
            },
        }
    );
}

#[test]
fn decode_refuses_mixed_partial_or_unknown_sources() {
    for value in [
        json!({"operation": "decode"}),
        json!({"operation": "decode", "raw_base64": "AAAA", "packet_ref": "dbref:packet:x"}),
        json!({"operation": "decode", "capture_ref": "dbref:capture:x"}),
        json!({"operation": "decode", "index": 0}),
        json!({"operation": "decode", "packet_ref": "dbref:packet:x", "index": 0}),
        json!({"operation": "decode", "packet_ref": "dbref:packet:x", "extra": true}),
    ] {
        assert!(parse(value.clone()).is_err(), "{value} must be refused");
    }
}

#[test]
fn a_decoded_input_serializes_back_to_the_same_public_shape() {
    let value = json!({"operation": "decode", "packet_ref": "dbref:packet:x"});
    assert_eq!(
        serde_json::to_value(parse(value.clone()).unwrap()).unwrap(),
        value
    );
}
