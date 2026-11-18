//! Mirrors oniko-web/packages/api/src/telemetry/contract.ts; the server rejects anything else.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub mod limits {
    pub const EVENTS: usize = 500;
    pub const ERRORS: usize = 100;
    pub const MESSAGE: usize = 2000;
    pub const STACK: usize = 16_000;
    pub const NAME: usize = 128;
    pub const TEXT: usize = 256;
    pub const PROPS: usize = 32;
    pub const KEY: usize = 64;
    pub const EVENT_NAME: usize = 64;
}

/// String, number, boolean or null.
pub type Props = BTreeMap<String, serde_json::Value>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelemetryEvent {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub props: Option<Props>,
    pub at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorSource {
    Main,
    Renderer,
    Ipc,
    Process,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelemetryError {
    pub source: ErrorSource,
    pub name: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<Props>,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OsInfo {
    pub platform: String,
    pub release: String,
    pub version: String,
    pub arch: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CpuInfo {
    pub model: String,
    pub cores: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_mhz: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayInfo {
    pub width: i64,
    pub height: i64,
    pub scale_factor: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_rate: Option<f64>,
}

/// The server's schema still names the Electron runtime; this app reports its own version there
/// and leaves the browser fields empty.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppInfo {
    pub version: String,
    pub electron: String,
    pub chrome: String,
    pub node: String,
    pub packaged: bool,
}

/// Hardware and runtime facts only: no hostname, username, paths, addresses or serials.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub os: OsInfo,
    pub cpu: CpuInfo,
    pub memory_bytes: u64,
    pub gpus: Vec<GpuInfo>,
    pub displays: Vec<DisplayInfo>,
    pub locale: String,
    pub app: AppInfo,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegisterOutput {
    pub token: String,
}
