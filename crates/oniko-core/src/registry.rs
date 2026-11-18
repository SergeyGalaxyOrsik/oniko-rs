//! How each agent is launched and how its logins are found and checked.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures::future::BoxFuture;
use serde::Deserialize;

use crate::model::{AccountStatus, AgentId};
use crate::util::{home_dir, now_ms, resolve};

/// A `None` value removes the variable from the child env rather than inheriting it.
pub type EnvLayer = BTreeMap<String, Option<String>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnSpec {
    pub command: String,
    pub args: Vec<String>,
    pub env: EnvLayer,
}

pub fn child_env<'a>(base: impl IntoIterator<Item = (String, String)>, layers: impl IntoIterator<Item = &'a EnvLayer>) -> BTreeMap<String, String> {
    let mut merged: BTreeMap<String, Option<String>> = base.into_iter().map(|(k, v)| (k, Some(v))).collect();
    for layer in layers {
        merged.extend(layer.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    merged.into_iter().filter_map(|(k, v)| v.map(|v| (k, v))).collect()
}

/// The environment children start from: this process's, with the PATH a login shell would have.
/// An app opened from Finder gets a minimal PATH without Homebrew, nvm or ~/.local/bin.
pub fn base_env() -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars().filter(|(k, _)| k != "PATH").collect();
    env.push(("PATH".into(), shell_path().to_owned()));
    env
}

/// Read once, from an interactive login shell so version managers that hook `.zshrc` count too.
pub fn shell_path() -> &'static str {
    static PATH: OnceLock<String> = OnceLock::new();
    PATH.get_or_init(|| {
        let inherited = std::env::var("PATH").unwrap_or_default();
        if cfg!(windows) {
            return inherited;
        }
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let output = std::process::Command::new(&shell)
            .args(["-l", "-i", "-c", "printf '__ONIKO_PATH__%s__ONIKO_END__' \"$PATH\""])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output();
        let from_shell = output.ok().and_then(|out| {
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            let start = text.find("__ONIKO_PATH__")? + "__ONIKO_PATH__".len();
            let end = text[start..].find("__ONIKO_END__")? + start;
            Some(text[start..end].to_owned())
        });
        match from_shell.filter(|p| !p.is_empty()) {
            // Keep anything the launcher added that the shell does not know about.
            Some(path) => {
                let mut parts: Vec<&str> = path.split(':').collect();
                for extra in inherited.split(':') {
                    if !extra.is_empty() && !parts.contains(&extra) {
                        parts.push(extra);
                    }
                }
                parts.join(":")
            }
            None => inherited,
        }
    })
}

/// The first executable named `name` on the login PATH.
pub fn which(name: &str) -> Option<PathBuf> {
    std::env::split_paths(shell_path()).map(|dir| dir.join(name)).find(|candidate| {
        std::fs::metadata(candidate).map(|m| m.is_file() && is_executable(&m)).unwrap_or(false)
    })
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    true
}

/// The adapter version the Electron app bundled; npx runs this one when nothing is installed.
pub const CLAUDE_ACP_PACKAGE: &str = "@agentclientprotocol/claude-agent-acp@0.98.0";

/// The Claude adapter is a Node package, so it runs from: `ONIKO_CLAUDE_ACP` (a full command line),
/// a `claude-agent-acp` installed on the PATH, or `npx` fetching the pinned version.
pub fn claude_spawn_spec() -> Result<SpawnSpec, String> {
    if let Ok(line) = std::env::var("ONIKO_CLAUDE_ACP")
        && !line.trim().is_empty()
    {
        let mut words = shell_words::split(&line).map_err(|e| format!("ONIKO_CLAUDE_ACP is not a valid command: {e}"))?;
        let command = words.remove(0);
        return Ok(SpawnSpec { command, args: words, env: EnvLayer::new() });
    }
    if let Some(bin) = which("claude-agent-acp") {
        return Ok(SpawnSpec { command: bin.to_string_lossy().into_owned(), args: vec![], env: EnvLayer::new() });
    }
    if let Some(npx) = which("npx") {
        return Ok(SpawnSpec {
            command: npx.to_string_lossy().into_owned(),
            args: vec!["--yes".into(), CLAUDE_ACP_PACKAGE.into()],
            env: EnvLayer::new(),
        });
    }
    Err("Claude needs Node.js: install it, or run `npm install -g @agentclientprotocol/claude-agent-acp`".into())
}

pub fn spawn_spec(agent: AgentId) -> Result<SpawnSpec, String> {
    match agent {
        AgentId::Claude => claude_spawn_spec(),
    }
}

/// Expands `~`, drops trailing slashes and `..`; None when the input is not an absolute path.
pub fn normalize_config_dir(input: &str, home: &Path) -> Option<String> {
    let trimmed = input.trim();
    let expanded: PathBuf = if trimmed == "~" {
        home.to_path_buf()
    } else if let Some(rest) = trimmed.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(trimmed)
    };
    expanded.is_absolute().then(|| resolve(&expanded).to_string_lossy().into_owned())
}

/// Claude keys the default login to an unset CLAUDE_CONFIG_DIR: setting it to ~/.claude reads a
/// different keychain entry and reports logged out. So the default dir unsets it, and every other
/// dir sets it, which also keeps whatever Oniko was launched with from leaking through.
pub fn claude_env_for(config_dir: &str, home: &Path) -> EnvLayer {
    let default = home.join(".claude");
    let value = (Path::new(config_dir) != default).then(|| config_dir.to_owned());
    EnvLayer::from([("CLAUDE_CONFIG_DIR".to_owned(), value)])
}

fn shell_quote(value: &str) -> String {
    let plain = !value.is_empty() && value.chars().all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if plain { value.to_owned() } else { format!("'{}'", value.replace('\'', r"'\''")) }
}

pub fn claude_login_command(config_dir: &str, home: &Path) -> String {
    match claude_env_for(config_dir, home).remove("CLAUDE_CONFIG_DIR").flatten() {
        None => "claude auth login".into(),
        Some(dir) => format!("CLAUDE_CONFIG_DIR={} claude auth login", shell_quote(&dir)),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthStatus {
    logged_in: bool,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    org_name: Option<String>,
    #[serde(default)]
    subscription_type: Option<String>,
}

pub fn parse_claude_auth_status(stdout: &str, now: i64) -> AccountStatus {
    let json: serde_json::Value = match serde_json::from_str(stdout) {
        Ok(json) => json,
        Err(_) => {
            let shown: String = stdout.trim().chars().take(200).collect();
            let shown = if shown.is_empty() { "(empty)".to_owned() } else { shown };
            return AccountStatus::Error { message: format!("Unexpected output from claude auth status: {shown}"), checked_at: now };
        }
    };
    let Ok(status) = serde_json::from_value::<AuthStatus>(json) else {
        return AccountStatus::Error { message: "claude auth status returned an unexpected shape".into(), checked_at: now };
    };
    if !status.logged_in {
        return AccountStatus::LoggedOut { checked_at: now };
    }
    AccountStatus::LoggedIn { email: status.email, org_name: status.org_name, plan: status.subscription_type, checked_at: now }
}

const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// `ONIKO_CLAUDE_CLI`, else `claude` on the login PATH: the CLI the adapter runs sessions with.
fn claude_cli() -> Option<PathBuf> {
    std::env::var_os("ONIKO_CLAUDE_CLI").map(PathBuf::from).or_else(|| which("claude"))
}

async fn probe_claude(config_dir: String) -> AccountStatus {
    let Some(cli) = claude_cli() else {
        return AccountStatus::Error { message: "Claude CLI not found on the PATH".into(), checked_at: now_ms() };
    };
    let env = child_env(base_env(), [&claude_env_for(&config_dir, &home_dir())]);
    let run = tokio::process::Command::new(cli).args(["auth", "status", "--json"]).env_clear().envs(env).kill_on_drop(true).output();
    match tokio::time::timeout(PROBE_TIMEOUT, run).await {
        Err(_) => AccountStatus::Error { message: "claude auth status timed out".into(), checked_at: now_ms() },
        Ok(Err(error)) => AccountStatus::Error { message: error.to_string(), checked_at: now_ms() },
        // A logged-out dir exits 1 with valid JSON on stdout, so stdout is parsed whatever the exit code.
        Ok(Ok(out)) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            if !out.status.success() && stdout.trim().is_empty() {
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
                let message = if stderr.is_empty() { format!("claude auth status exited with {}", out.status) } else { stderr };
                return AccountStatus::Error { message, checked_at: now_ms() };
            }
            parse_claude_auth_status(&stdout, now_ms())
        }
    }
}

fn claude_dirs(home: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(home) else { return vec![] };
    let mut dirs: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name == ".claude" || name.strip_prefix(".claude-").is_some_and(|rest| !rest.is_empty())
        })
        .filter(|e| e.path().is_dir())
        .map(|e| e.path().to_string_lossy().into_owned())
        .collect();
    dirs.sort();
    dirs
}

/// What Oniko knows about an agent's logins. A trait so tests can stand in for the real CLI.
pub trait AccountSpec: Send + Sync {
    fn env_for(&self, config_dir: &str) -> EnvLayer;
    fn probe_status(&self, config_dir: &str) -> BoxFuture<'static, AccountStatus>;
    /// Existing config dirs worth suggesting when adding a subscription.
    fn default_dirs(&self) -> Vec<String>;
    fn login_command(&self, config_dir: &str) -> String;
}

pub struct ClaudeAccounts;

impl AccountSpec for ClaudeAccounts {
    fn env_for(&self, config_dir: &str) -> EnvLayer {
        claude_env_for(config_dir, &home_dir())
    }

    fn probe_status(&self, config_dir: &str) -> BoxFuture<'static, AccountStatus> {
        Box::pin(probe_claude(config_dir.to_owned()))
    }

    fn default_dirs(&self) -> Vec<String> {
        claude_dirs(&home_dir())
    }

    fn login_command(&self, config_dir: &str) -> String {
        claude_login_command(config_dir, &home_dir())
    }
}

pub type AccountSpecs = BTreeMap<AgentId, Arc<dyn AccountSpec>>;

pub fn account_specs() -> AccountSpecs {
    AccountSpecs::from([(AgentId::Claude, Arc::new(ClaudeAccounts) as Arc<dyn AccountSpec>)])
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/Users/someone";

    fn home() -> PathBuf {
        PathBuf::from(HOME)
    }

    #[test]
    fn normalize_expands_tilde_and_drops_trailing_slashes() {
        assert_eq!(normalize_config_dir("~/.claude-personal/", &home()).as_deref(), Some("/Users/someone/.claude-personal"));
        assert_eq!(normalize_config_dir("~", &home()).as_deref(), Some(HOME));
        assert_eq!(normalize_config_dir("  /opt/claude//work/../team/  ", &home()).as_deref(), Some("/opt/claude/team"));
    }

    #[test]
    fn normalize_rejects_relative_paths() {
        assert_eq!(normalize_config_dir(".claude", &home()), None);
        assert_eq!(normalize_config_dir("~other/.claude", &home()), None);
    }

    #[test]
    fn default_dir_unsets_claude_config_dir() {
        assert_eq!(claude_env_for("/Users/someone/.claude", &home())["CLAUDE_CONFIG_DIR"], None);
        assert_eq!(claude_env_for("/Users/someone/.claude-personal", &home())["CLAUDE_CONFIG_DIR"].as_deref(), Some("/Users/someone/.claude-personal"));
        let typed = normalize_config_dir("~/.claude/", &home()).unwrap();
        assert_eq!(claude_env_for(&typed, &home())["CLAUDE_CONFIG_DIR"], None);
    }

    #[test]
    fn inherited_claude_config_dir_never_reaches_the_child() {
        let inherited = vec![("PATH".to_owned(), "/usr/bin".to_owned()), ("CLAUDE_CONFIG_DIR".to_owned(), "/bogus".to_owned())];
        let env = child_env(inherited.clone(), [&claude_env_for("/Users/someone/.claude", &home())]);
        assert_eq!(env, BTreeMap::from([("PATH".to_owned(), "/usr/bin".to_owned())]));
        let env = child_env(inherited, [&claude_env_for("/Users/someone/.claude-work", &home())]);
        assert_eq!(env["CLAUDE_CONFIG_DIR"], "/Users/someone/.claude-work");
    }

    #[test]
    fn login_command_can_be_pasted() {
        assert_eq!(claude_login_command("/Users/someone/.claude", &home()), "claude auth login");
        assert_eq!(
            claude_login_command("/Users/someone/.claude-personal", &home()),
            "CLAUDE_CONFIG_DIR=/Users/someone/.claude-personal claude auth login"
        );
        assert_eq!(claude_login_command("/Users/some one/it's", &home()), r"CLAUDE_CONFIG_DIR='/Users/some one/it'\''s' claude auth login");
    }

    #[test]
    fn parses_auth_status() {
        let logged_in = r#"{"loggedIn":true,"authMethod":"claude.ai","email":"dev@example.com","orgName":"Example Org","subscriptionType":"max"}"#;
        assert_eq!(
            parse_claude_auth_status(logged_in, 7),
            AccountStatus::LoggedIn {
                email: Some("dev@example.com".into()),
                org_name: Some("Example Org".into()),
                plan: Some("max".into()),
                checked_at: 7
            }
        );
        assert_eq!(parse_claude_auth_status(r#"{"loggedIn":false,"authMethod":"none"}"#, 7), AccountStatus::LoggedOut { checked_at: 7 });
        for garbage in ["Not logged in. Run claude auth login", r#"{"loggedIn":"yes"}"#, ""] {
            assert!(matches!(parse_claude_auth_status(garbage, 7), AccountStatus::Error { checked_at: 7, .. }), "{garbage}");
        }
    }
}
