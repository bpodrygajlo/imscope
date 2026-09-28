/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

//! End-to-end smoke test: starts a real [`Producer`] and drives it with a
//! hand-rolled Zenoh client that queries exactly like a real consumer would
//! (same magics, same offsets, same `imscope/<name>/...` key expressions),
//! without depending on the `imscope` crate (that pulls in the whole GUI
//! stack, which would make `cargo test` for this crate needlessly heavy).
//! This is the network-level counterpart to the pure byte-layout unit tests
//! in `src/wire.rs`.

use std::convert::TryInto;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use zenoh::Session;
use zenoh::Wait;

use imscope_producer_rs::{NRmetadata, Producer, ScopeDesc, ScopeType, TrySendError};

const ANNOUNCE_MSG_ID: u32 = 0xABCDEF01;
const SCOPE_REQ_MSG_ID: u32 = 0xABCDEF02;
const SETTING_REQ_GET_ALL: u32 = 0xABCDEF10;
const SETTING_REQ_SET: u32 = 0xABCDEF11;
const SETTING_REP_GET_ALL: u32 = 0xABCDEF20;
const SETTING_REP_SET: u32 = 0xABCDEF21;

/// Query timeout used throughout: generous relative to the producer pump
/// thread's 5ms cadence in [`end_to_end_announce_data_and_control`], but
/// short enough that a genuinely-never-answered query (the
/// drop-if-nobody-waiting test) doesn't slow the suite down much.
const QUERY_TIMEOUT: Duration = Duration::from_secs(3);

fn open_client() -> Session {
    zenoh::open(zenoh::Config::default())
        .wait()
        .expect("open zenoh client session")
}

/// Sends one Zenoh `get()` carrying `payload` to `key_expr` and returns the
/// first reply's payload bytes.
fn query(session: &Session, key_expr: &str, payload: Vec<u8>) -> Vec<u8> {
    let receiver = session
        .get(key_expr)
        .payload(payload)
        .timeout(QUERY_TIMEOUT)
        .wait()
        .expect("send get");
    let reply = receiver.recv().expect("recv reply");
    let sample = reply.result().expect("reply carried an error");
    sample.payload().to_bytes().into_owned()
}

fn parse_c_str(bytes: &[u8]) -> String {
    let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..len]).into_owned()
}

#[test]
fn end_to_end_announce_data_and_control() {
    let name = "integration_test";
    let announce_key = format!("imscope/{name}/announce");
    let control_key = format!("imscope/{name}/control");
    let data_key_prefix = format!("imscope/{name}/data");

    let scopes = [
        ScopeDesc::new("RealScope", ScopeType::Real),
        ScopeDesc::new("IqScope", ScopeType::IqData),
        ScopeDesc::new("Int32Scope", ScopeType::Int32),
        ScopeDesc::new("FloatScope", ScopeType::Float),
    ];

    let producer = Producer::new(None, name, &scopes).expect("producer");

    // Keep producing on every scope in the background so that whenever the
    // test client's request lands, there's a producer thread ready to
    // notice the pending slot and reply -- mirroring how a real embedding
    // application continuously calls try_send_* regardless of whether a
    // consumer happens to be connected right now.
    let stop = Arc::new(AtomicBool::new(false));
    let pump_stop = stop.clone();
    let pump_producer = producer.clone();
    let pump = thread::spawn(move || {
        let mut t: u64 = 0;
        while !pump_stop.load(Ordering::Relaxed) {
            let meta = NRmetadata {
                frame: 1,
                slot: 2,
                timestamp: t,
            };
            let real_samples = [1i16, -2, 3];
            let iq_real = [10i16, -20];
            let iq_imag = [30i16, -40];
            let int32_samples = [100_000i32, -200_000];
            let float_samples = [1.5f32, -2.5];

            for result in [
                pump_producer.try_send_real(0, &real_samples, meta),
                pump_producer.try_send_iq(1, &iq_real, &iq_imag, meta),
            ] {
                if let Err(e) = result {
                    assert!(matches!(e, TrySendError::NoPendingRequest));
                }
            }
            if let Err(e) = pump_producer.try_send_int32(2, &int32_samples, meta) {
                assert!(matches!(e, TrySendError::NoPendingRequest));
            }
            if let Err(e) = pump_producer.try_send_float(3, &float_samples, meta) {
                assert!(matches!(e, TrySendError::NoPendingRequest));
            }

            t += 1;
            thread::sleep(Duration::from_millis(5));
        }
    });

    let client = open_client();

    // --- Announce handshake ---
    let resp = query(
        &client,
        &announce_key,
        ANNOUNCE_MSG_ID.to_ne_bytes().to_vec(),
    );

    let resp_data_addr = parse_c_str(&resp[0..128]);
    let resp_control_addr = parse_c_str(&resp[128..256]);
    let resp_name = parse_c_str(&resp[256..384]);
    let num_scopes = i32::from_ne_bytes(resp[384..388].try_into().unwrap());

    assert_eq!(resp_data_addr, data_key_prefix);
    assert_eq!(resp_control_addr, control_key);
    assert_eq!(resp_name, name);
    assert_eq!(num_scopes, 4);

    let expected_types = [0i32, 1, 2, 3]; // Real, IqData, Int32, Float
    let expected_names = ["RealScope", "IqScope", "Int32Scope", "FloatScope"];
    for i in 0..4usize {
        let off = 388 + i * 136;
        let name = parse_c_str(&resp[off..off + 64]);
        let stype = i32::from_ne_bytes(resp[off + 128..off + 132].try_into().unwrap());
        let sdomain = i32::from_ne_bytes(resp[off + 132..off + 136].try_into().unwrap());
        assert_eq!(name, expected_names[i]);
        assert_eq!(stype, expected_types[i]);
        assert_eq!(sdomain, 0); // ScopeDomain::Time by default
    }

    // --- Scope data fetch, one per type ---
    // scope 0: Real (i16 samples)
    fetch_and_check(&client, &data_key_prefix, 0, 3 * 2);
    // scope 1: IqData (i16 real + i16 imag interleaved)
    fetch_and_check(&client, &data_key_prefix, 1, 2 * 4);
    // scope 2: Int32
    fetch_and_check(&client, &data_key_prefix, 2, 2 * 4);
    // scope 3: Float
    fetch_and_check(&client, &data_key_prefix, 3, 2 * 4);

    // --- Control/settings stub ---
    let mut get_all_req = [0u8; 76];
    get_all_req[0..4].copy_from_slice(&SETTING_REQ_GET_ALL.to_ne_bytes());
    let reply = query(&client, &control_key, get_all_req.to_vec());
    assert_eq!(reply.len(), 12);
    let magic = u32::from_ne_bytes(reply[0..4].try_into().unwrap());
    let status = i32::from_ne_bytes(reply[4..8].try_into().unwrap());
    let num_settings = i32::from_ne_bytes(reply[8..12].try_into().unwrap());
    assert_eq!(magic, SETTING_REP_GET_ALL);
    assert_eq!(status, 0);
    assert_eq!(num_settings, 0);

    let mut set_req = [0u8; 76];
    set_req[0..4].copy_from_slice(&SETTING_REQ_SET.to_ne_bytes());
    set_req[4..8].copy_from_slice(b"gain");
    let reply = query(&client, &control_key, set_req.to_vec());
    let magic = u32::from_ne_bytes(reply[0..4].try_into().unwrap());
    let status = i32::from_ne_bytes(reply[4..8].try_into().unwrap());
    assert_eq!(magic, SETTING_REP_SET);
    assert_eq!(status, -3); // stub: setting never found

    stop.store(true, Ordering::Relaxed);
    pump.join().unwrap();
}

fn fetch_and_check(
    client: &Session,
    data_key_prefix: &str,
    scope_id: i32,
    expected_data_size: usize,
) {
    let mut req = [0u8; 8];
    req[0..4].copy_from_slice(&SCOPE_REQ_MSG_ID.to_ne_bytes());
    req[4..8].copy_from_slice(&scope_id.to_ne_bytes());
    let key_expr = format!("{data_key_prefix}/{scope_id}");
    let reply = query(client, &key_expr, req.to_vec());

    assert!(reply.len() >= 48, "reply too short: {}", reply.len());
    let frame = u32::from_ne_bytes(reply[0..4].try_into().unwrap());
    let slot = u32::from_ne_bytes(reply[4..8].try_into().unwrap());
    let id = i32::from_ne_bytes(reply[24..28].try_into().unwrap());
    let data_size = u64::from_ne_bytes(reply[32..40].try_into().unwrap());

    assert_eq!(frame, 1);
    assert_eq!(slot, 2);
    assert_eq!(id, scope_id);
    assert_eq!(data_size as usize, expected_data_size);
    assert_eq!(reply.len(), 48 + data_size as usize);
}

/// Confirms the drop-if-nobody-waiting semantics still hold over Zenoh: with
/// no outstanding query for a scope, `try_send_real` must not block and must
/// report [`TrySendError::NoPendingRequest`], exactly as the NNG-based
/// producer did (`imscope_try_send_data`'s `IMSCOPE_ERROR_BUSY`).
#[test]
fn try_send_without_pending_request_is_dropped() {
    let scopes = [ScopeDesc::new("Lonely", ScopeType::Real)];
    let producer = Producer::new(None, "drop_test_producer", &scopes).expect("producer");

    let meta = NRmetadata {
        frame: 0,
        slot: 0,
        timestamp: 0,
    };
    let samples = [1i16, 2, 3];

    // Nobody has ever queried `imscope/drop_test_producer/data/0`, so this
    // must be dropped immediately rather than blocking or queuing.
    let result = producer.try_send_real(0, &samples, meta);
    assert!(matches!(result, Err(TrySendError::NoPendingRequest)));
}
