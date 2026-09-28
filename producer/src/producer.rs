/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

//! The [`Producer`] type: a native-Rust counterpart to the C++
//! `ImscopeProducer` in `common/src/imscope_producer.cpp`. It opens a single
//! Zenoh [`Session`] and declares one [`Queryable`] each for the announce
//! and control key expressions, plus one `Queryable` per scope for data --
//! mirroring the C++ side's `AnnounceCtx` / `SettingsCtx` / `ScopeCtx`
//! pattern, which is what lets multiple independent consumers each run
//! their own query/reply cycle concurrently.
//!
//! # Key-expression scheme
//! Given a producer `name`, this crate uses:
//! - `imscope/<name>/announce` -- announce handshake.
//! - `imscope/<name>/control` -- settings get/set (stubbed, see
//!   [`spawn_control_queryable`]).
//! - `imscope/<name>/data/<scope_id>` -- one `Queryable` per scope, where
//!   `scope_id` is the scope's decimal index into the `scopes` slice passed
//!   to [`Producer::new`].
//!
//! A later migration stage's consumer code depends on these exact key
//! expressions matching.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use zenoh::Wait;
use zenoh::query::{Query, Queryable};

use crate::wire::{self, NRmetadata, ScopeConfigWire, ScopeDomain, ScopeType, SettingRequest};

/// A scope's static description, mirrors `imscope_scope_desc_t` (plus the
/// `group` field that only the C API's `*_by_group` helpers expose).
#[derive(Debug, Clone)]
pub struct ScopeDesc {
    pub name: String,
    pub scope_type: ScopeType,
    pub domain: ScopeDomain,
    pub group: String,
}

impl ScopeDesc {
    pub fn new(name: impl Into<String>, scope_type: ScopeType) -> Self {
        Self {
            name: name.into(),
            scope_type,
            domain: ScopeDomain::Time,
            group: String::new(),
        }
    }

    pub fn with_domain(mut self, domain: ScopeDomain) -> Self {
        self.domain = domain;
        self
    }

    pub fn with_group(mut self, group: impl Into<String>) -> Self {
        self.group = group.into();
        self
    }
}

/// Errors that can occur while setting up a [`Producer`].
#[derive(Debug)]
pub enum ProducerError {
    Zenoh(zenoh::Error),
}

impl fmt::Display for ProducerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProducerError::Zenoh(e) => write!(f, "Zenoh error: {e}"),
        }
    }
}

impl std::error::Error for ProducerError {}

impl From<zenoh::Error> for ProducerError {
    fn from(e: zenoh::Error) -> Self {
        ProducerError::Zenoh(e)
    }
}

/// Errors from a non-blocking `try_send_*` call.
#[derive(Debug)]
pub enum TrySendError {
    /// No consumer currently has an outstanding request for this scope --
    /// the exact non-blocking-drop semantics of the C++ producer's
    /// `IMSCOPE_ERROR_BUSY` return from `imscope_try_send_data`. The caller
    /// should simply drop the frame and try again next time, not block.
    NoPendingRequest,
    /// `scope_id` is out of range for the scopes this producer was
    /// constructed with.
    InvalidScopeId,
    /// `real`/`imag` slices passed to [`Producer::try_send_iq`] had
    /// different lengths.
    MismatchedLengths,
    /// The scope was declared with a different [`ScopeType`] than the
    /// `try_send_*` method used.
    WrongScopeType,
    /// The Zenoh reply itself failed (e.g. the querying peer vanished
    /// between the request and the reply).
    Zenoh(zenoh::Error),
}

impl fmt::Display for TrySendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TrySendError::NoPendingRequest => write!(f, "no consumer request pending for scope"),
            TrySendError::InvalidScopeId => write!(f, "invalid scope id"),
            TrySendError::MismatchedLengths => write!(f, "real/imag slice length mismatch"),
            TrySendError::WrongScopeType => write!(f, "scope was not registered with this type"),
            TrySendError::Zenoh(e) => write!(f, "Zenoh error: {e}"),
        }
    }
}

impl std::error::Error for TrySendError {}

/// A `Query` that has just been received on a scope's data `Queryable` and
/// is waiting for the application to hand it a reply. Analogous to a
/// "waiting" `ScopeCtx` in the C++ producer being recorded in
/// `active_requests`. Zenoh's `Query` can be cloned and replied to later,
/// outside the `Queryable` callback that received it -- that's what makes
/// this parked-request pattern possible over Zenoh, exactly as NNG's parked
/// `Context`/`Aio` pair did before this crate's Zenoh migration.
struct PendingSlot {
    query: Query,
}

struct DataShared {
    pending: Mutex<HashMap<i32, PendingSlot>>,
}

struct AnnounceInfo {
    name: String,
    data_address: String,
    control_address: String,
    scopes: Vec<ScopeConfigWire>,
}

/// Declares the announce `Queryable`. On a valid announce request, replies
/// with the unchanged `wire::encode_announce_response` byte blob, using the
/// `imscope/<name>/data` and `imscope/<name>/control` key-expression
/// prefixes as the (now purely informational -- nothing dials them) address
/// fields.
fn spawn_announce_queryable(
    session: &zenoh::Session,
    key_expr: &str,
    info: Arc<AnnounceInfo>,
) -> Result<Queryable<()>, zenoh::Error> {
    session
        .declare_queryable(key_expr)
        .callback(move |query: Query| {
            let Some(payload) = query.payload() else {
                return;
            };
            let bytes = payload.to_bytes();
            if !wire::is_valid_announce_request(&bytes) {
                return;
            }
            let reply_bytes = wire::encode_announce_response(
                &info.name,
                &info.data_address,
                &info.control_address,
                &info.scopes,
            );
            let _ = query.reply(query.key_expr().clone(), reply_bytes).wait();
        })
        .wait()
}

/// Declares the control/settings `Queryable`.
///
/// # Control protocol status: STUBBED
/// This producer does not implement dynamic runtime settings
/// (`imscope_register_setting_bool/int32/float` on the C++ side has no
/// Rust equivalent yet). `SETTING_REQ_GET_ALL` always replies with zero
/// settings, and `SETTING_REQ_SET` always replies with status `-3` ("not
/// found"), which is exactly how the C++ producer replies when asked to set
/// a setting name it doesn't recognize -- so existing consumers (which only
/// care about the wire-level status code) degrade gracefully instead of
/// erroring out. Extending this to a real settings registry would mean
/// adding a `Producer::register_setting_*` API mirroring
/// `common/src/imscope_producer.cpp`'s `SettingInfo`/`registered_settings`
/// and answering from that state here instead of the constants below.
fn spawn_control_queryable(
    session: &zenoh::Session,
    key_expr: &str,
) -> Result<Queryable<()>, zenoh::Error> {
    session
        .declare_queryable(key_expr)
        .callback(move |query: Query| {
            let Some(payload) = query.payload() else {
                return;
            };
            let bytes = payload.to_bytes();
            let reply_bytes = match wire::parse_setting_request(&bytes) {
                Some(SettingRequest::GetAll) => wire::encode_setting_get_all_response(&[]),
                Some(SettingRequest::Set { .. }) => wire::encode_setting_set_response(-3),
                None => return,
            };
            let _ = query.reply(query.key_expr().clone(), reply_bytes).wait();
        })
        .wait()
}

/// Declares one scope's data `Queryable`. On a valid scope data request,
/// clones the `Query` and parks it in `shared.pending` keyed by the
/// requested scope id, instead of replying immediately -- the reply is sent
/// later by [`Producer::try_send_payload`], once the application has actual
/// sample data to hand back. If nothing ever calls a `try_send_*` for this
/// scope, the `Query` is simply dropped (replaced by the next request, or
/// dropped along with the producer) and Zenoh times the requester out --
/// the same drop-if-nobody-produces semantics the NNG version had.
fn spawn_data_queryable(
    session: &zenoh::Session,
    key_expr: &str,
    shared: Arc<DataShared>,
) -> Result<Queryable<()>, zenoh::Error> {
    session
        .declare_queryable(key_expr)
        .callback(move |query: Query| {
            let Some(payload) = query.payload() else {
                return;
            };
            let bytes = payload.to_bytes();
            if let Some(scope_id) = wire::parse_scope_request(&bytes) {
                let mut pending = shared.pending.lock().unwrap();
                pending.insert(scope_id, PendingSlot { query });
                // Deliberately do not reply here: this `Query` now belongs
                // to `scope_id` until a `try_send_*` call claims it from
                // `pending` and replies. This mirrors the C++
                // `ScopeCtx::recv_callback`'s early return after
                // registering itself in `active_requests`.
            }
            // Unrecognized request: drop it, `query` goes out of scope
            // unanswered and the requester's `get()` eventually times out.
        })
        .wait()
}

struct Inner {
    scopes: Vec<ScopeDesc>,
    data_shared: Arc<DataShared>,
    // Kept alive for the lifetime of the producer: dropping a `Queryable`
    // undeclares it, which would stop it from receiving queries.
    _session: zenoh::Session,
    _announce_queryable: Queryable<()>,
    _control_queryable: Queryable<()>,
    _data_queryables: Vec<Queryable<()>>,
}

/// A native-Rust imscope data producer.
///
/// Opens a single Zenoh [`Session`](zenoh::Session) and declares the
/// announce/control/data `Queryable`s used by the imscope wire protocol,
/// serving the announce handshake plus scope data requests from any number
/// of concurrent `imscope-gui` / `imscope-tui` / `imscope-view` / C++
/// consumers. Cloning a `Producer` is cheap (it's an `Arc` handle) and all
/// clones share the same session.
#[derive(Clone)]
pub struct Producer {
    inner: Arc<Inner>,
}

impl Producer {
    /// Opens a Zenoh session and starts serving `scopes` under the
    /// `imscope/<name>/...` key-expression prefix (see the module docs for
    /// the exact scheme).
    ///
    /// `locator` is an optional Zenoh endpoint to explicitly connect to
    /// (e.g. `"tcp/127.0.0.1:7447"`); pass `None` to rely on Zenoh's default
    /// scouting/discovery instead.
    pub fn new(
        locator: Option<&str>,
        name: &str,
        scopes: &[ScopeDesc],
    ) -> Result<Self, ProducerError> {
        let mut config = zenoh::Config::default();
        if let Some(loc) = locator {
            config
                .insert_json5("connect/endpoints", &format!("[{loc:?}]"))
                .map_err(ProducerError::Zenoh)?;
        }
        let session = zenoh::open(config).wait().map_err(ProducerError::Zenoh)?;

        let announce_key = format!("imscope/{name}/announce");
        let control_key = format!("imscope/{name}/control");
        let data_key_prefix = format!("imscope/{name}/data");

        let announce_info = Arc::new(AnnounceInfo {
            name: name.to_string(),
            data_address: data_key_prefix.clone(),
            control_address: control_key.clone(),
            scopes: scopes
                .iter()
                .map(|s| ScopeConfigWire {
                    name: s.name.clone(),
                    group: s.group.clone(),
                    scope_type: s.scope_type,
                    domain: s.domain,
                })
                .collect(),
        });
        let announce_queryable = spawn_announce_queryable(&session, &announce_key, announce_info)?;
        let control_queryable = spawn_control_queryable(&session, &control_key)?;

        let data_shared = Arc::new(DataShared {
            pending: Mutex::new(HashMap::new()),
        });
        let mut data_queryables = Vec::with_capacity(scopes.len());
        for scope_id in 0..scopes.len() {
            let key = format!("{data_key_prefix}/{scope_id}");
            data_queryables.push(spawn_data_queryable(&session, &key, data_shared.clone())?);
        }

        Ok(Producer {
            inner: Arc::new(Inner {
                scopes: scopes.to_vec(),
                data_shared,
                _session: session,
                _announce_queryable: announce_queryable,
                _control_queryable: control_queryable,
                _data_queryables: data_queryables,
            }),
        })
    }

    /// Looks up a scope's id by name, as assigned at construction (index
    /// into the `scopes` slice passed to [`Producer::new`]).
    pub fn scope_id_by_name(&self, name: &str) -> Option<usize> {
        self.inner.scopes.iter().position(|s| s.name == name)
    }

    fn check_scope(&self, scope_id: usize, expected: ScopeType) -> Result<i32, TrySendError> {
        let scope = self
            .inner
            .scopes
            .get(scope_id)
            .ok_or(TrySendError::InvalidScopeId)?;
        if scope.scope_type != expected {
            return Err(TrySendError::WrongScopeType);
        }
        i32::try_from(scope_id).map_err(|_| TrySendError::InvalidScopeId)
    }

    /// Non-blocking send: if a consumer currently has an outstanding request
    /// for `scope_id`, encodes `payload` and replies immediately; otherwise
    /// drops the frame and returns [`TrySendError::NoPendingRequest`] without
    /// blocking. This matches `imscope_try_send_data`'s `IMSCOPE_ERROR_BUSY`
    /// drop-rather-than-block semantics on the C++ side.
    fn try_send_payload(
        &self,
        scope_id: i32,
        meta: NRmetadata,
        payload: Vec<u8>,
    ) -> Result<(), TrySendError> {
        let slot = {
            let mut pending = self.inner.data_shared.pending.lock().unwrap();
            pending.remove(&scope_id)
        };
        let PendingSlot { query } = slot.ok_or(TrySendError::NoPendingRequest)?;

        let start = Instant::now();
        let mut bytes = wire::encode_scope_message(scope_id, meta, 0, &payload);
        let time_taken_in_ns = start.elapsed().as_nanos() as u64;
        // Patch in the measured encode time (mirrors the C++ producer
        // timing just the copy/serialize step, not the network send).
        bytes[16..24].copy_from_slice(&time_taken_in_ns.to_ne_bytes());

        query
            .reply(query.key_expr().clone(), bytes)
            .wait()
            .map_err(TrySendError::Zenoh)
    }

    /// Sends real-only (`SCOPE_TYPE_REAL`) samples: one `i16` per sample.
    pub fn try_send_real(
        &self,
        scope_id: usize,
        samples: &[i16],
        meta: NRmetadata,
    ) -> Result<(), TrySendError> {
        let id = self.check_scope(scope_id, ScopeType::Real)?;
        self.try_send_payload(id, meta, wire::encode_real(samples))
    }

    /// Sends IQ (`SCOPE_TYPE_IQ_DATA`) samples: `real[i]`/`imag[i]` pairs,
    /// packed as interleaved `i16` (real then imag) per sample.
    pub fn try_send_iq(
        &self,
        scope_id: usize,
        real: &[i16],
        imag: &[i16],
        meta: NRmetadata,
    ) -> Result<(), TrySendError> {
        if real.len() != imag.len() {
            return Err(TrySendError::MismatchedLengths);
        }
        let id = self.check_scope(scope_id, ScopeType::IqData)?;
        self.try_send_payload(id, meta, wire::encode_iq(real, imag))
    }

    /// Sends `SCOPE_TYPE_INT32` samples: one `i32` per sample.
    pub fn try_send_int32(
        &self,
        scope_id: usize,
        samples: &[i32],
        meta: NRmetadata,
    ) -> Result<(), TrySendError> {
        let id = self.check_scope(scope_id, ScopeType::Int32)?;
        self.try_send_payload(id, meta, wire::encode_int32(samples))
    }

    /// Sends `SCOPE_TYPE_FLOAT` samples: one `f32` per sample.
    pub fn try_send_float(
        &self,
        scope_id: usize,
        samples: &[f32],
        meta: NRmetadata,
    ) -> Result<(), TrySendError> {
        let id = self.check_scope(scope_id, ScopeType::Float)?;
        self.try_send_payload(id, meta, wire::encode_float(samples))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_desc_builder_defaults() {
        let scope = ScopeDesc::new("Foo", ScopeType::IqData);
        assert_eq!(scope.name, "Foo");
        assert_eq!(scope.domain, ScopeDomain::Time);
        assert_eq!(scope.group, "");

        let scope = scope.with_domain(ScopeDomain::Frequency).with_group("grp");
        assert_eq!(scope.domain, ScopeDomain::Frequency);
        assert_eq!(scope.group, "grp");
    }
}
