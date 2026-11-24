//! The host's settings file: JSON, every key optional, unknown keys refused (a typo must
//! not silently fall back to a default). Read by `argusd`; the app can share it.
//!
//! ```json
//! {
//!   "data_root": "/home/me/.local/share/argus",
//!   "socket": "/run/user/1000/argus.sock",
//!   "tools": { "claude": "claude", "tla2tools_jar": "/opt/tla/tla2tools.jar" },
//!   "presets": { "default": ["claude:opus"], "panel": ["claude:opus", "codex:gpt-5:high"] },
//!   "default_preset": "default",
//!   "snapshot_on_turn_end": true,
//!   "snapshot_deny": ["*.sqlite"],
//!   "sensitive": ["infra/**"],
//!   "terminal_days": 30,
//!   "tz_minutes": 540,
//!   "skill_dirs": ["/home/me/.claude/skills"],
//!   "phone": { "listen": "100.101.102.103:7420", "devices": [{ "device": "pixel", "token": "<64 hex>" }] }
//! }
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use argus_core::models::{ModelChoice, ModelPresets, ModelSet, PresetName};
use argus_core::period::UtcOffset;
use argus_core::plan::ToolPaths;
use argus_core::policy::PolicyConfig;
use argus_core::views::packet::{PacketConfig, SENSITIVE_DEFAULT};
use argus_json::Json;

use crate::{HostConfig, PairedDevice, PhoneAccess, PhoneListen, Retention};

/// Where things go when the file does not say: the XDG directories, else `$HOME`.
#[derive(Clone, Debug)]
pub struct Defaults {
    pub home: PathBuf,
    pub data_root: PathBuf,
    pub socket: PathBuf,
    /// The settings file itself.
    pub config: PathBuf,
}

impl Defaults {
    /// From the environment as `std::env::var_os` reads it. `None` without `$HOME`.
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> Option<Defaults> {
        let absolute = |name: &str| var(name).map(PathBuf::from).filter(|p| p.is_absolute());
        let home = absolute("HOME")?;
        let data = absolute("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local/share"));
        let config = absolute("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
        let data_root = data.join("argus");
        let socket = absolute("XDG_RUNTIME_DIR")
            .map(|run| run.join("argus.sock"))
            .unwrap_or_else(|| data_root.join("argus.sock"));
        Some(Defaults {
            config: config.join("argus").join("config.json"),
            home,
            data_root,
            socket,
        })
    }
}

/// The shortest pairing token accepted: 128 bits as hex.
const MIN_TOKEN: usize = 32;

/// `text` is the settings file, or `None` when there is none (all defaults).
pub fn host_config(text: Option<&str>, defaults: &Defaults) -> Result<HostConfig, String> {
    let json = match text {
        Some(text) => argus_json::parse(text.as_bytes()).map_err(|e| format!("{e:?}"))?,
        None => Json::Obj(Vec::new()),
    };
    let Json::Obj(fields) = &json else {
        return Err("the settings must be a JSON object".into());
    };
    const KEYS: &[&str] = &[
        "data_root",
        "socket",
        "tools",
        "presets",
        "default_preset",
        "snapshot_on_turn_end",
        "snapshot_deny",
        "sensitive",
        "terminal_days",
        "tz_minutes",
        "skill_dirs",
        "phone",
    ];
    if let Some((key, _)) = fields.iter().find(|(k, _)| !KEYS.contains(&k.as_str())) {
        return Err(format!("unknown setting {key:?}"));
    }
    let get = |key: &str| json.get(key).filter(|v| **v != Json::Null);

    let data_root = match get("data_root") {
        Some(v) => absolute(v, "data_root")?,
        None => defaults.data_root.clone(),
    };
    let socket = match get("socket") {
        Some(v) => absolute(v, "socket")?,
        None => defaults.socket.clone(),
    };
    let skill_dirs = match get("skill_dirs") {
        Some(v) => list(v, "skill_dirs")?
            .iter()
            .map(|p| absolute(p, "skill_dirs"))
            .collect::<Result<_, _>>()?,
        None => vec![defaults.home.join(".claude").join("skills")],
    };
    // Review needs heads, and a snapshot costs only git: on unless turned off.
    let snapshot_on_turn_end = match get("snapshot_on_turn_end") {
        Some(Json::Bool(b)) => *b,
        Some(_) => return Err("snapshot_on_turn_end: expected true or false".into()),
        None => true,
    };
    let policy = PolicyConfig {
        snapshot_on_turn_end,
        ..PolicyConfig::default()
    };
    let mut sensitive: Vec<String> = SENSITIVE_DEFAULT.iter().map(|s| s.to_string()).collect();
    sensitive.extend(strings(get("sensitive"), "sensitive")?);
    // `"terminal_days": null` keeps terminal logs forever.
    let terminal_days = match json.get("terminal_days") {
        None => Some(30),
        Some(Json::Null) => None,
        Some(v) => Some(int(v, "terminal_days", 1, i64::from(u16::MAX))? as u16),
    };
    let tz = match get("tz_minutes") {
        Some(v) => UtcOffset {
            minutes: int(v, "tz_minutes", -14 * 60, 14 * 60)? as i16,
        },
        None => UtcOffset { minutes: 0 },
    };
    Ok(HostConfig {
        data_root,
        socket,
        phone: get("phone").map(phone).transpose()?,
        tools: tools(get("tools"))?,
        policy,
        presets: presets(get("presets"), get("default_preset"))?,
        packet: PacketConfig { sensitive },
        snapshot_deny: strings(get("snapshot_deny"), "snapshot_deny")?,
        retention: Retention { terminal_days },
        tz,
        skill_dirs,
    })
}

fn text<'a>(v: &'a Json, what: &str) -> Result<&'a str, String> {
    v.as_str()
        .ok_or_else(|| format!("{what}: expected a string"))
}

fn absolute(v: &Json, what: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(text(v, what)?);
    match path.is_absolute() {
        true => Ok(path),
        false => Err(format!(
            "{what}: {} is not an absolute path",
            path.display()
        )),
    }
}

fn list<'a>(v: &'a Json, what: &str) -> Result<&'a [Json], String> {
    match v {
        Json::Arr(items) => Ok(items),
        _ => Err(format!("{what}: expected a list")),
    }
}

fn strings(v: Option<&Json>, what: &str) -> Result<Vec<String>, String> {
    let Some(v) = v else { return Ok(Vec::new()) };
    list(v, what)?
        .iter()
        .map(|s| text(s, what).map(str::to_owned))
        .collect()
}

fn int(v: &Json, what: &str, min: i64, max: i64) -> Result<i64, String> {
    match v {
        Json::Num(n) => n.as_i64().filter(|n| (min..=max).contains(n)),
        _ => None,
    }
    .ok_or_else(|| format!("{what}: expected a whole number from {min} to {max}"))
}

/// Each tool by name, looked up on `PATH` when the process starts, unless set.
fn tools(v: Option<&Json>) -> Result<ToolPaths, String> {
    let mut tools = ToolPaths {
        claude: "claude".into(),
        codex: "codex".into(),
        java: "java".into(),
        tla2tools_jar: "tla2tools.jar".into(),
        tlars: "tlars".into(),
        lake: "lake".into(),
        eas: "eas".into(),
        script: "script".into(),
    };
    let Some(v) = v else { return Ok(tools) };
    let Json::Obj(fields) = v else {
        return Err("tools: expected an object".into());
    };
    for (name, path) in fields {
        let slot = match name.as_str() {
            "claude" => &mut tools.claude,
            "codex" => &mut tools.codex,
            "java" => &mut tools.java,
            "tla2tools_jar" => &mut tools.tla2tools_jar,
            "tlars" => &mut tools.tlars,
            "lake" => &mut tools.lake,
            "eas" => &mut tools.eas,
            "script" => &mut tools.script,
            _ => return Err(format!("tools: unknown tool {name:?}")),
        };
        *slot = PathBuf::from(text(path, &format!("tools.{name}"))?);
    }
    Ok(tools)
}

/// Without any: one preset, `default`, of `claude:opus`.
fn presets(v: Option<&Json>, default: Option<&Json>) -> Result<ModelPresets, String> {
    let mut sets = BTreeMap::new();
    match v {
        None => {
            let opus = ModelChoice::parse("claude:opus").expect("a valid choice");
            sets.insert(
                PresetName::parse("default").expect("a valid name"),
                ModelSet::new([opus]).expect("one model"),
            );
        }
        Some(Json::Obj(fields)) => {
            for (name, models) in fields {
                let what = format!("presets.{name}");
                let key = PresetName::parse(name).ok_or_else(|| {
                    format!("{what}: not a preset name (lowercase words and hyphens)")
                })?;
                let choices = list(models, &what)?
                    .iter()
                    .map(|m| {
                        let m = text(m, &what)?;
                        ModelChoice::parse(m)
                            .ok_or_else(|| format!("{what}: {m:?} is not like claude:opus"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let set = ModelSet::new(choices).ok_or_else(|| format!("{what}: no models"))?;
                sets.insert(key, set);
            }
        }
        Some(_) => return Err("presets: expected an object".into()),
    }
    let default = match default {
        Some(v) => text(v, "default_preset")?.to_owned(),
        None if sets.len() == 1 => sets.keys().next().unwrap().as_str().to_owned(),
        None => "default".to_owned(),
    };
    let name = PresetName::parse(&default)
        .ok_or_else(|| format!("default_preset: {default:?} is not a preset name"))?;
    ModelPresets::new(sets, name).ok_or_else(|| format!("default_preset: no preset {default:?}"))
}

fn phone(v: &Json) -> Result<PhoneAccess, String> {
    let addr = text(v.get("listen").unwrap_or(&Json::Null), "phone.listen")?;
    let addr = addr
        .parse()
        .map_err(|_| format!("phone.listen: {addr:?} is not an address and port"))?;
    let listen = PhoneListen::new(addr)
        .ok_or_else(|| format!("phone.listen: {addr} is neither loopback nor a tailnet address"))?;
    let devices = match v.get("devices") {
        None | Some(Json::Null) => &[][..],
        Some(d) => list(d, "phone.devices")?,
    };
    let devices = devices
        .iter()
        .map(|d| {
            let device = text(
                d.get("device").unwrap_or(&Json::Null),
                "phone.devices.device",
            )?;
            let token = text(d.get("token").unwrap_or(&Json::Null), "phone.devices.token")?;
            if token.len() < MIN_TOKEN {
                return Err(format!(
                    "phone.devices: the token of {device:?} is shorter than {MIN_TOKEN} characters"
                ));
            }
            Ok(PairedDevice {
                device: device.into(),
                token: token.into(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(PhoneAccess { listen, devices })
}

/// Reads the settings file at `path`; a missing file is all defaults.
pub fn read_host_config(path: &Path, defaults: &Defaults) -> Result<HostConfig, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    host_config(text.as_deref(), defaults).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Defaults {
        Defaults::from_env(|name| match name {
            "HOME" => Some("/home/me".into()),
            "XDG_RUNTIME_DIR" => Some("/run/user/1000".into()),
            _ => None,
        })
        .unwrap()
    }

    #[test]
    fn defaults_follow_xdg_and_fall_back_to_home() {
        let d = defaults();
        assert_eq!(d.data_root, PathBuf::from("/home/me/.local/share/argus"));
        assert_eq!(d.socket, PathBuf::from("/run/user/1000/argus.sock"));
        assert_eq!(
            d.config,
            PathBuf::from("/home/me/.config/argus/config.json")
        );

        let bare = Defaults::from_env(|name| (name == "HOME").then(|| "/home/me".into())).unwrap();
        assert_eq!(
            bare.socket,
            PathBuf::from("/home/me/.local/share/argus/argus.sock")
        );
        // A relative XDG directory is ignored, as the spec says.
        let odd = Defaults::from_env(|name| match name {
            "HOME" => Some("/home/me".into()),
            "XDG_DATA_HOME" => Some("data".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(odd.data_root, PathBuf::from("/home/me/.local/share/argus"));
        assert!(Defaults::from_env(|_| None).is_none());
    }

    #[test]
    fn no_file_is_every_default() {
        let c = host_config(None, &defaults()).unwrap();
        assert_eq!(c.data_root, defaults().data_root);
        assert_eq!(c.tools.claude, PathBuf::from("claude"));
        assert_eq!(c.presets.default_name().as_str(), "default");
        assert!(c.policy.snapshot_on_turn_end);
        assert_eq!(c.retention.terminal_days, Some(30));
        assert_eq!(c.tz.minutes, 0);
        assert_eq!(c.skill_dirs, vec![PathBuf::from("/home/me/.claude/skills")]);
        assert_eq!(c.packet.sensitive.len(), SENSITIVE_DEFAULT.len());
        assert!(c.phone.is_none());
    }

    #[test]
    fn every_setting_is_read() {
        let token = "a".repeat(64);
        let text = format!(
            r#"{{
            "data_root": "/srv/argus", "socket": "/srv/argus.sock",
            "tools": {{"claude": "/opt/claude", "tla2tools_jar": "/opt/tla.jar"}},
            "presets": {{"quick": ["claude:haiku"], "panel": ["claude:opus", "codex:gpt-5:high"]}},
            "default_preset": "panel",
            "snapshot_on_turn_end": false, "snapshot_deny": ["*.sqlite"], "sensitive": ["infra/**"],
            "terminal_days": null, "tz_minutes": 540, "skill_dirs": ["/srv/skills"],
            "phone": {{"listen": "100.101.102.103:7420", "devices": [{{"device": "pixel", "token": "{token}"}}]}}
        }}"#
        );
        let c = host_config(Some(&text), &defaults()).unwrap();
        assert_eq!(c.data_root, PathBuf::from("/srv/argus"));
        assert_eq!(c.socket, PathBuf::from("/srv/argus.sock"));
        assert_eq!(c.tools.claude, PathBuf::from("/opt/claude"));
        assert_eq!(c.tools.tla2tools_jar, PathBuf::from("/opt/tla.jar"));
        assert_eq!(c.tools.codex, PathBuf::from("codex"));
        assert_eq!(c.presets.default_name().as_str(), "panel");
        let panel: Vec<String> = c
            .presets
            .iter()
            .find(|(n, _)| n.as_str() == "panel")
            .unwrap()
            .1
            .iter()
            .map(|m| m.to_string())
            .collect();
        assert_eq!(panel, ["claude:opus", "codex:gpt-5:high"]);
        assert!(!c.policy.snapshot_on_turn_end);
        assert_eq!(c.snapshot_deny, ["*.sqlite"]);
        assert_eq!(c.packet.sensitive.last().unwrap(), "infra/**");
        assert_eq!(c.retention.terminal_days, None);
        assert_eq!(c.tz.minutes, 540);
        assert_eq!(c.skill_dirs, [PathBuf::from("/srv/skills")]);
        let phone = c.phone.unwrap();
        assert_eq!(phone.listen.addr().to_string(), "100.101.102.103:7420");
        assert_eq!(phone.devices[0].device, "pixel");
    }

    #[test]
    fn mistakes_are_refused_with_the_setting_named() {
        let short = r#"{"phone": {"listen": "127.0.0.1:7420", "devices": [{"device": "p", "token": "abc"}]}}"#;
        let cases = [
            (r#"[]"#, "object"),
            (r#"{"data_rot": "/x"}"#, "data_rot"),
            (r#"{"data_root": "rel/dir"}"#, "data_root"),
            (r#"{"tools": {"gcc": "/x"}}"#, "gcc"),
            (
                r#"{"presets": {"Panel": ["claude:opus"]}}"#,
                "presets.Panel",
            ),
            (r#"{"presets": {"p": ["gpt-5"]}}"#, "presets.p"),
            (r#"{"presets": {"p": []}}"#, "presets.p"),
            (
                r#"{"presets": {"a": ["claude:opus"], "b": ["claude:haiku"]}}"#,
                "default_preset",
            ),
            (r#"{"default_preset": "nope"}"#, "default_preset"),
            (r#"{"tz_minutes": 9000}"#, "tz_minutes"),
            (r#"{"terminal_days": 0}"#, "terminal_days"),
            (r#"{"snapshot_on_turn_end": "yes"}"#, "snapshot_on_turn_end"),
            (r#"{"phone": {"listen": "0.0.0.0:7420"}}"#, "phone.listen"),
            (
                r#"{"phone": {"listen": "192.168.1.5:7420"}}"#,
                "phone.listen",
            ),
            (short, "phone.devices"),
        ];
        for (text, named) in cases {
            let err = host_config(Some(text), &defaults())
                .err()
                .unwrap_or_else(|| panic!("{text}"));
            assert!(err.contains(named), "{text}: {err}");
        }
    }
}
