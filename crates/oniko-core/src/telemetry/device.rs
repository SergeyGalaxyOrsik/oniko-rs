//! The device id and hardware facts telemetry registers with. Names follow Node's `os` module
//! (`darwin`, `arm64`, `x64`), which is what the server has seen so far.

use std::time::Duration;

use sha2::{Digest, Sha256};

use super::client::hex;
use super::contract::{AppInfo, CpuInfo, DeviceInfo, DisplayInfo, OsInfo};
use super::scrub::truncate;

pub fn parse_ioreg(out: &str) -> Option<String> {
    let re = regex::Regex::new(r#""IOPlatformUUID"\s*=\s*"([^"]+)""#).expect("valid regex");
    re.captures(out).map(|c| c[1].to_owned())
}

pub fn parse_reg_query(out: &str) -> Option<String> {
    let re = regex::RegexBuilder::new(r"MachineGuid\s+REG_SZ\s+(\S+)").case_insensitive(true).build().expect("valid regex");
    re.captures(out).map(|c| c[1].to_owned())
}

pub fn parse_machine_id(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// The raw hardware id never leaves the machine, only this salted digest.
pub fn hash_device_id(raw: &str) -> String {
    hex(&Sha256::digest(format!("oniko-telemetry:{raw}").as_bytes()))
}

async fn run(program: &str, args: &[&str]) -> Option<String> {
    let output = tokio::process::Command::new(program).args(args).kill_on_drop(true).output();
    let out = tokio::time::timeout(Duration::from_secs(3), output).await.ok()?.ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

pub async fn read_hardware_id() -> Option<String> {
    if cfg!(target_os = "macos") {
        parse_ioreg(&run("ioreg", &["-rd1", "-c", "IOPlatformExpertDevice"]).await?)
    } else if cfg!(windows) {
        parse_reg_query(&run("reg", &["query", r"HKLM\SOFTWARE\Microsoft\Cryptography", "/v", "MachineGuid", "/reg:64"]).await?)
    } else {
        for path in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
            if let Some(id) = tokio::fs::read_to_string(path).await.ok().and_then(|t| parse_machine_id(&t)) {
                return Some(id);
            }
        }
        None
    }
}

fn platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        "x86" => "ia32",
        other => other,
    }
}

async fn sysctl(name: &str) -> Option<String> {
    run("sysctl", &["-n", name]).await.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

async fn cpu_model() -> String {
    if cfg!(target_os = "macos") {
        return sysctl("machdep.cpu.brand_string").await.unwrap_or_else(|| "unknown".into());
    }
    let info = tokio::fs::read_to_string("/proc/cpuinfo").await.unwrap_or_default();
    info.lines()
        .find(|l| l.starts_with("model name"))
        .and_then(|l| l.split(':').nth(1))
        .map(|m| m.trim().to_owned())
        .unwrap_or_else(|| "unknown".into())
}

async fn memory_bytes() -> u64 {
    if cfg!(target_os = "macos") {
        return sysctl("hw.memsize").await.and_then(|m| m.parse().ok()).unwrap_or(0);
    }
    let info = tokio::fs::read_to_string("/proc/meminfo").await.unwrap_or_default();
    info.lines()
        .find(|l| l.starts_with("MemTotal"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|kb| kb.parse::<u64>().ok())
        .map_or(0, |kb| kb * 1024)
}

async fn locale() -> String {
    if cfg!(target_os = "macos")
        && let Some(locale) = run("defaults", &["read", "-g", "AppleLocale"]).await
    {
        return locale.trim().split('@').next().unwrap_or("").replace('_', "-");
    }
    std::env::var("LANG").ok().and_then(|l| l.split('.').next().map(|l| l.replace('_', "-"))).filter(|l| !l.is_empty() && l != "C").unwrap_or_else(|| "en-US".into())
}

/// Hardware and runtime facts only: no hostname, username, paths, addresses or serials.
pub async fn collect_device_info(app_version: &str, displays: Vec<DisplayInfo>) -> DeviceInfo {
    let text = |value: String| truncate(&value, 256);
    let release = run("uname", &["-r"]).await.unwrap_or_default().trim().to_owned();
    let version = run("uname", &["-v"]).await.unwrap_or_default().trim().to_owned();
    DeviceInfo {
        os: OsInfo { platform: platform().into(), release: text(release), version: text(version), arch: arch().into() },
        cpu: CpuInfo {
            model: text(cpu_model().await),
            cores: std::thread::available_parallelism().map_or(0, |n| n.get() as u32),
            speed_mhz: None,
        },
        memory_bytes: memory_bytes().await,
        gpus: vec![],
        displays: displays.into_iter().take(8).collect(),
        locale: text(locale().await),
        app: AppInfo {
            version: text(app_version.to_owned()),
            electron: String::new(),
            chrome: String::new(),
            node: String::new(),
            packaged: !cfg!(debug_assertions),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ioreg_reg_query_and_machine_id_output() {
        assert_eq!(parse_ioreg("  \"IOPlatformSerialNumber\" = \"C02X\"\n  \"IOPlatformUUID\" = \"1A2B-3C4D\"\n").as_deref(), Some("1A2B-3C4D"));
        assert_eq!(parse_ioreg("nothing"), None);
        assert_eq!(
            parse_reg_query("\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Cryptography\r\n    MachineGuid    REG_SZ    abcd-ef01\r\n").as_deref(),
            Some("abcd-ef01")
        );
        assert_eq!(parse_machine_id("0123abcd\n").as_deref(), Some("0123abcd"));
        assert_eq!(parse_machine_id("\n"), None);
    }

    #[test]
    fn hashes_with_the_salt_into_64_hex() {
        let id = hash_device_id("1A2B-3C4D");
        assert_eq!(id.len(), 64);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(id, hash_device_id("1A2B-3C4D"));
        assert_ne!(id, hash_device_id("1A2B-3C4E"));
    }
}
