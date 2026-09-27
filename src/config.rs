//! KDL config: the ordered layout pair and its file location.
//!
//! The core is one node in `$XDG_CONFIG_HOME/niri-punto/config.kdl`:
//!
//! ```kdl
//! layouts "us" "ru"
//! ```
//!
//! Position in the pair maps to the niri layout index, so the order must
//! match the `layout` line in the niri config. Parsing and validation are
//! pure and unit-tested; only the path helpers and file reads touch the
//! environment.

use crate::reader;
use crate::trigger::{Bind, GestureKind, ModSet, TapMode, TimingConfig};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Default config written by `setup`. Never overwrites an existing file.
pub const DEFAULT_CONFIG: &str = r#"// niri-punto config. The ordered layout pair: position maps to the niri
// layout index, so the order must match the `layout` line in your niri
// config. `setup` never overwrites this file once created.
layouts "us" "ru"
// Lone Mod tap trigger: "meta" converts on tap, "off" disables it.
tap "meta"
// Daemon-side binds in niri style: no niri binds needed. One per scope,
// mirroring the gesture table in README.
binds {
    Mod+L word
    Mod+P phrase
    Mod+S selection
}
// Trigger timings in milliseconds: absent keys mean these defaults.
timings {
    double-shift-ms 400
    undo-ms 3000
    debounce-ms 30
    pending-ms 2000
    tap-ms 300
}
"#;

/// File name of the udev rule, shared with `setup` and `doctor`. The `70-`
/// prefix is load-bearing: it must sort before stock `71-seat` (derives
/// the seat tag from `uaccess`) and `73-seat-late` (queues the ACL
/// builtin); a `99-*` name tags devices too late and no ACL is written.
pub const RULE_FILE_NAME: &str = "70-niri-punto.rules";

/// File name of the modules-load entry that pulls in `uinput` at boot.
pub const MODULES_FILE_NAME: &str = "niri-punto.conf";

/// Ordered layout pair from the `layouts` node.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LayoutPair {
    pub first: String,
    pub second: String,
}

impl LayoutPair {
    /// Validate two codes: non-empty and distinct.
    pub fn new(first: &str, second: &str) -> Result<Self, ConfigError> {
        if first.is_empty() || second.is_empty() {
            return Err(ConfigError::EmptyCode);
        }
        if first == second {
            return Err(ConfigError::Duplicate(first.to_string()));
        }
        Ok(Self {
            first: first.to_string(),
            second: second.to_string(),
        })
    }

    /// Code at pair position `index` (0 or 1); `None` for anything else.
    pub fn get(&self, index: u8) -> Option<&str> {
        match index {
            0 => Some(&self.first),
            1 => Some(&self.second),
            _ => None,
        }
    }
}

/// Why a config could not be loaded or understood.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ConfigError {
    /// The file is not valid KDL.
    Parse(String),
    /// No `layouts` node present.
    MissingLayouts,
    /// More than one `layouts` node present.
    DuplicateNode,
    /// The node does not hold exactly two layout codes.
    BadArity { found: usize },
    /// A code is empty or both codes are identical.
    EmptyCode,
    /// Both codes are identical.
    Duplicate(String),
    /// The `tap` node is repeated or holds a bad value.
    BadTap(String),
    /// A `binds` entry is malformed (combo or action).
    BadBind(String),
    /// The `timings` block is malformed (keys or values).
    BadTimings(String),
    /// File I/O failed (missing file, permissions).
    Io(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(message) => write!(f, "invalid KDL: {message}"),
            Self::MissingLayouts => {
                write!(f, "missing `layouts` node (want `layouts \"us\" \"ru\"`)")
            }
            Self::DuplicateNode => write!(f, "more than one `layouts` node: keep exactly one"),
            Self::BadArity { found } => write!(
                f,
                "`layouts` needs exactly two codes, found {found} (want `layouts \"us\" \"ru\"`)"
            ),
            Self::EmptyCode => write!(f, "layout codes must be non-empty"),
            Self::Duplicate(code) => write!(f, "layout codes must differ, both are \"{code}\""),
            Self::BadTap(message) => write!(f, "bad `tap` node: {message}"),
            Self::BadBind(message) => write!(f, "bad bind: {message}"),
            Self::BadTimings(message) => write!(f, "bad `timings` block: {message}"),
            Self::Io(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<io::Error> for ConfigError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

/// Parse config text into the layout pair. Unknown nodes are ignored so
/// later tickets can extend the file without breaking old binaries.
pub fn parse(text: &str) -> Result<LayoutPair, ConfigError> {
    let document: kdl::KdlDocument = text
        .parse()
        .map_err(|error: kdl::KdlError| ConfigError::Parse(error.to_string()))?;
    let mut layouts = document
        .nodes()
        .iter()
        .filter(|node| node.name().value() == "layouts");
    let node = layouts.next().ok_or(ConfigError::MissingLayouts)?;
    if layouts.next().is_some() {
        return Err(ConfigError::DuplicateNode);
    }
    // Strict shape: exactly two positional string arguments, no
    // properties. Anything else is almost certainly a typo, so report
    // the entry count rather than guessing.
    if node.entries().len() != 2 {
        return Err(ConfigError::BadArity {
            found: node.entries().len(),
        });
    }
    let mut codes = Vec::with_capacity(2);
    for entry in node.entries() {
        let Some(code) = (entry.name().is_none())
            .then(|| match entry.value() {
                kdl::KdlValue::String(code) => Some(code.as_str()),
                _ => None,
            })
            .flatten()
        else {
            return Err(ConfigError::BadArity { found: 2 });
        };
        codes.push(code);
    }
    LayoutPair::new(codes[0], codes[1])
}

/// Trigger settings from the optional `tap` node and repeatable `chord`
/// nodes. Missing nodes mean defaults (tap on, no chords), so config files
/// written before these nodes existed keep working.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct TriggerSettings {
    pub tap: TapMode,
    pub binds: Vec<Bind>,
    pub timing: TimingConfig,
}

/// Parse trigger settings. Unknown nodes are ignored, like in [`parse`].
pub fn parse_settings(text: &str) -> Result<TriggerSettings, ConfigError> {
    let document: kdl::KdlDocument = text
        .parse()
        .map_err(|error: kdl::KdlError| ConfigError::Parse(error.to_string()))?;
    let mut settings = TriggerSettings::default();
    let mut tap_seen = false;
    let mut timings_seen = false;
    let mut binds_seen = false;
    for node in document.nodes() {
        match node.name().value() {
            "tap" => {
                if tap_seen {
                    return Err(ConfigError::BadTap(
                        "more than one `tap` node: keep exactly one".to_string(),
                    ));
                }
                tap_seen = true;
                settings.tap = parse_tap(node)?;
            }
            "binds" => {
                if binds_seen {
                    return Err(ConfigError::BadBind(
                        "more than one `binds` block: keep exactly one".to_string(),
                    ));
                }
                binds_seen = true;
                settings.binds = parse_binds(node)?;
            }
            // `chord` lines died in favor of the `binds` block: fail loudly
            // instead of silently dropping the user's triggers.
            "chord" => {
                return Err(ConfigError::BadBind(
                    "`chord` lines were replaced by the `binds` block (e.g. `binds { Mod+L word }`)".to_string(),
                ));
            }
            "timings" => {
                if timings_seen {
                    return Err(ConfigError::BadTimings(
                        "more than one `timings` block: keep exactly one".to_string(),
                    ));
                }
                timings_seen = true;
                settings.timing = parse_timings(node)?;
            }
            _ => {}
        }
    }
    Ok(settings)
}

fn node_args(node: &kdl::KdlNode) -> Option<Vec<&str>> {
    let mut args = Vec::with_capacity(node.entries().len());
    for entry in node.entries() {
        let arg = (entry.name().is_none())
            .then(|| match entry.value() {
                kdl::KdlValue::String(arg) => Some(arg.as_str()),
                _ => None,
            })
            .flatten()?;
        args.push(arg);
    }
    Some(args)
}

fn parse_tap(node: &kdl::KdlNode) -> Result<TapMode, ConfigError> {
    match node_args(node).as_deref() {
        Some(["meta"]) => Ok(TapMode::Meta),
        Some(["off"]) => Ok(TapMode::Off),
        _ => Err(ConfigError::BadTap(
            "want exactly one of `meta`, `off` (e.g. `tap \"meta\"`)".to_string(),
        )),
    }
}

fn parse_binds(node: &kdl::KdlNode) -> Result<Vec<Bind>, ConfigError> {
    if !node.entries().is_empty() {
        return Err(ConfigError::BadBind(
            "the block takes no arguments, only `Combo action` lines".to_string(),
        ));
    }
    let mut binds = Vec::new();
    if let Some(children) = node.children() {
        for child in children.nodes() {
            binds.push(parse_bind(child)?);
        }
    }
    Ok(binds)
}

/// One bind: the node name is the niri-style combo (`Mod+L`,
/// `Mod+Shift+P`), the single argument is the action.
fn parse_bind(node: &kdl::KdlNode) -> Result<Bind, ConfigError> {
    let action = match node_args(node).as_deref() {
        Some([action]) => *action,
        _ => {
            return Err(ConfigError::BadBind(format!(
                "{:?} wants exactly one action (`word`, `phrase`, or `selection`)",
                node.name().value()
            )));
        }
    };
    let combo = node.name().value();
    let mut parts: Vec<&str> = combo.split('+').collect();
    let Some(key) = parts.pop() else {
        return Err(ConfigError::BadBind(format!("bad combo {combo:?}")));
    };
    let mut mods = ModSet::default();
    for part in &parts {
        match part.to_lowercase().as_str() {
            "mod" => mods.meta = true,
            "shift" => mods.shift = true,
            "ctrl" => mods.ctrl = true,
            "alt" => {
                return Err(ConfigError::BadBind(
                    "alt binds need Alt tracking, which is not implemented yet".to_string(),
                ));
            }
            _ => {
                return Err(ConfigError::BadBind(format!(
                    "bad modifier {part:?} in {combo:?}: want `Mod`, `Shift`, or `Ctrl`"
                )));
            }
        }
    }
    if !mods.any() {
        return Err(ConfigError::BadBind(format!(
            "bad combo {combo:?}: need at least one modifier (e.g. `Mod+L`)"
        )));
    }
    let Some(scancode) = reader::scancode_by_name(&key.to_lowercase()) else {
        return Err(ConfigError::BadBind(format!(
            "unknown key {key:?} in {combo:?}: want a letter or digit"
        )));
    };
    let kind = match action {
        "word" => GestureKind::Word,
        "phrase" => GestureKind::Phrase,
        "selection" => GestureKind::Selection,
        _ => {
            return Err(ConfigError::BadBind(format!(
                "bad action {action:?}: want `word`, `phrase`, or `selection`"
            )));
        }
    };
    Ok(Bind {
        mods,
        scancode,
        kind,
    })
}

/// Exactly one positional integer argument, no properties.
fn single_int(node: &kdl::KdlNode) -> Option<i128> {
    let [entry] = node.entries() else {
        return None;
    };
    if entry.name().is_some() {
        return None;
    }
    match entry.value() {
        kdl::KdlValue::Integer(ms) => Some(*ms),
        _ => None,
    }
}

/// Parse the `timings` block. Absent keys mean defaults; unknown keys,
/// duplicates, and non-integer values are errors (a typoed number must
/// not silently apply).
fn parse_timings(node: &kdl::KdlNode) -> Result<TimingConfig, ConfigError> {
    if !node.entries().is_empty() {
        return Err(ConfigError::BadTimings(
            "the block takes no arguments, only child nodes".to_string(),
        ));
    }
    let Some(children) = node.children() else {
        return Ok(TimingConfig::default());
    };
    let mut timing = TimingConfig::default();
    let mut seen = [false; 5];
    for child in children.nodes() {
        let slot = match child.name().value() {
            "double-shift-ms" => 0,
            "undo-ms" => 1,
            "debounce-ms" => 2,
            "pending-ms" => 3,
            "tap-ms" => 4,
            unknown => {
                return Err(ConfigError::BadTimings(format!("unknown key {unknown:?}")));
            }
        };
        if seen[slot] {
            return Err(ConfigError::BadTimings(format!(
                "duplicate key {:?}",
                child.name().value()
            )));
        }
        seen[slot] = true;
        let Some(ms) = single_int(child) else {
            return Err(ConfigError::BadTimings(format!(
                "{:?} wants exactly one integer number of milliseconds",
                child.name().value()
            )));
        };
        if ms < 0 || ms > u64::MAX as i128 {
            return Err(ConfigError::BadTimings(format!(
                "{:?} must fit in milliseconds 0..=u64::MAX, got {ms}",
                child.name().value()
            )));
        }
        let ms = ms as u64;
        match slot {
            0 => timing.double_shift_ms = ms,
            1 => timing.undo_ms = ms,
            2 => timing.debounce_ms = ms,
            3 => timing.pending_ms = ms,
            _ => timing.tap_ms = ms,
        }
    }
    Ok(timing)
}

/// Config path for explicit homes. Pure: the env-reading wrapper is
/// [`config_path`].
pub fn config_path_for(home: &Path, xdg_config_home: Option<&str>) -> PathBuf {
    let base = match xdg_config_home {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => home.join(".config"),
    };
    base.join("niri-punto").join("config.kdl")
}

/// `$XDG_CONFIG_HOME/niri-punto/config.kdl`, falling back to
/// `~/.config/niri-punto/config.kdl`.
pub fn config_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    let xdg = std::env::var("XDG_CONFIG_HOME").ok();
    config_path_for(&home, xdg.as_deref())
}

/// Read and parse the config at `path`.
pub fn load_from(path: &Path) -> Result<LayoutPair, ConfigError> {
    let text = read_text(path)?;
    parse(&text).map_err(|error| with_path(path, error))
}

/// Read and parse the default-location config.
pub fn load() -> Result<LayoutPair, ConfigError> {
    load_from(&config_path())
}

/// Read and parse trigger settings at `path`.
pub fn load_settings_from(path: &Path) -> Result<TriggerSettings, ConfigError> {
    let text = read_text(path)?;
    parse_settings(&text).map_err(|error| with_path(path, error))
}

/// Read and parse trigger settings at the default location.
pub fn load_settings() -> Result<TriggerSettings, ConfigError> {
    load_settings_from(&config_path())
}

fn read_text(path: &Path) -> Result<String, ConfigError> {
    std::fs::read_to_string(path)
        .map_err(|error| ConfigError::Io(format!("cannot read {}: {error}", path.display())))
}

/// Name the file in content errors, so a bad line is findable.
fn with_path(path: &Path, error: ConfigError) -> ConfigError {
    match error {
        ConfigError::Parse(_)
        | ConfigError::MissingLayouts
        | ConfigError::DuplicateNode
        | ConfigError::BadArity { .. }
        | ConfigError::EmptyCode
        | ConfigError::Duplicate(_)
        | ConfigError::BadTap(_)
        | ConfigError::BadBind(_)
        | ConfigError::BadTimings(_) => ConfigError::Io(format!("{}: {error}", path.display())),
        ConfigError::Io(_) => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_pair() {
        let pair = parse("layouts \"us\" \"ru\"\n").unwrap();
        assert_eq!(pair.first, "us");
        assert_eq!(pair.second, "ru");
    }

    #[test]
    fn default_config_parses_to_us_ru() {
        let pair = parse(DEFAULT_CONFIG).unwrap();
        assert_eq!(
            pair,
            LayoutPair {
                first: "us".to_string(),
                second: "ru".to_string(),
            }
        );
    }

    #[test]
    fn comments_and_unknown_nodes_are_ignored() {
        let pair =
            parse("// leading comment\nlayouts \"us\" \"ru\" // trailing\nbind \"x\"\n").unwrap();
        assert_eq!(pair.first, "us");
    }

    #[test]
    fn missing_node_reports() {
        assert_eq!(parse("bind \"x\"\n"), Err(ConfigError::MissingLayouts));
        assert_eq!(parse(""), Err(ConfigError::MissingLayouts));
    }

    #[test]
    fn duplicate_node_reports() {
        assert_eq!(
            parse("layouts \"us\" \"ru\"\nlayouts \"us\" \"de\"\n"),
            Err(ConfigError::DuplicateNode)
        );
    }

    #[test]
    fn wrong_arity_reports_count() {
        assert_eq!(
            parse("layouts \"us\"\n"),
            Err(ConfigError::BadArity { found: 1 })
        );
        assert_eq!(
            parse("layouts \"us\" \"ru\" \"de\"\n"),
            Err(ConfigError::BadArity { found: 3 })
        );
        assert_eq!(parse("layouts\n"), Err(ConfigError::BadArity { found: 0 }));
    }

    #[test]
    fn non_string_arguments_report_arity() {
        assert_eq!(
            parse("layouts \"us\" 42\n"),
            Err(ConfigError::BadArity { found: 2 })
        );
    }

    #[test]
    fn properties_are_rejected() {
        assert!(parse("layouts first=\"us\" second=\"ru\"\n").is_err());
    }

    #[test]
    fn empty_or_equal_codes_are_rejected() {
        assert_eq!(parse("layouts \"\" \"ru\"\n"), Err(ConfigError::EmptyCode));
        assert_eq!(
            parse("layouts \"us\" \"us\"\n"),
            Err(ConfigError::Duplicate("us".to_string()))
        );
    }

    #[test]
    fn invalid_kdl_reports_parse_error() {
        assert!(matches!(
            parse("layouts \"us\"\nlayouts\n\"dangling"),
            Err(ConfigError::Parse(_))
        ));
    }

    #[test]
    fn get_maps_positions() {
        let pair = LayoutPair::new("us", "ru").unwrap();
        assert_eq!(pair.get(0), Some("us"));
        assert_eq!(pair.get(1), Some("ru"));
        assert_eq!(pair.get(2), None);
    }

    #[test]
    fn xdg_home_wins_over_dot_config() {
        assert_eq!(
            config_path_for(Path::new("/home/u"), Some("/home/u/.xcfg")),
            PathBuf::from("/home/u/.xcfg/niri-punto/config.kdl")
        );
        assert_eq!(
            config_path_for(Path::new("/home/u"), None),
            PathBuf::from("/home/u/.config/niri-punto/config.kdl")
        );
        assert_eq!(
            config_path_for(Path::new("/home/u"), Some("")),
            PathBuf::from("/home/u/.config/niri-punto/config.kdl")
        );
    }

    #[test]
    fn settings_default_to_tap_on_without_binds() {
        let settings = parse_settings("layouts \"us\" \"ru\"\n").unwrap();
        assert_eq!(settings.tap, TapMode::Meta);
        assert!(settings.binds.is_empty());
    }

    #[test]
    fn default_config_settings_parse() {
        let settings = parse_settings(DEFAULT_CONFIG).unwrap();
        assert_eq!(settings.tap, TapMode::Meta);
        assert_eq!(settings.binds.len(), 3);
        assert_eq!(settings.binds[0].kind, GestureKind::Word);
        assert_eq!(settings.binds[1].kind, GestureKind::Phrase);
        assert_eq!(settings.binds[2].kind, GestureKind::Selection);
        assert!(settings.binds.iter().all(|bind| bind.mods.meta));
    }

    #[test]
    fn tap_off_parses() {
        let settings = parse_settings("layouts \"us\" \"ru\"\ntap \"off\"\n").unwrap();
        assert_eq!(settings.tap, TapMode::Off);
    }

    #[test]
    fn tap_rejects_bad_values() {
        assert!(parse_settings("layouts \"us\" \"ru\"\ntap \"alt\"\n").is_err());
        assert!(parse_settings("layouts \"us\" \"ru\"\ntap\n").is_err());
        assert!(parse_settings("layouts \"us\" \"ru\"\ntap \"meta\" \"off\"\n").is_err());
        assert!(parse_settings("layouts \"us\" \"ru\"\ntap \"meta\"\ntap \"off\"\n").is_err());
    }

    #[test]
    fn binds_parse_all_actions() {
        let settings = parse_settings(
            "layouts \"us\" \"ru\"\nbinds {\n Mod+L word\n Mod+S selection\n Mod+Shift+P phrase\n}\n",
        )
        .unwrap();
        assert_eq!(settings.binds.len(), 3);
        assert_eq!(settings.binds[0].scancode, 38);
        assert_eq!(settings.binds[0].kind, GestureKind::Word);
        assert!(settings.binds[0].mods.meta);
        assert_eq!(settings.binds[1].kind, GestureKind::Selection);
        assert_eq!(settings.binds[2].kind, GestureKind::Phrase);
        assert!(settings.binds[2].mods.meta && settings.binds[2].mods.shift);
    }

    #[test]
    fn binds_accept_lowercase_modifiers() {
        let settings = parse_settings("layouts \"us\" \"ru\"\nbinds {\n mod+l word\n}\n").unwrap();
        assert!(settings.binds[0].mods.meta);
    }

    #[test]
    fn binds_reject_bad_combos_and_actions() {
        for block in [
            "binds {\n Win+L word\n}\n",
            "binds {\n L word\n}\n",
            "binds {\n Mod+ word\n}\n",
            "binds {\n Mod+Space word\n}\n",
            "binds {\n Mod+Alt+L word\n}\n",
            "binds {\n Mod+L sentence\n}\n",
            "binds {\n Mod+L\n}\n",
            "binds {\n Mod+L word extra\n}\n",
            "binds {\n Mod+L word\n}\nbinds {\n Mod+S selection\n}\n",
            "binds Mod+L\n",
        ] {
            let text = format!("layouts \"us\" \"ru\"\n{block}");
            assert!(parse_settings(&text).is_err(), "{block}");
        }
    }

    #[test]
    fn chord_lines_fail_with_migration_hint() {
        let error =
            parse_settings("layouts \"us\" \"ru\"\nchord \"meta+l\" \"word\"\n").unwrap_err();
        assert!(error.to_string().contains("binds"), "{error}");
    }

    #[test]
    fn timings_default_without_block() {
        let settings = parse_settings("layouts \"us\" \"ru\"\n").unwrap();
        assert_eq!(settings.timing, TimingConfig::default());
    }

    #[test]
    fn timings_full_block_parses() {
        let settings = parse_settings(
            "layouts \"us\" \"ru\"\ntimings {\n double-shift-ms 1000\n undo-ms 2000\n debounce-ms 10\n pending-ms 500\n tap-ms 200\n}\n",
        )
        .unwrap();
        assert_eq!(
            settings.timing,
            TimingConfig {
                double_shift_ms: 1000,
                undo_ms: 2000,
                debounce_ms: 10,
                pending_ms: 500,
                tap_ms: 200,
            }
        );
    }

    #[test]
    fn timings_partial_block_keeps_defaults() {
        let settings = parse_settings("layouts \"us\" \"ru\"\ntimings { tap-ms 200 }\n").unwrap();
        assert_eq!(settings.timing.tap_ms, 200);
        assert_eq!(
            settings.timing,
            TimingConfig {
                tap_ms: 200,
                ..Default::default()
            }
        );
    }

    #[test]
    fn timings_reject_bad_blocks() {
        for block in [
            "timings { double-shift-ms 1.5 }\n",
            "timings { double-shift-ms -5 }\n",
            "timings { double-shift-ms \"fast\" }\n",
            "timings { double-shift-ms }\n",
            "timings { double-shift-ms 100 200 }\n",
            "timings { turbo-ms 100 }\n",
            "timings { tap-ms 100 tap-ms 200 }\n",
            "timings { tap-ms 100 }\ntimings { tap-ms 200 }\n",
            "timings 100\n",
        ] {
            let text = format!("layouts \"us\" \"ru\"\n{block}");
            assert!(parse_settings(&text).is_err(), "{block}");
        }
    }

    #[test]
    fn load_from_missing_file_names_the_path() {
        let error = load_from(Path::new("/nonexistent-dir-for-tests/config.kdl")).unwrap_err();
        assert!(error.to_string().contains("config.kdl"), "{error}");
    }

    #[test]
    fn load_from_bad_content_names_the_path() {
        let dir = std::env::temp_dir().join("niri-punto-test-config");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad-config-test.kdl");
        std::fs::write(&path, "layouts \"only-one\"\n").unwrap();
        let error = load_from(&path).unwrap_err();
        assert!(error.to_string().contains("bad-config-test.kdl"), "{error}");
        std::fs::remove_file(&path).ok();
    }
}
