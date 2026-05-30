/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

#![allow(dead_code)]

use nng::options::{Options, RecvTimeout};
use nng::{Protocol, Socket};
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

pub const ANNOUNCE_MSG_ID: u32 = 0xABCDEF01;
pub const SCOPE_REQ_MSG_ID: u32 = 0xABCDEF02;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ScopeType {
    Real = 0,
    IqData = 1,
    Int32 = 2,
    Float = 3,
}

#[derive(Debug, Clone)]
pub struct ScopeConfig {
    pub name: String,
    pub group: String,
    pub scope_type: ScopeType,
}

#[derive(Debug, Clone)]
pub struct AnnounceResponse {
    pub name: String,
    pub data_address: String,
    pub control_address: String,
    pub scopes: Vec<ScopeConfig>,
}

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct NRmetadata {
    pub frame: u32,
    pub slot: u32,
    pub timestamp: u64,
}

#[derive(Debug, Clone)]
pub struct ScopeMessage {
    pub meta: NRmetadata,
    pub time_taken_in_ns: u64,
    pub id: i32,
    pub data_size: u64,
    pub real: Vec<f64>,
    pub imag: Vec<f64>, // empty if Real, Int32, or Float type
}

#[derive(Debug, Clone)]
pub struct IQSnapshot {
    pub scope_id: i32,
    pub meta: NRmetadata,
    pub real: Vec<f64>,
    pub imag: Vec<f64>,
    pub power: Vec<f32>,
    pub max_iq: f64,
    pub max_power: f32,
    pub nonzero_count: usize,
    pub min_val: f64,
    pub max_val: f64,

    // Stacking/collecting variables
    pub current_timestamp: u64,
    pub max_stacked_size: usize,
}

impl Default for IQSnapshot {
    fn default() -> Self {
        Self {
            scope_id: -1,
            meta: Default::default(),
            real: Vec::new(),
            imag: Vec::new(),
            power: Vec::new(),
            max_iq: 0.0,
            max_power: 0.0,
            nonzero_count: 0,
            min_val: 0.0,
            max_val: 0.0,
            current_timestamp: 0,
            max_stacked_size: 16000,
        }
    }
}

impl IQSnapshot {
    pub fn new(scope_id: i32) -> Self {
        Self {
            scope_id,
            max_stacked_size: 16000,
            ..Default::default()
        }
    }

    pub fn size(&self) -> usize {
        self.real.len()
    }

    pub fn preprocess(&mut self) {
        let size = self.size();
        self.power.resize(size, 0.0);
        self.max_iq = 0.0;
        self.max_power = 0.0;
        self.nonzero_count = 0;
        self.min_val = f64::MAX;
        self.max_val = f64::MIN;

        let has_imag = self.imag.len() >= size;

        for i in 0..size {
            let r = self.real[i];
            let im = if has_imag { self.imag[i] } else { 0.0 };

            let abs_r = r.abs();
            if abs_r > self.max_iq {
                self.max_iq = abs_r;
            }
            let abs_im = im.abs();
            if abs_im > self.max_iq {
                self.max_iq = abs_im;
            }

            if r < self.min_val {
                self.min_val = r;
            }
            if r > self.max_val {
                self.max_val = r;
            }

            let p = (r as f32) * (r as f32) + (im as f32) * (im as f32);
            self.power[i] = p;
            if p > self.max_power {
                self.max_power = p;
            }
            if p > 0.0 {
                self.nonzero_count += 1;
            }
        }

        if size == 0 {
            self.min_val = 0.0;
            self.max_val = 0.0;
        }
    }

    pub fn read_scope_msg(&mut self, msg: &ScopeMessage, collect: bool) {
        self.meta = msg.meta;
        let num_samples = msg.real.len();
        let is_iq = !msg.imag.is_empty();

        if !collect {
            self.real.clone_from(&msg.real);
            self.imag.clone_from(&msg.imag);
            self.preprocess();
        } else {
            let current_size = self.real.len();
            let mut gap = 0;
            if current_size > 0 && msg.meta.timestamp > self.current_timestamp {
                gap = (msg.meta.timestamp - self.current_timestamp) as usize;
            }

            let new_size = current_size + num_samples + gap;

            // Resize with zeroes for gaps
            self.real.resize(new_size, 0.0);
            if is_iq {
                self.imag.resize(new_size, 0.0);
            } else {
                self.imag.clear();
            }

            // Write new samples at the end
            for i in 0..num_samples {
                self.real[current_size + gap + i] = msg.real[i];
                if is_iq {
                    self.imag[current_size + gap + i] = msg.imag[i];
                }
            }

            self.current_timestamp = msg.meta.timestamp + num_samples as u64;

            if new_size > self.max_stacked_size {
                let to_remove = new_size - self.max_stacked_size;
                self.real.drain(0..to_remove);
                if is_iq {
                    self.imag.drain(0..to_remove);
                }
            }
            self.preprocess();
        }
    }
}

#[derive(Clone, Copy)]
#[repr(C)]
struct AnnounceResponseHeader {
    pub data_address: [u8; 128],
    pub control_address: [u8; 128],
    pub name: [u8; 128],
    pub num_scopes: i32,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct ImscopeScopeConfig {
    pub name: [u8; 64],
    pub group: [u8; 64],
    pub scope_type: i32,
}

pub fn parse_announce_response(bytes: &[u8]) -> Result<AnnounceResponse, String> {
    if bytes.len() < 388 {
        return Err("Message too short for announce header".into());
    }

    let data_address = parse_c_str(&bytes[0..128])?;
    let control_address = parse_c_str(&bytes[128..256])?;
    let name = parse_c_str(&bytes[256..384])?;
    let num_scopes = i32::from_ne_bytes(bytes[384..388].try_into().unwrap());

    if num_scopes < 0 {
        return Err("Negative number of scopes".into());
    }

    let mut scopes = Vec::new();
    let scope_size = 132; // 64 name + 64 group + 4 type
    let expected_len = 388 + (num_scopes as usize) * scope_size;
    if bytes.len() < expected_len {
        return Err(format!(
            "Announce message size {} is smaller than expected {} for {} scopes",
            bytes.len(),
            expected_len,
            num_scopes
        ));
    }

    for i in 0..num_scopes as usize {
        let offset = 388 + i * scope_size;
        let scope_name = parse_c_str(&bytes[offset..offset + 64])?;
        let scope_group = parse_c_str(&bytes[offset + 64..offset + 128])?;
        let scope_type_val =
            i32::from_ne_bytes(bytes[offset + 128..offset + 132].try_into().unwrap());
        let scope_type = match scope_type_val {
            0 => ScopeType::Real,
            1 => ScopeType::IqData,
            2 => ScopeType::Int32,
            3 => ScopeType::Float,
            _ => return Err(format!("Unknown scope type {}", scope_type_val)),
        };
        scopes.push(ScopeConfig {
            name: scope_name,
            group: scope_group,
            scope_type,
        });
    }

    Ok(AnnounceResponse {
        name,
        data_address,
        control_address,
        scopes,
    })
}

fn parse_c_str(bytes: &[u8]) -> Result<String, String> {
    let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    std::str::from_utf8(&bytes[..len])
        .map(|s| s.to_string())
        .map_err(|e| format!("Invalid UTF-8 string: {}", e))
}

#[derive(Clone, Copy)]
#[repr(C)]
struct ScopeRequest {
    magic: u32,
    scope_id: i32,
}

pub fn parse_scope_message(bytes: &[u8], scope_type: ScopeType) -> Result<ScopeMessage, String> {
    if bytes.len() < 48 {
        return Err("Scope message too short for header (minimum 48 bytes)".into());
    }

    let frame = u32::from_ne_bytes(bytes[0..4].try_into().unwrap());
    let slot = u32::from_ne_bytes(bytes[4..8].try_into().unwrap());
    let timestamp = u64::from_ne_bytes(bytes[8..16].try_into().unwrap());
    let time_taken_in_ns = u64::from_ne_bytes(bytes[16..24].try_into().unwrap());
    let id = i32::from_ne_bytes(bytes[24..28].try_into().unwrap());
    let data_size = u64::from_ne_bytes(bytes[32..40].try_into().unwrap());

    let expected_total_len = 48 + data_size as usize;
    if bytes.len() < expected_total_len {
        return Err(format!(
            "Scope message too small: got {} bytes, expected {} (data_size {})",
            bytes.len(),
            expected_total_len,
            data_size
        ));
    }

    let payload = &bytes[48..expected_total_len];
    let mut real = Vec::new();
    let mut imag = Vec::new();

    match scope_type {
        ScopeType::Real => {
            let num_samples = data_size as usize / 2;
            real.reserve(num_samples);
            for i in 0..num_samples {
                let val = i16::from_ne_bytes(payload[i * 2..i * 2 + 2].try_into().unwrap());
                real.push(val as f64);
            }
        }
        ScopeType::IqData => {
            let num_samples = data_size as usize / 4;
            real.reserve(num_samples);
            imag.reserve(num_samples);
            for i in 0..num_samples {
                let r_val = i16::from_ne_bytes(payload[i * 4..i * 4 + 2].try_into().unwrap());
                let im_val = i16::from_ne_bytes(payload[i * 4 + 2..i * 4 + 4].try_into().unwrap());
                real.push(r_val as f64);
                imag.push(im_val as f64);
            }
        }
        ScopeType::Int32 => {
            let num_samples = data_size as usize / 4;
            real.reserve(num_samples);
            for i in 0..num_samples {
                let val = i32::from_ne_bytes(payload[i * 4..i * 4 + 4].try_into().unwrap());
                real.push(val as f64);
            }
        }
        ScopeType::Float => {
            let num_samples = data_size as usize / 4;
            real.reserve(num_samples);
            for i in 0..num_samples {
                let val = f32::from_ne_bytes(payload[i * 4..i * 4 + 4].try_into().unwrap());
                real.push(val as f64);
            }
        }
    }

    Ok(ScopeMessage {
        meta: NRmetadata {
            frame,
            slot,
            timestamp,
        },
        time_taken_in_ns,
        id,
        data_size,
        real,
        imag,
    })
}

pub fn check_noise_filter(
    real: &[f64],
    imag: &[f64],
    noise_cutoff_linear: f32,
    noise_cutoff_percentage: f32,
) -> bool {
    if real.is_empty() {
        return true;
    }

    let mut num_noise_samples = 0;
    let num_samples = real.len();
    let cutoff_sq = noise_cutoff_linear * noise_cutoff_linear;
    let has_imag = imag.len() >= num_samples;

    for i in 0..num_samples {
        let r = real[i] as f32;
        let im = if has_imag { imag[i] as f32 } else { 0.0 };
        let square = r * r + im * im;
        if square < cutoff_sq {
            num_noise_samples += 1;
        }
    }

    let noise_percentage = (num_noise_samples as f32 / num_samples as f32) * 100.0;
    noise_percentage <= noise_cutoff_percentage
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum SettingType {
    Bool = 0,
    Int32 = 1,
    Float = 2,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SettingValue {
    Bool(bool),
    Int32(i32),
    Float(f32),
}

#[derive(Debug, Clone)]
pub struct SettingInfo {
    pub name: String,
    pub setting_type: SettingType,
    pub value: SettingValue,
}

pub const SETTING_REQ_GET_ALL: u32 = 0xABCDEF10;
pub const SETTING_REQ_SET: u32 = 0xABCDEF11;
pub const SETTING_REP_GET_ALL: u32 = 0xABCDEF20;
pub const SETTING_REP_SET: u32 = 0xABCDEF21;

pub fn build_get_all_request() -> Vec<u8> {
    let mut bytes = vec![0u8; 76];
    bytes[0..4].copy_from_slice(&SETTING_REQ_GET_ALL.to_ne_bytes());
    bytes
}

pub fn build_set_request(name: &str, stype: SettingType, val: &SettingValue) -> Vec<u8> {
    let mut bytes = vec![0u8; 76];
    bytes[0..4].copy_from_slice(&SETTING_REQ_SET.to_ne_bytes());

    // Copy name
    let name_bytes = name.as_bytes();
    let copy_len = name_bytes.len().min(63);
    bytes[4..4 + copy_len].copy_from_slice(&name_bytes[..copy_len]);

    // Type
    let type_val = stype as i32;
    bytes[68..72].copy_from_slice(&type_val.to_ne_bytes());

    // Value
    match val {
        SettingValue::Bool(b) => {
            bytes[72] = if *b { 1 } else { 0 };
        }
        SettingValue::Int32(i) => {
            bytes[72..76].copy_from_slice(&i.to_ne_bytes());
        }
        SettingValue::Float(f) => {
            bytes[72..76].copy_from_slice(&f.to_ne_bytes());
        }
    }

    bytes
}

pub fn parse_setting_response(bytes: &[u8]) -> Result<(i32, Vec<SettingInfo>), String> {
    if bytes.len() < 12 {
        return Err("Response too short".into());
    }
    let magic = u32::from_ne_bytes(bytes[0..4].try_into().unwrap());
    let status = i32::from_ne_bytes(bytes[4..8].try_into().unwrap());
    let num_settings = i32::from_ne_bytes(bytes[8..12].try_into().unwrap());

    if magic == SETTING_REP_SET {
        return Ok((status, Vec::new()));
    }

    if magic != SETTING_REP_GET_ALL {
        return Err(format!("Invalid response magic: {:#X}", magic));
    }

    let mut settings = Vec::new();
    let expected_len = 12 + (num_settings as usize) * 72;
    if bytes.len() < expected_len {
        return Err(format!(
            "Response size {} smaller than expected {} for {} settings",
            bytes.len(),
            expected_len,
            num_settings
        ));
    }

    for i in 0..(num_settings as usize) {
        let offset = 12 + i * 72;
        let name_bytes = &bytes[offset..offset + 64];
        let name = parse_c_str(name_bytes)?;

        let type_val = i32::from_ne_bytes(bytes[offset + 64..offset + 68].try_into().unwrap());
        let stype = match type_val {
            0 => SettingType::Bool,
            1 => SettingType::Int32,
            2 => SettingType::Float,
            _ => return Err(format!("Unknown setting type {}", type_val)),
        };

        let val_bytes = &bytes[offset + 68..offset + 72];
        let value = match stype {
            SettingType::Bool => SettingValue::Bool(val_bytes[0] != 0),
            SettingType::Int32 => {
                SettingValue::Int32(i32::from_ne_bytes(val_bytes.try_into().unwrap()))
            }
            SettingType::Float => {
                SettingValue::Float(f32::from_ne_bytes(val_bytes.try_into().unwrap()))
            }
        };

        settings.push(SettingInfo {
            name,
            setting_type: stype,
            value,
        });
    }

    Ok((status, settings))
}

// Commands from TUI to Worker thread
pub enum WorkerCommand {
    Connect {
        url: String,
    },
    SelectScope {
        scope_id: usize,
        scope_type: ScopeType,
    },
    SelectGroup {
        members: Vec<(usize, ScopeType)>,
    },
    RequestSingleFrame,
    SetAutoCollect(bool),
    SetFilter {
        enabled: bool,
        cutoff: f32,
        percentage: f32,
    },
    RefreshScopes,
    GetSettings,
    UpdateSetting {
        name: String,
        setting_type: SettingType,
        value: SettingValue,
    },
}

// Events from Worker to TUI thread
pub enum WorkerEvent {
    Connecting,
    Connected {
        name: String,
        data_address: String,
        control_address: String,
        scopes: Vec<ScopeConfig>,
    },
    ConnectionFailed(String),
    NewData {
        scope_id: usize,
        msg: ScopeMessage,
    },
    Error(String),
    ScopesRefreshed {
        scopes: Vec<ScopeConfig>,
    },
    SettingsRefreshed {
        settings: Vec<SettingInfo>,
    },
    SettingUpdated {
        name: String,
        status: i32,
    },
}

pub fn run_worker(cmd_rx: Receiver<WorkerCommand>, event_tx: Sender<WorkerEvent>) {
    let mut data_socket: Option<Socket> = None;
    let mut control_socket: Option<Socket> = None;
    let mut selected_scopes: Vec<(usize, ScopeType)> = Vec::new();
    let mut scope_cycle_idx: usize = 0;
    let mut auto_collect = true;
    let mut announce_url: Option<String> = None;

    let mut filter_enabled = false;
    let mut filter_cutoff = 0.0f32;
    let mut filter_percentage = 50.0f32;

    loop {
        // 1. Process all pending commands with no blocking if auto collecting,
        // or blocking if idle.
        let has_work = auto_collect && !selected_scopes.is_empty() && data_socket.is_some();

        let cmd = if has_work {
            cmd_rx.try_recv().ok()
        } else {
            cmd_rx.recv().ok()
        };

        if let Some(command) = cmd {
            match command {
                WorkerCommand::Connect { url } => {
                    announce_url = Some(url.clone());
                    let _ = event_tx.send(WorkerEvent::Connecting);
                    match perform_announce(&url) {
                        Ok(ann) => {
                            let mut d_sock = None;
                            let mut c_sock = None;
                            match Socket::new(Protocol::Req0) {
                                Ok(sock) => match sock.dial(&ann.data_address) {
                                    Ok(_) => {
                                        let _ = sock
                                            .set_opt::<RecvTimeout>(Some(Duration::from_secs(1)));
                                        d_sock = Some(sock);
                                    }
                                    Err(e) => {
                                        let _ =
                                            event_tx.send(WorkerEvent::ConnectionFailed(format!(
                                                "Failed to dial data address {}: {}",
                                                ann.data_address, e
                                            )));
                                    }
                                },
                                Err(e) => {
                                    let _ = event_tx.send(WorkerEvent::ConnectionFailed(format!(
                                        "Failed to create data socket: {}",
                                        e
                                    )));
                                }
                            }

                            if d_sock.is_some() {
                                match Socket::new(Protocol::Req0) {
                                    Ok(sock) => match sock.dial(&ann.control_address) {
                                        Ok(_) => {
                                            let _ = sock.set_opt::<RecvTimeout>(Some(
                                                Duration::from_secs(1),
                                            ));
                                            c_sock = Some(sock);
                                        }
                                        Err(e) => {
                                            let _ = event_tx.send(WorkerEvent::Error(format!(
                                                "Failed to dial control address {}: {}",
                                                ann.control_address, e
                                            )));
                                        }
                                    },
                                    Err(e) => {
                                        let _ = event_tx.send(WorkerEvent::Error(format!(
                                            "Failed to create control socket: {}",
                                            e
                                        )));
                                    }
                                }
                            }

                            if d_sock.is_some() {
                                data_socket = d_sock;
                                control_socket = c_sock;
                                let _ = event_tx.send(WorkerEvent::Connected {
                                    name: ann.name,
                                    data_address: ann.data_address,
                                    control_address: ann.control_address,
                                    scopes: ann.scopes,
                                });
                                // Automatically fetch initial settings when connected
                                if let Some(ref sock) = control_socket {
                                    let req = build_get_all_request();
                                    if sock.send(&req).is_ok() {
                                        if let Ok(reply) = sock.recv() {
                                            if let Ok((_status, settings)) =
                                                parse_setting_response(&reply)
                                            {
                                                let _ =
                                                    event_tx.send(WorkerEvent::SettingsRefreshed {
                                                        settings,
                                                    });
                                            }
                                        }
                                    }
                                }
                            } else {
                                data_socket = None;
                                control_socket = None;
                            }
                        }
                        Err(e) => {
                            let _ = event_tx.send(WorkerEvent::ConnectionFailed(e));
                            data_socket = None;
                            control_socket = None;
                        }
                    }
                }
                WorkerCommand::SelectScope {
                    scope_id,
                    scope_type,
                } => {
                    selected_scopes = vec![(scope_id, scope_type)];
                    scope_cycle_idx = 0;
                }
                WorkerCommand::SelectGroup { members } => {
                    selected_scopes = members;
                    scope_cycle_idx = 0;
                }
                WorkerCommand::RequestSingleFrame => {
                    if let Some(sock) = &data_socket {
                        if let Some(&(scope_id, scope_type)) = selected_scopes.first() {
                            match fetch_data(sock, scope_id, scope_type) {
                                Ok(msg) => {
                                    if !filter_enabled
                                        || scope_type != ScopeType::IqData
                                        || check_noise_filter(
                                            &msg.real,
                                            &msg.imag,
                                            filter_cutoff,
                                            filter_percentage,
                                        )
                                    {
                                        let _ =
                                            event_tx.send(WorkerEvent::NewData { scope_id, msg });
                                    }
                                }
                                Err(e) => {
                                    let _ = event_tx
                                        .send(WorkerEvent::Error(format!("Fetch error: {}", e)));
                                }
                            }
                        }
                    }
                }
                WorkerCommand::SetAutoCollect(val) => {
                    auto_collect = val;
                }
                WorkerCommand::SetFilter {
                    enabled,
                    cutoff,
                    percentage,
                } => {
                    filter_enabled = enabled;
                    filter_cutoff = cutoff;
                    filter_percentage = percentage;
                }
                WorkerCommand::RefreshScopes => {
                    if let Some(ref url) = announce_url {
                        match perform_announce(url) {
                            Ok(ann) => {
                                let _ = event_tx
                                    .send(WorkerEvent::ScopesRefreshed { scopes: ann.scopes });
                                // Also fetch settings when scopes are refreshed
                                if let Some(ref sock) = control_socket {
                                    let req = build_get_all_request();
                                    if sock.send(&req).is_ok() {
                                        if let Ok(reply) = sock.recv() {
                                            if let Ok((_status, settings)) =
                                                parse_setting_response(&reply)
                                            {
                                                let _ =
                                                    event_tx.send(WorkerEvent::SettingsRefreshed {
                                                        settings,
                                                    });
                                            }
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                let _ = event_tx.send(WorkerEvent::Error(format!(
                                    "Failed to refresh scopes: {}",
                                    e
                                )));
                            }
                        }
                    }
                }
                WorkerCommand::GetSettings => {
                    if let Some(ref sock) = control_socket {
                        let req = build_get_all_request();
                        match sock.send(&req) {
                            Ok(_) => match sock.recv() {
                                Ok(reply) => match parse_setting_response(&reply) {
                                    Ok((_status, settings)) => {
                                        let _ = event_tx
                                            .send(WorkerEvent::SettingsRefreshed { settings });
                                    }
                                    Err(e) => {
                                        let _ = event_tx.send(WorkerEvent::Error(format!(
                                            "Failed to parse settings: {}",
                                            e
                                        )));
                                    }
                                },
                                Err(e) => {
                                    let _ = event_tx.send(WorkerEvent::Error(format!(
                                        "Failed to receive settings: {}",
                                        e
                                    )));
                                }
                            },
                            Err(e) => {
                                let _ = event_tx.send(WorkerEvent::Error(format!(
                                    "Failed to send settings request: {}",
                                    e.1
                                )));
                            }
                        }
                    }
                }
                WorkerCommand::UpdateSetting {
                    name,
                    setting_type,
                    value,
                } => {
                    if let Some(ref sock) = control_socket {
                        let req = build_set_request(&name, setting_type, &value);
                        match sock.send(&req) {
                            Ok(_) => match sock.recv() {
                                Ok(reply) => match parse_setting_response(&reply) {
                                    Ok((status, _)) => {
                                        let _ = event_tx
                                            .send(WorkerEvent::SettingUpdated { name, status });
                                    }
                                    Err(e) => {
                                        let _ = event_tx.send(WorkerEvent::Error(format!(
                                            "Failed to parse update reply: {}",
                                            e
                                        )));
                                    }
                                },
                                Err(e) => {
                                    let _ = event_tx.send(WorkerEvent::Error(format!(
                                        "Failed to receive update reply: {}",
                                        e
                                    )));
                                }
                            },
                            Err(e) => {
                                let _ = event_tx.send(WorkerEvent::Error(format!(
                                    "Failed to send update request: {}",
                                    e.1
                                )));
                            }
                        }
                    }
                }
            }
        }

        // 2. If auto_collect is enabled, pull data frame (round-robin for groups).
        if auto_collect && !selected_scopes.is_empty() {
            if let Some(sock) = &data_socket {
                if scope_cycle_idx >= selected_scopes.len() {
                    scope_cycle_idx = 0;
                }
                let (scope_id, scope_type) = selected_scopes[scope_cycle_idx];
                scope_cycle_idx = (scope_cycle_idx + 1) % selected_scopes.len();

                match fetch_data(sock, scope_id, scope_type) {
                    Ok(msg) => {
                        if !filter_enabled
                            || scope_type != ScopeType::IqData
                            || check_noise_filter(
                                &msg.real,
                                &msg.imag,
                                filter_cutoff,
                                filter_percentage,
                            )
                        {
                            let _ = event_tx.send(WorkerEvent::NewData { scope_id, msg });
                        }
                    }
                    Err(e) => {
                        if !e.contains("TimedOut") {
                            let _ =
                                event_tx.send(WorkerEvent::Error(format!("Fetch error: {}", e)));
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }
            }
        }

        // Minor sleep to prevent cpu hogging when we have work.
        if has_work {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn perform_announce(url: &str) -> Result<AnnounceResponse, String> {
    let req_sock =
        Socket::new(Protocol::Req0).map_err(|e| format!("Failed to create req socket: {}", e))?;

    req_sock
        .dial(url)
        .map_err(|e| format!("Failed to dial announce URL: {}", e))?;

    req_sock
        .set_opt::<RecvTimeout>(Some(Duration::from_secs(2)))
        .map_err(|e| format!("Failed to set timeout: {}", e))?;

    let magic_bytes = ANNOUNCE_MSG_ID.to_ne_bytes();
    let req_msg = nng::Message::from(&magic_bytes[..]);

    req_sock
        .send(req_msg)
        .map_err(|e| format!("Failed to send announce request: {:?}", e))?;

    let res_msg = req_sock
        .recv()
        .map_err(|e| format!("Failed to receive announce response: {}", e))?;

    parse_announce_response(&res_msg)
}

fn fetch_data(
    sock: &Socket,
    scope_id: usize,
    scope_type: ScopeType,
) -> Result<ScopeMessage, String> {
    let req = ScopeRequest {
        magic: SCOPE_REQ_MSG_ID,
        scope_id: scope_id as i32,
    };

    let bytes = unsafe {
        std::slice::from_raw_parts(
            &req as *const ScopeRequest as *const u8,
            std::mem::size_of::<ScopeRequest>(),
        )
    };

    let req_msg = nng::Message::from(bytes);
    sock.send(req_msg)
        .map_err(|e| format!("Send failed: {:?}", e))?;

    let res_msg = sock.recv().map_err(|e| format!("Recv failed: {:?}", e))?;

    parse_scope_message(&res_msg, scope_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_announce() {
        // scope_size = 64 (name) + 64 (group) + 4 (type) = 132
        let mut bytes = vec![0u8; 388 + 132];
        // Populate data_address
        bytes[0..13].copy_from_slice(b"tcp://127.0.1");
        // Populate control_address
        bytes[128..141].copy_from_slice(b"tcp://127.0.2");
        // Populate name
        bytes[256..266].copy_from_slice(b"test_proto");
        // Populate num_scopes = 1
        let num_scopes: i32 = 1;
        bytes[384..388].copy_from_slice(&num_scopes.to_ne_bytes());
        // Populate scope name at offset 388
        bytes[388..397].copy_from_slice(b"TestScope");
        // group field is all zeros (empty string) at offset 388+64=452
        // Populate scope type = 1 (IqData) at offset 388+128=516
        let scope_type: i32 = 1;
        bytes[388 + 128..388 + 132].copy_from_slice(&scope_type.to_ne_bytes());

        let res = parse_announce_response(&bytes).unwrap();
        assert_eq!(res.name, "test_proto");
        assert_eq!(res.data_address, "tcp://127.0.1");
        assert_eq!(res.control_address, "tcp://127.0.2");
        assert_eq!(res.scopes.len(), 1);
        assert_eq!(res.scopes[0].name, "TestScope");
        assert_eq!(res.scopes[0].group, "");
        assert_eq!(res.scopes[0].scope_type, ScopeType::IqData);
    }
}

#[cfg(test)]
mod bench_tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn benchmark_read_scope_msg() {
        let mut snap = IQSnapshot::new(1);
        let msg = ScopeMessage {
            meta: NRmetadata::default(),
            time_taken_in_ns: 0,
            id: 1,
            data_size: 16000 * 8,
            real: vec![0.5; 16000],
            imag: vec![0.5; 16000],
        };

        let iterations = 10000;

        let start = Instant::now();
        for _ in 0..iterations {
            snap.read_scope_msg(&msg, false);
        }
        let duration = start.elapsed();

        println!("Baseline duration: {:?}", duration);
    }
}
