/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

//! Byte-level encoding for the imscope NNG wire protocol.
//!
//! This module is the producer-side mirror of `src/consumer.rs`'s parsing
//! functions (`parse_announce_response`, `parse_scope_message`,
//! `parse_setting_response`) and of the C structs in
//! `common/include/imscope_common.h`. Every offset/size below is chosen to
//! match those two ground truths byte-for-byte; see the tests at the bottom
//! of this file for round-trip checks pinned to the exact layouts.
//!
//! All integers are encoded in native endianness (`to_ne_bytes`), matching
//! the existing C++ producer and Rust consumer, which both assume producer
//! and consumer run on machines of the same endianness (in practice: x86_64
//! little-endian on both ends).

use std::convert::TryInto;

pub const ANNOUNCE_MSG_ID: u32 = 0xABCDEF01;
pub const SCOPE_REQ_MSG_ID: u32 = 0xABCDEF02;

pub const SETTING_REQ_GET_ALL: u32 = 0xABCDEF10;
pub const SETTING_REQ_SET: u32 = 0xABCDEF11;
pub const SETTING_REP_GET_ALL: u32 = 0xABCDEF20;
pub const SETTING_REP_SET: u32 = 0xABCDEF21;

const MAX_SCOPE_NAME_LEN: usize = 64;
const MAX_GROUP_NAME_LEN: usize = 64;
const ANNOUNCE_ADDR_FIELD_LEN: usize = 128;
const ANNOUNCE_NAME_FIELD_LEN: usize = 128;
const ANNOUNCE_SCOPE_ENTRY_LEN: usize = MAX_SCOPE_NAME_LEN + MAX_GROUP_NAME_LEN + 4 + 4; // 136
const ANNOUNCE_HEADER_LEN: usize = ANNOUNCE_ADDR_FIELD_LEN * 2 + ANNOUNCE_NAME_FIELD_LEN + 4; // 388

/// Fixed 48-byte header in front of every scope data reply: `frame`(4),
/// `slot`(4), `timestamp`(8), `time_taken_in_ns`(8), `id`(4), then 4 bytes of
/// C struct padding, `data_size`(8), and finally 8 bytes of trailing padding
/// introduced because `scope_msg_t` rounds its `sizeof()` up to an 8-byte
/// multiple. Mirrors `scope_msg_t` in imscope_common.h: the C++ producer
/// writes the sample payload at `(scope_msg_t*)msg + 1`, i.e. right after the
/// full padded struct size (48), not right after the nominal `data` field
/// (offset 40) -- see `src/consumer.rs::parse_scope_message` for the
/// consumer side of this same quirk.
const SCOPE_MSG_HEADER_LEN: usize = 48;

/// Scope sample kind, mirrors `scope_type_t` / `src/consumer.rs::ScopeType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ScopeType {
    Real = 0,
    IqData = 1,
    Int32 = 2,
    Float = 3,
}

/// Whether a scope's samples are raw time-domain data or already
/// frequency-domain. Mirrors `scope_domain_t` / `src/consumer.rs::ScopeDomain`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(i32)]
pub enum ScopeDomain {
    #[default]
    Time = 0,
    Frequency = 1,
}

/// Frame/slot/timestamp metadata attached to every scope sample, mirrors
/// `NRmetadata` in imscope_common.h.
#[derive(Debug, Clone, Copy, Default)]
pub struct NRmetadata {
    pub frame: u32,
    pub slot: u32,
    pub timestamp: u64,
}

/// One announced scope's static description, mirrors `imscope_scope_config_t`.
#[derive(Debug, Clone)]
pub struct ScopeConfigWire {
    pub name: String,
    pub group: String,
    pub scope_type: ScopeType,
    pub domain: ScopeDomain,
}

fn write_c_str(dst: &mut [u8], s: &str) {
    let bytes = s.as_bytes();
    let n = bytes.len().min(dst.len() - 1);
    dst[..n].copy_from_slice(&bytes[..n]);
    // Remaining bytes (including at least one NUL terminator) stay zero,
    // since callers always start from a zero-filled buffer.
}

/// Encodes the announce handshake reply (`announce_response_t`).
pub fn encode_announce_response(
    name: &str,
    data_address: &str,
    control_address: &str,
    scopes: &[ScopeConfigWire],
) -> Vec<u8> {
    let mut bytes = vec![0u8; ANNOUNCE_HEADER_LEN + scopes.len() * ANNOUNCE_SCOPE_ENTRY_LEN];

    write_c_str(&mut bytes[0..128], data_address);
    write_c_str(&mut bytes[128..256], control_address);
    write_c_str(&mut bytes[256..384], name);
    bytes[384..388].copy_from_slice(&(scopes.len() as i32).to_ne_bytes());

    for (i, scope) in scopes.iter().enumerate() {
        let off = ANNOUNCE_HEADER_LEN + i * ANNOUNCE_SCOPE_ENTRY_LEN;
        write_c_str(&mut bytes[off..off + 64], &scope.name);
        write_c_str(&mut bytes[off + 64..off + 128], &scope.group);
        bytes[off + 128..off + 132].copy_from_slice(&(scope.scope_type as i32).to_ne_bytes());
        bytes[off + 132..off + 136].copy_from_slice(&(scope.domain as i32).to_ne_bytes());
    }

    bytes
}

/// Validates an incoming announce request (`announce_request_t`: a single
/// `uint32_t magic`). Returns `true` if the magic matches.
pub fn is_valid_announce_request(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && u32::from_ne_bytes(bytes[0..4].try_into().unwrap()) == ANNOUNCE_MSG_ID
}

/// Parses an incoming scope data request (`scope_request_t`: `uint32_t
/// magic; int32_t scope_id;`). Returns the requested scope id if the magic
/// matches.
pub fn parse_scope_request(bytes: &[u8]) -> Option<i32> {
    if bytes.len() < 8 {
        return None;
    }
    let magic = u32::from_ne_bytes(bytes[0..4].try_into().unwrap());
    if magic != SCOPE_REQ_MSG_ID {
        return None;
    }
    Some(i32::from_ne_bytes(bytes[4..8].try_into().unwrap()))
}

/// Encodes a scope data reply (`scope_msg_t` + sample payload).
pub fn encode_scope_message(
    scope_id: i32,
    meta: NRmetadata,
    time_taken_in_ns: u64,
    payload: &[u8],
) -> Vec<u8> {
    let mut bytes = vec![0u8; SCOPE_MSG_HEADER_LEN + payload.len()];
    bytes[0..4].copy_from_slice(&meta.frame.to_ne_bytes());
    bytes[4..8].copy_from_slice(&meta.slot.to_ne_bytes());
    bytes[8..16].copy_from_slice(&meta.timestamp.to_ne_bytes());
    bytes[16..24].copy_from_slice(&time_taken_in_ns.to_ne_bytes());
    bytes[24..28].copy_from_slice(&scope_id.to_ne_bytes());
    // bytes[28..40] left zero: 4 bytes of C struct padding before data_size,
    // then data_size itself.
    bytes[32..40].copy_from_slice(&(payload.len() as u64).to_ne_bytes());
    // bytes[40..48] left zero: trailing struct padding (see
    // SCOPE_MSG_HEADER_LEN doc comment above).
    bytes[SCOPE_MSG_HEADER_LEN..].copy_from_slice(payload);
    bytes
}

/// Packs real-only samples (`SCOPE_TYPE_REAL`): one `i16` per sample.
pub fn encode_real(samples: &[i16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        bytes.extend_from_slice(&s.to_ne_bytes());
    }
    bytes
}

/// Packs interleaved IQ samples (`SCOPE_TYPE_IQ_DATA`): `i16` real then
/// `i16` imaginary per sample, matching
/// `src/consumer.rs::parse_scope_message`'s `ScopeType::IqData` arm (real
/// read from the first 2 bytes of each 4-byte group, imag from the next 2).
///
/// # Panics
/// Panics if `real.len() != imag.len()`.
pub fn encode_iq(real: &[i16], imag: &[i16]) -> Vec<u8> {
    assert_eq!(real.len(), imag.len(), "real/imag length mismatch");
    let mut bytes = Vec::with_capacity(real.len() * 4);
    for (&r, &i) in real.iter().zip(imag.iter()) {
        bytes.extend_from_slice(&r.to_ne_bytes());
        bytes.extend_from_slice(&i.to_ne_bytes());
    }
    bytes
}

/// Packs `SCOPE_TYPE_INT32` samples: one `i32` per sample.
pub fn encode_int32(samples: &[i32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 4);
    for &s in samples {
        bytes.extend_from_slice(&s.to_ne_bytes());
    }
    bytes
}

/// Packs `SCOPE_TYPE_FLOAT` samples: one `f32` per sample.
pub fn encode_float(samples: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 4);
    for &s in samples {
        bytes.extend_from_slice(&s.to_ne_bytes());
    }
    bytes
}

/// Setting value kind, mirrors `setting_type_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum SettingType {
    Bool = 0,
    Int32 = 1,
    Float = 2,
}

/// A registered setting's current value.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingValue {
    Bool(bool),
    Int32(i32),
    Float(f32),
}

#[derive(Debug, Clone)]
pub struct SettingWire {
    pub name: String,
    pub setting_type: SettingType,
    pub value: SettingValue,
}

/// Incoming setting request, mirrors `setting_request_t`. `SETTING_REQ_SET`
/// carries `name`/`setting_type`/`value`; `SETTING_REQ_GET_ALL` only uses
/// `magic`.
#[derive(Debug, Clone)]
pub enum SettingRequest {
    GetAll,
    Set {
        name: String,
        setting_type_raw: i32,
        raw_value: [u8; 4],
    },
}

/// Parses an incoming `setting_request_t` (76 bytes: `magic`(4), `name`(64),
/// `type`(4), `value`(4)), matching `src/consumer.rs::build_get_all_request`
/// and `build_set_request`.
pub fn parse_setting_request(bytes: &[u8]) -> Option<SettingRequest> {
    if bytes.len() < 4 {
        return None;
    }
    let magic = u32::from_ne_bytes(bytes[0..4].try_into().unwrap());
    match magic {
        SETTING_REQ_GET_ALL => Some(SettingRequest::GetAll),
        SETTING_REQ_SET => {
            if bytes.len() < 76 {
                return None;
            }
            let name_bytes = &bytes[4..68];
            let len = name_bytes.iter().position(|&b| b == 0).unwrap_or(64);
            let name = String::from_utf8_lossy(&name_bytes[..len]).into_owned();
            let setting_type_raw = i32::from_ne_bytes(bytes[68..72].try_into().unwrap());
            let mut raw_value = [0u8; 4];
            raw_value.copy_from_slice(&bytes[72..76]);
            Some(SettingRequest::Set {
                name,
                setting_type_raw,
                raw_value,
            })
        }
        _ => None,
    }
}

/// Encodes a `SETTING_REP_GET_ALL` reply (`setting_response_t` + settings
/// array). The producer crate currently always calls this with an empty
/// slice -- see the `// STUB:` comment on the control worker in
/// `producer.rs` for why the dynamic settings protocol isn't implemented yet.
pub fn encode_setting_get_all_response(settings: &[SettingWire]) -> Vec<u8> {
    let mut bytes = vec![0u8; 12 + settings.len() * 72];
    bytes[0..4].copy_from_slice(&SETTING_REP_GET_ALL.to_ne_bytes());
    bytes[4..8].copy_from_slice(&0i32.to_ne_bytes()); // status
    bytes[8..12].copy_from_slice(&(settings.len() as i32).to_ne_bytes());
    for (i, s) in settings.iter().enumerate() {
        let off = 12 + i * 72;
        write_c_str(&mut bytes[off..off + 64], &s.name);
        bytes[off + 64..off + 68].copy_from_slice(&(s.setting_type as i32).to_ne_bytes());
        match s.value {
            SettingValue::Bool(b) => bytes[off + 68] = if b { 1 } else { 0 },
            SettingValue::Int32(v) => bytes[off + 68..off + 72].copy_from_slice(&v.to_ne_bytes()),
            SettingValue::Float(v) => bytes[off + 68..off + 72].copy_from_slice(&v.to_ne_bytes()),
        }
    }
    bytes
}

/// Encodes a `SETTING_REP_SET` reply (12-byte header, no settings array).
/// `status` mirrors the C++ producer's convention: `0` = applied, `-2` =
/// type mismatch, `-3` = setting not found.
pub fn encode_setting_set_response(status: i32) -> Vec<u8> {
    let mut bytes = vec![0u8; 12];
    bytes[0..4].copy_from_slice(&SETTING_REP_SET.to_ne_bytes());
    bytes[4..8].copy_from_slice(&status.to_ne_bytes());
    bytes[8..12].copy_from_slice(&0i32.to_ne_bytes()); // num_settings
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announce_response_header_layout() {
        let bytes = encode_announce_response("prod", "tcp://a", "tcp://b", &[]);
        assert_eq!(bytes.len(), ANNOUNCE_HEADER_LEN);
        assert_eq!(&bytes[0..7], b"tcp://a");
        assert_eq!(&bytes[128..135], b"tcp://b");
        assert_eq!(&bytes[256..260], b"prod");
        let num_scopes = i32::from_ne_bytes(bytes[384..388].try_into().unwrap());
        assert_eq!(num_scopes, 0);
    }

    #[test]
    fn announce_response_scope_entries() {
        let scopes = vec![
            ScopeConfigWire {
                name: "IQ".into(),
                group: "grp".into(),
                scope_type: ScopeType::IqData,
                domain: ScopeDomain::Time,
            },
            ScopeConfigWire {
                name: "Real".into(),
                group: "".into(),
                scope_type: ScopeType::Real,
                domain: ScopeDomain::Frequency,
            },
        ];
        let bytes = encode_announce_response("n", "d", "c", &scopes);
        assert_eq!(
            bytes.len(),
            ANNOUNCE_HEADER_LEN + 2 * ANNOUNCE_SCOPE_ENTRY_LEN
        );

        let off0 = ANNOUNCE_HEADER_LEN;
        assert_eq!(&bytes[off0..off0 + 2], b"IQ");
        assert_eq!(&bytes[off0 + 64..off0 + 67], b"grp");
        let type0 = i32::from_ne_bytes(bytes[off0 + 128..off0 + 132].try_into().unwrap());
        assert_eq!(type0, ScopeType::IqData as i32);
        let domain0 = i32::from_ne_bytes(bytes[off0 + 132..off0 + 136].try_into().unwrap());
        assert_eq!(domain0, ScopeDomain::Time as i32);

        let off1 = ANNOUNCE_HEADER_LEN + ANNOUNCE_SCOPE_ENTRY_LEN;
        assert_eq!(&bytes[off1..off1 + 4], b"Real");
        let domain1 = i32::from_ne_bytes(bytes[off1 + 132..off1 + 136].try_into().unwrap());
        assert_eq!(domain1, ScopeDomain::Frequency as i32);
    }

    #[test]
    fn announce_request_validation() {
        assert!(is_valid_announce_request(&ANNOUNCE_MSG_ID.to_ne_bytes()));
        assert!(!is_valid_announce_request(&0xDEADBEEFu32.to_ne_bytes()));
        assert!(!is_valid_announce_request(&[0u8; 2]));
    }

    #[test]
    fn scope_request_roundtrip() {
        let mut bytes = [0u8; 8];
        bytes[0..4].copy_from_slice(&SCOPE_REQ_MSG_ID.to_ne_bytes());
        bytes[4..8].copy_from_slice(&7i32.to_ne_bytes());
        assert_eq!(parse_scope_request(&bytes), Some(7));

        let mut bad = bytes;
        bad[0..4].copy_from_slice(&0u32.to_ne_bytes());
        assert_eq!(parse_scope_request(&bad), None);

        assert_eq!(parse_scope_request(&[0u8; 4]), None);
    }

    #[test]
    fn scope_message_header_layout() {
        let meta = NRmetadata {
            frame: 10,
            slot: 2,
            timestamp: 1000,
        };
        let payload = encode_real(&[100, -200, 300]);
        let bytes = encode_scope_message(3, meta, 500, &payload);

        assert_eq!(bytes.len(), SCOPE_MSG_HEADER_LEN + payload.len());
        assert_eq!(u32::from_ne_bytes(bytes[0..4].try_into().unwrap()), 10);
        assert_eq!(u32::from_ne_bytes(bytes[4..8].try_into().unwrap()), 2);
        assert_eq!(u64::from_ne_bytes(bytes[8..16].try_into().unwrap()), 1000);
        assert_eq!(u64::from_ne_bytes(bytes[16..24].try_into().unwrap()), 500);
        assert_eq!(i32::from_ne_bytes(bytes[24..28].try_into().unwrap()), 3);
        assert_eq!(
            u64::from_ne_bytes(bytes[32..40].try_into().unwrap()),
            payload.len() as u64
        );
        assert_eq!(&bytes[48..], &payload[..]);
    }

    #[test]
    fn encode_real_matches_i16_le() {
        let bytes = encode_real(&[100, -200, 300]);
        assert_eq!(bytes.len(), 6);
        assert_eq!(i16::from_ne_bytes(bytes[0..2].try_into().unwrap()), 100);
        assert_eq!(i16::from_ne_bytes(bytes[2..4].try_into().unwrap()), -200);
        assert_eq!(i16::from_ne_bytes(bytes[4..6].try_into().unwrap()), 300);
    }

    #[test]
    fn encode_iq_interleaves_real_then_imag() {
        let bytes = encode_iq(&[10, -30], &[20, 40]);
        assert_eq!(bytes.len(), 8);
        assert_eq!(i16::from_ne_bytes(bytes[0..2].try_into().unwrap()), 10);
        assert_eq!(i16::from_ne_bytes(bytes[2..4].try_into().unwrap()), 20);
        assert_eq!(i16::from_ne_bytes(bytes[4..6].try_into().unwrap()), -30);
        assert_eq!(i16::from_ne_bytes(bytes[6..8].try_into().unwrap()), 40);
    }

    #[test]
    #[should_panic]
    fn encode_iq_mismatched_lengths_panics() {
        encode_iq(&[1, 2], &[1]);
    }

    #[test]
    fn encode_int32_roundtrip() {
        let bytes = encode_int32(&[100000, -200000]);
        assert_eq!(i32::from_ne_bytes(bytes[0..4].try_into().unwrap()), 100000);
        assert_eq!(i32::from_ne_bytes(bytes[4..8].try_into().unwrap()), -200000);
    }

    #[test]
    fn encode_float_roundtrip() {
        let bytes = encode_float(&[1.5, -2.5]);
        assert!((f32::from_ne_bytes(bytes[0..4].try_into().unwrap()) - 1.5).abs() < 1e-6);
        assert!((f32::from_ne_bytes(bytes[4..8].try_into().unwrap()) - (-2.5)).abs() < 1e-6);
    }

    #[test]
    fn setting_get_all_request_parsed() {
        let mut bytes = vec![0u8; 76];
        bytes[0..4].copy_from_slice(&SETTING_REQ_GET_ALL.to_ne_bytes());
        assert!(matches!(
            parse_setting_request(&bytes),
            Some(SettingRequest::GetAll)
        ));
    }

    #[test]
    fn setting_set_request_parsed() {
        let mut bytes = vec![0u8; 76];
        bytes[0..4].copy_from_slice(&SETTING_REQ_SET.to_ne_bytes());
        bytes[4..8].copy_from_slice(b"gain");
        bytes[68..72].copy_from_slice(&(SettingType::Float as i32).to_ne_bytes());
        bytes[72..76].copy_from_slice(&2.75f32.to_ne_bytes());

        match parse_setting_request(&bytes).unwrap() {
            SettingRequest::Set {
                name,
                setting_type_raw,
                raw_value,
            } => {
                assert_eq!(name, "gain");
                assert_eq!(setting_type_raw, SettingType::Float as i32);
                assert!((f32::from_ne_bytes(raw_value) - 2.75).abs() < 1e-5);
            }
            _ => panic!("expected Set request"),
        }
    }

    #[test]
    fn setting_get_all_response_empty() {
        let bytes = encode_setting_get_all_response(&[]);
        assert_eq!(bytes.len(), 12);
        assert_eq!(
            u32::from_ne_bytes(bytes[0..4].try_into().unwrap()),
            SETTING_REP_GET_ALL
        );
        assert_eq!(i32::from_ne_bytes(bytes[8..12].try_into().unwrap()), 0);
    }

    #[test]
    fn setting_set_response_status() {
        let bytes = encode_setting_set_response(-3);
        assert_eq!(bytes.len(), 12);
        assert_eq!(
            u32::from_ne_bytes(bytes[0..4].try_into().unwrap()),
            SETTING_REP_SET
        );
        assert_eq!(i32::from_ne_bytes(bytes[4..8].try_into().unwrap()), -3);
    }
}
