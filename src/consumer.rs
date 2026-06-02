/*
 * Copyright (c) 2025-2026 Bartosz Podrygajlo
 *
 * Licensed under the MIT License.
 * See LICENSE file in the project root for full license information.
 */

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
            let mut gap = 0usize;
            if current_size > 0 && msg.meta.timestamp > self.current_timestamp {
                // Clamp gap to max_stacked_size to prevent unbounded allocation on
                // a large or corrupted timestamp jump.
                gap = ((msg.meta.timestamp - self.current_timestamp) as usize)
                    .min(self.max_stacked_size);
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

            self.current_timestamp = msg.meta.timestamp.saturating_add(num_samples as u64);

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

pub fn parse_scope_message(bytes: &[u8], scope_type: ScopeType) -> Result<ScopeMessage, String> {
    if bytes.len() < 48 {
        return Err("Scope message too short for header (minimum 48 bytes)".into());
    }

    let frame = u32::from_ne_bytes(bytes[0..4].try_into().unwrap());
    let slot = u32::from_ne_bytes(bytes[4..8].try_into().unwrap());
    let timestamp = u64::from_ne_bytes(bytes[8..16].try_into().unwrap());
    let time_taken_in_ns = u64::from_ne_bytes(bytes[16..24].try_into().unwrap());
    let id = i32::from_ne_bytes(bytes[24..28].try_into().unwrap());
    // bytes[28..32] are C struct padding inserted by the compiler to align size_t data_size
    // to an 8-byte boundary (scope_msg_t: NRmetadata(16) + time_taken(8) + int id(4) + pad(4) + size_t(8)).
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
            match cmd_rx.try_recv() {
                Ok(cmd) => Some(cmd),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
            }
        } else {
            match cmd_rx.recv() {
                Ok(cmd) => Some(cmd),
                Err(_) => return, // all senders dropped; exit cleanly
            }
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
    // Explicit serialization mirrors scope_request_t { uint32_t magic; int32_t scope_id; }.
    let mut bytes = [0u8; 8];
    bytes[0..4].copy_from_slice(&SCOPE_REQ_MSG_ID.to_ne_bytes());
    bytes[4..8].copy_from_slice(&(scope_id as i32).to_ne_bytes());
    let req_msg = nng::Message::from(&bytes[..]);
    sock.send(req_msg)
        .map_err(|e| format!("Send failed: {:?}", e))?;

    let res_msg = sock.recv().map_err(|e| format!("Recv failed: {:?}", e))?;

    parse_scope_message(&res_msg, scope_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn make_scope_msg_bytes(
        frame: u32,
        slot: u32,
        timestamp: u64,
        time_ns: u64,
        id: i32,
        data: &[u8],
    ) -> Vec<u8> {
        // Mirrors scope_msg_t layout: meta(16) + time_taken(8) + id(4) + pad(4) + data_size(8) + data
        let mut bytes = vec![0u8; 48 + data.len()];
        bytes[0..4].copy_from_slice(&frame.to_ne_bytes());
        bytes[4..8].copy_from_slice(&slot.to_ne_bytes());
        bytes[8..16].copy_from_slice(&timestamp.to_ne_bytes());
        bytes[16..24].copy_from_slice(&time_ns.to_ne_bytes());
        bytes[24..28].copy_from_slice(&id.to_ne_bytes());
        // bytes[28..32] = C struct padding, left zero
        bytes[32..40].copy_from_slice(&(data.len() as u64).to_ne_bytes());
        bytes[48..].copy_from_slice(data);
        bytes
    }

    fn make_announce_bytes(
        data_addr: &str,
        ctrl_addr: &str,
        name: &str,
        scopes: &[(&str, &str, i32)],
    ) -> Vec<u8> {
        let scope_size = 132usize;
        let mut bytes = vec![0u8; 388 + scopes.len() * scope_size];
        let copy_str = |dst: &mut [u8], s: &str| {
            let b = s.as_bytes();
            let n = b.len().min(dst.len() - 1);
            dst[..n].copy_from_slice(&b[..n]);
        };
        copy_str(&mut bytes[0..128], data_addr);
        copy_str(&mut bytes[128..256], ctrl_addr);
        copy_str(&mut bytes[256..384], name);
        bytes[384..388].copy_from_slice(&(scopes.len() as i32).to_ne_bytes());
        for (i, &(sname, sgroup, stype)) in scopes.iter().enumerate() {
            let off = 388 + i * scope_size;
            copy_str(&mut bytes[off..off + 64], sname);
            copy_str(&mut bytes[off + 64..off + 128], sgroup);
            bytes[off + 128..off + 132].copy_from_slice(&stype.to_ne_bytes());
        }
        bytes
    }

    // ── parse_announce_response ──────────────────────────────────────────────

    #[test]
    fn test_parse_announce_basic() {
        let bytes = make_announce_bytes(
            "tcp://127.0.1",
            "tcp://127.0.2",
            "test_proto",
            &[("TestScope", "", 1)],
        );
        let res = parse_announce_response(&bytes).unwrap();
        assert_eq!(res.name, "test_proto");
        assert_eq!(res.data_address, "tcp://127.0.1");
        assert_eq!(res.control_address, "tcp://127.0.2");
        assert_eq!(res.scopes.len(), 1);
        assert_eq!(res.scopes[0].name, "TestScope");
        assert_eq!(res.scopes[0].group, "");
        assert_eq!(res.scopes[0].scope_type, ScopeType::IqData);
    }

    #[test]
    fn test_parse_announce_multiple_scopes() {
        let bytes = make_announce_bytes(
            "tcp://d",
            "tcp://c",
            "proto",
            &[
                ("ScopeA", "grp1", 0),
                ("ScopeB", "grp1", 1),
                ("ScopeC", "", 2),
                ("ScopeD", "", 3),
            ],
        );
        let res = parse_announce_response(&bytes).unwrap();
        assert_eq!(res.scopes.len(), 4);
        assert_eq!(res.scopes[0].scope_type, ScopeType::Real);
        assert_eq!(res.scopes[0].group, "grp1");
        assert_eq!(res.scopes[1].scope_type, ScopeType::IqData);
        assert_eq!(res.scopes[2].scope_type, ScopeType::Int32);
        assert_eq!(res.scopes[3].scope_type, ScopeType::Float);
    }

    #[test]
    fn test_parse_announce_too_short() {
        assert!(parse_announce_response(&[0u8; 10]).is_err());
        assert!(parse_announce_response(&[0u8; 387]).is_err());
    }

    #[test]
    fn test_parse_announce_negative_scopes() {
        let mut bytes = vec![0u8; 388];
        bytes[384..388].copy_from_slice(&(-1i32).to_ne_bytes());
        assert!(parse_announce_response(&bytes).is_err());
    }

    #[test]
    fn test_parse_announce_truncated_scope_data() {
        // Claims 2 scopes but only has room for 1
        let mut bytes = make_announce_bytes("d", "c", "n", &[("A", "", 0)]);
        bytes[384..388].copy_from_slice(&2i32.to_ne_bytes()); // lie about scope count
        assert!(parse_announce_response(&bytes).is_err());
    }

    #[test]
    fn test_parse_announce_unknown_scope_type() {
        let bytes = make_announce_bytes("d", "c", "n", &[("A", "", 99)]);
        assert!(parse_announce_response(&bytes).is_err());
    }

    // ── parse_scope_message ──────────────────────────────────────────────────

    #[test]
    fn test_parse_scope_message_real() {
        let samples: Vec<i16> = vec![100, -200, 300];
        let data: Vec<u8> = samples.iter().flat_map(|v| v.to_ne_bytes()).collect();
        let bytes = make_scope_msg_bytes(10, 2, 1000, 500, 3, &data);
        let msg = parse_scope_message(&bytes, ScopeType::Real).unwrap();
        assert_eq!(msg.meta.frame, 10);
        assert_eq!(msg.meta.slot, 2);
        assert_eq!(msg.meta.timestamp, 1000);
        assert_eq!(msg.time_taken_in_ns, 500);
        assert_eq!(msg.id, 3);
        assert_eq!(msg.real, vec![100.0, -200.0, 300.0]);
        assert!(msg.imag.is_empty());
    }

    #[test]
    fn test_parse_scope_message_iq() {
        let pairs: Vec<i16> = vec![10, 20, -30, 40]; // r1, im1, r2, im2
        let data: Vec<u8> = pairs.iter().flat_map(|v| v.to_ne_bytes()).collect();
        let bytes = make_scope_msg_bytes(1, 0, 0, 0, 0, &data);
        let msg = parse_scope_message(&bytes, ScopeType::IqData).unwrap();
        assert_eq!(msg.real, vec![10.0, -30.0]);
        assert_eq!(msg.imag, vec![20.0, 40.0]);
    }

    #[test]
    fn test_parse_scope_message_int32() {
        let vals: Vec<i32> = vec![100000, -200000];
        let data: Vec<u8> = vals.iter().flat_map(|v| v.to_ne_bytes()).collect();
        let bytes = make_scope_msg_bytes(0, 0, 0, 0, 0, &data);
        let msg = parse_scope_message(&bytes, ScopeType::Int32).unwrap();
        assert_eq!(msg.real, vec![100000.0, -200000.0]);
        assert!(msg.imag.is_empty());
    }

    #[test]
    fn test_parse_scope_message_float() {
        let vals: Vec<f32> = vec![1.5, -2.5];
        let data: Vec<u8> = vals.iter().flat_map(|v| v.to_ne_bytes()).collect();
        let bytes = make_scope_msg_bytes(0, 0, 0, 0, 0, &data);
        let msg = parse_scope_message(&bytes, ScopeType::Float).unwrap();
        assert!((msg.real[0] - 1.5).abs() < 1e-6);
        assert!((msg.real[1] - (-2.5)).abs() < 1e-6);
    }

    #[test]
    fn test_parse_scope_message_too_short() {
        assert!(parse_scope_message(&[0u8; 10], ScopeType::Real).is_err());
        assert!(parse_scope_message(&[0u8; 47], ScopeType::Real).is_err());
    }

    #[test]
    fn test_parse_scope_message_data_size_mismatch() {
        // header claims data_size = 100, but payload only has 4 bytes
        let data = vec![0u8; 4];
        let mut bytes = make_scope_msg_bytes(0, 0, 0, 0, 0, &data);
        // overwrite data_size with 100
        bytes[32..40].copy_from_slice(&100u64.to_ne_bytes());
        assert!(parse_scope_message(&bytes, ScopeType::Real).is_err());
    }

    // ── check_noise_filter ───────────────────────────────────────────────────

    #[test]
    fn test_check_noise_filter_empty() {
        // Empty slices pass the filter (nothing to reject).
        assert!(check_noise_filter(&[], &[], 100.0, 50.0));
    }

    #[test]
    fn test_check_noise_filter_all_noise() {
        // All zeros → 100% noise, cutoff 50% → reject.
        let r = vec![0.0f64; 10];
        let i = vec![0.0f64; 10];
        assert!(!check_noise_filter(&r, &i, 1.0, 50.0));
    }

    #[test]
    fn test_check_noise_filter_good_signal() {
        // All large values → 0% noise → pass.
        let r = vec![1000.0f64; 10];
        let i = vec![1000.0f64; 10];
        assert!(check_noise_filter(&r, &i, 100.0, 50.0));
    }

    #[test]
    fn test_check_noise_filter_exactly_at_threshold() {
        // 5 out of 10 samples below cutoff = exactly 50% noise → passes (<=).
        let mut r = vec![1000.0f64; 10];
        r[0..5].fill(0.0);
        let i = vec![0.0f64; 10];
        assert!(check_noise_filter(&r, &i, 1.0, 50.0));
    }

    // ── build / parse settings ───────────────────────────────────────────────

    #[test]
    fn test_build_get_all_request() {
        let bytes = build_get_all_request();
        assert_eq!(bytes.len(), 76);
        let magic = u32::from_ne_bytes(bytes[0..4].try_into().unwrap());
        assert_eq!(magic, SETTING_REQ_GET_ALL);
    }

    #[test]
    fn test_build_set_request_bool() {
        let bytes = build_set_request("flag", SettingType::Bool, &SettingValue::Bool(true));
        assert_eq!(bytes.len(), 76);
        let magic = u32::from_ne_bytes(bytes[0..4].try_into().unwrap());
        assert_eq!(magic, SETTING_REQ_SET);
        assert_eq!(&bytes[4..8], b"flag");
        let type_val = i32::from_ne_bytes(bytes[68..72].try_into().unwrap());
        assert_eq!(type_val, SettingType::Bool as i32);
        assert_eq!(bytes[72], 1);
    }

    #[test]
    fn test_build_set_request_int32() {
        let bytes = build_set_request("count", SettingType::Int32, &SettingValue::Int32(42));
        let val = i32::from_ne_bytes(bytes[72..76].try_into().unwrap());
        assert_eq!(val, 42);
    }

    #[test]
    fn test_build_set_request_float() {
        let bytes = build_set_request("gain", SettingType::Float, &SettingValue::Float(3.14));
        let val = f32::from_ne_bytes(bytes[72..76].try_into().unwrap());
        assert!((val - 3.14f32).abs() < 1e-5);
    }

    #[test]
    fn test_build_set_request_name_truncation() {
        let long_name = "a".repeat(100);
        let bytes = build_set_request(&long_name, SettingType::Bool, &SettingValue::Bool(false));
        // Name field is bytes[4..68] (64 bytes). copy_len = min(100, 63) = 63.
        // Bytes 4..67 (indices 4..=66) contain 'a'; byte 67 is the implicit NUL.
        assert_eq!(bytes.len(), 76);
        assert_eq!(bytes[66], b'a'); // last copied byte
        assert_eq!(bytes[67], 0); // NUL terminator preserved
    }

    fn make_setting_response(
        magic: u32,
        status: i32,
        settings: &[(&str, i32, [u8; 4])],
    ) -> Vec<u8> {
        let mut bytes = vec![0u8; 12 + settings.len() * 72];
        bytes[0..4].copy_from_slice(&magic.to_ne_bytes());
        bytes[4..8].copy_from_slice(&status.to_ne_bytes());
        bytes[8..12].copy_from_slice(&(settings.len() as i32).to_ne_bytes());
        for (i, &(name, stype, val)) in settings.iter().enumerate() {
            let off = 12 + i * 72;
            let nb = name.as_bytes();
            let n = nb.len().min(63);
            bytes[off..off + n].copy_from_slice(&nb[..n]);
            bytes[off + 64..off + 68].copy_from_slice(&stype.to_ne_bytes());
            bytes[off + 68..off + 72].copy_from_slice(&val);
        }
        bytes
    }

    #[test]
    fn test_parse_setting_response_get_all() {
        let val_bytes: [u8; 4] = 7i32.to_ne_bytes();
        let bytes = make_setting_response(SETTING_REP_GET_ALL, 0, &[("count", 1, val_bytes)]);
        let (status, settings) = parse_setting_response(&bytes).unwrap();
        assert_eq!(status, 0);
        assert_eq!(settings.len(), 1);
        assert_eq!(settings[0].name, "count");
        assert_eq!(settings[0].setting_type, SettingType::Int32);
        assert_eq!(settings[0].value, SettingValue::Int32(7));
    }

    #[test]
    fn test_parse_setting_response_set_reply() {
        let bytes = make_setting_response(SETTING_REP_SET, 0, &[]);
        let (status, settings) = parse_setting_response(&bytes).unwrap();
        assert_eq!(status, 0);
        assert!(settings.is_empty());
    }

    #[test]
    fn test_parse_setting_response_invalid_magic() {
        let bytes = make_setting_response(0xDEADBEEF, 0, &[]);
        assert!(parse_setting_response(&bytes).is_err());
    }

    #[test]
    fn test_parse_setting_response_too_short() {
        assert!(parse_setting_response(&[0u8; 5]).is_err());
    }

    // ── IQSnapshot ───────────────────────────────────────────────────────────

    #[test]
    fn test_iqsnapshot_preprocess_empty() {
        let mut snap = IQSnapshot::new(1);
        snap.preprocess();
        assert_eq!(snap.min_val, 0.0);
        assert_eq!(snap.max_val, 0.0);
        assert_eq!(snap.max_iq, 0.0);
        assert_eq!(snap.max_power, 0.0);
        assert_eq!(snap.nonzero_count, 0);
    }

    #[test]
    fn test_iqsnapshot_preprocess_iq() {
        let mut snap = IQSnapshot::new(1);
        snap.real = vec![3.0, -4.0, 5.0];
        snap.imag = vec![4.0, 3.0, -5.0];
        snap.preprocess();
        assert_eq!(snap.max_iq, 5.0);
        assert_eq!(snap.min_val, -4.0);
        assert_eq!(snap.max_val, 5.0);
        assert_eq!(snap.max_power, 50.0f32); // 5^2 + (-5)^2 = 50
        assert_eq!(snap.nonzero_count, 3);
        assert_eq!(snap.power.len(), 3);
    }

    #[test]
    fn test_iqsnapshot_preprocess_real_only() {
        let mut snap = IQSnapshot::new(1);
        snap.real = vec![10.0, -5.0, 0.0];
        snap.preprocess();
        assert_eq!(snap.max_iq, 10.0);
        assert_eq!(snap.min_val, -5.0);
        assert_eq!(snap.max_val, 10.0);
        assert_eq!(snap.nonzero_count, 2); // 0.0 power is not nonzero
    }

    fn make_msg(real: Vec<f64>, imag: Vec<f64>, timestamp: u64) -> ScopeMessage {
        ScopeMessage {
            meta: NRmetadata {
                frame: 0,
                slot: 0,
                timestamp,
            },
            time_taken_in_ns: 0,
            id: 1,
            data_size: (real.len() * 8) as u64,
            real,
            imag,
        }
    }

    #[test]
    fn test_iqsnapshot_read_no_collect() {
        let mut snap = IQSnapshot::new(1);
        let msg = make_msg(vec![1.0, 2.0, 3.0], vec![], 0);
        snap.read_scope_msg(&msg, false);
        assert_eq!(snap.real, vec![1.0, 2.0, 3.0]);
        assert_eq!(snap.max_val, 3.0);
        assert_eq!(snap.min_val, 1.0);
    }

    #[test]
    fn test_iqsnapshot_read_no_collect_replaces() {
        let mut snap = IQSnapshot::new(1);
        snap.read_scope_msg(&make_msg(vec![1.0, 2.0], vec![], 0), false);
        snap.read_scope_msg(&make_msg(vec![9.0, 10.0], vec![], 5), false);
        assert_eq!(snap.real, vec![9.0, 10.0]); // second read replaces first
    }

    #[test]
    fn test_iqsnapshot_read_collect_appends() {
        let mut snap = IQSnapshot::new(1);
        snap.max_stacked_size = 1000;
        snap.read_scope_msg(&make_msg(vec![1.0, 2.0], vec![], 0), true);
        // timestamp advances to 0 + 2 = 2
        snap.read_scope_msg(&make_msg(vec![3.0, 4.0], vec![], 2), true);
        assert_eq!(snap.real, vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn test_iqsnapshot_read_collect_gap() {
        let mut snap = IQSnapshot::new(1);
        snap.max_stacked_size = 1000;
        snap.read_scope_msg(&make_msg(vec![1.0], vec![], 0), true);
        // timestamp = 0 + 1 = 1; next msg starts at 3 → gap = 2
        snap.read_scope_msg(&make_msg(vec![9.0], vec![], 3), true);
        assert_eq!(snap.real.len(), 1 + 2 + 1); // original + gap + new
        assert_eq!(snap.real[0], 1.0);
        assert_eq!(snap.real[1], 0.0); // gap zero
        assert_eq!(snap.real[2], 0.0); // gap zero
        assert_eq!(snap.real[3], 9.0);
    }

    #[test]
    fn test_iqsnapshot_collect_truncates_at_max() {
        let mut snap = IQSnapshot::new(1);
        snap.max_stacked_size = 5;
        // Fill to 4
        snap.read_scope_msg(&make_msg(vec![1.0, 2.0, 3.0, 4.0], vec![], 0), true);
        // Add 3 more → total 7 → truncate to 5
        snap.read_scope_msg(&make_msg(vec![5.0, 6.0, 7.0], vec![], 4), true);
        assert_eq!(snap.real.len(), 5);
        // oldest samples removed; newest at the end
        assert_eq!(*snap.real.last().unwrap(), 7.0);
    }

    #[test]
    fn test_iqsnapshot_collect_gap_clamped() {
        // A huge timestamp jump must not cause an OOM or panic.
        let mut snap = IQSnapshot::new(1);
        snap.max_stacked_size = 50;
        snap.read_scope_msg(&make_msg(vec![1.0], vec![], 0), true);
        // Gap would be u64::MAX - 1 ≈ 1.8e19 samples; must be clamped.
        snap.read_scope_msg(&make_msg(vec![2.0], vec![], u64::MAX), true);
        assert!(snap.real.len() <= snap.max_stacked_size);
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

        let start = Instant::now();
        for _ in 0..10_000 {
            snap.read_scope_msg(&msg, false);
        }
        println!("Baseline duration: {:?}", start.elapsed());
    }
}
