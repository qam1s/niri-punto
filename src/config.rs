//! KDL config: the ordered layout pair and its file location.

use crate::reader;
use crate::trigger::{Bind, GestureKind, ModSet, TimingConfig};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Default config written by `setup`. Never overwrites an existing file.
pub const DEFAULT_CONFIG: &str = r#"// niri-punto config. The ordered layout pair: position maps to the niri
layout "us" "ru"
binds {
    Mod word
    Double-Shift selection
}
timings {
    double-shift-ms 400
    undo-ms 3000
    debounce-ms 30
    pending-ms 2000
    tap-ms 300
}
"#;

/// File name of the udev rule, shared with `setup` and `doctor`.
pub const RULE_FILE_NAME: &str = "70-niri-punto.rules";

/// File name of the modules-load entry that pulls in `uinput` at boot.
pub const MODULES_FILE_NAME: &str = "niri-punto.conf";

/// Ordered layout pair from the `layout` node.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LayoutPair {
    pub first: String,
    pub second: String,
}

impl LayoutPair {
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

    pub fn get(&self, index: u8) -> Option<&str> {
        match index {
            0 => Some(&self.first),
            1 => Some(&self.second),
            _ => None,
        }
    }

    pub fn latin_index(&self) -> u8 {
        if is_cyrillic_layout(&self.first) {
            1
        } else {
            0
        }
    }
}

fn is_cyrillic_layout(code: &str) -> bool {
    let base: String = code
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_lowercase();
    matches!(
        base.as_str(),
        "ru" | "ua" | "by" | "bg" | "mk" | "rs" | "kz" | "kg" | "tj" | "mn"
    )
}

/// Why a config could not be loaded or understood.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ConfigError {
    Parse(String),
    MissingLayouts,
    DuplicateNode,
    BadArity { found: usize },
    EmptyCode,
    Duplicate(String),
    BadTap(String),
    BadBind(String),
    RenamedLayouts,
    BadDoubleShift(String),
    BadTimings(String),
    Io(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(message) => write!(f, "invalid KDL: {message}"),
            Self::MissingLayouts => {
                write!(f, "missing `layout` node (want `layout \"us\" \"ru\"`)")
            }
            Self::RenamedLayouts => {
                write!(
                    f,
                    "`layouts` was renamed to `layout` (e.g. `layout \"us\" \"ru\"`)"
                )
            }
            Self::DuplicateNode => write!(f, "more than one `layout` node: keep exactly one"),
            Self::BadArity { found } => write!(
                f,
                "`layout` needs exactly two codes, found {found} (want `layout \"us\" \"ru\"`)"
            ),
            Self::EmptyCode => write!(f, "layout codes must be non-empty"),
            Self::Duplicate(code) => write!(f, "layout codes must differ, both are \"{code}\""),
            Self::BadTap(message) => write!(f, "bad `tap` node: {message}"),
            Self::BadBind(message) => write!(f, "bad bind: {message}"),
            Self::BadDoubleShift(message) => write!(f, "bad `double-shift` node: {message}"),
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

/// Parse config text into the layout pair.
pub fn parse(text: &str) -> Result<LayoutPair, ConfigError> {
    let document: kdl::KdlDocument = text
        .parse()
        .map_err(|error: kdl::KdlError| ConfigError::Parse(error.to_string()))?;
    if document
        .nodes()
        .iter()
        .any(|node| node.name().value() == "layouts")
    {
        return Err(ConfigError::RenamedLayouts);
    }
    let mut layout = document
        .nodes()
        .iter()
        .filter(|node| node.name().value() == "layout");
    let node = layout.next().ok_or(ConfigError::MissingLayouts)?;
    if layout.next().is_some() {
        return Err(ConfigError::DuplicateNode);
    }
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

/// Trigger settings from the `binds` block, the optional `double-shift`
/// node, and the `timings` block.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TriggerSettings {
    /// Lone-Mod-tap scope from a bare-`Mod` bind; `None` disables the tap.
    pub tap_action: Option<GestureKind>,
    pub binds: Vec<Bind>,
    pub pair_base: GestureKind,
    pub timing: TimingConfig,
}

impl Default for TriggerSettings {
    fn default() -> Self {
        Self {
            tap_action: Some(GestureKind::Word),
            binds: Vec::new(),
            pair_base: GestureKind::Word,
            timing: TimingConfig::default(),
        }
    }
}

/// Parse trigger settings.
pub fn parse_settings(text: &str) -> Result<TriggerSettings, ConfigError> {
    let document: kdl::KdlDocument = text
        .parse()
        .map_err(|error: kdl::KdlError| ConfigError::Parse(error.to_string()))?;
    let mut settings = TriggerSettings::default();
    let mut timings_seen = false;
    let mut binds_seen = false;
    for node in document.nodes() {
        match node.name().value() {
            "tap" => {
                return Err(ConfigError::BadTap(
                    "`tap` is now a bare-`Mod` bind (e.g. `binds { Mod word }`); no bare bind means tap off".to_string(),
                ));
            }
            "binds" => {
                if binds_seen {
                    return Err(ConfigError::BadBind(
                        "more than one `binds` block: keep exactly one".to_string(),
                    ));
                }
                binds_seen = true;
                parse_binds(node, &mut settings)?;
            }
            "chord" => {
                return Err(ConfigError::BadBind(
                    "`chord` lines were replaced by the `binds` block (e.g. `binds { Mod+L word }`)".to_string(),
                ));
            }
            name if name.eq_ignore_ascii_case("double-shift") => {
                return Err(ConfigError::BadDoubleShift(
                    "`Double-Shift` moved inside the `binds` block (e.g. `binds { Double-Shift selection }`)".to_string(),
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

fn parse_action(action: &str) -> Result<GestureKind, String> {
    match action {
        "word" => Ok(GestureKind::Word),
        "phrase" => Ok(GestureKind::Phrase),
        "selection" => Ok(GestureKind::Selection),
        _ => Err(format!(
            "bad action {action:?}: want `word`, `phrase`, or `selection`"
        )),
    }
}

fn parse_binds(node: &kdl::KdlNode, settings: &mut TriggerSettings) -> Result<(), ConfigError> {
    if !node.entries().is_empty() {
        return Err(ConfigError::BadBind(
            "the block takes no arguments, only `Combo action` lines".to_string(),
        ));
    }
    let mut tap_seen = false;
    let mut pair_seen = false;
    if let Some(children) = node.children() {
        for child in children.nodes() {
            match parse_bind(child)? {
                BindLine::Bind(bind) => settings.binds.push(bind),
                BindLine::Binds(first, second) => {
                    settings.binds.push(first);
                    settings.binds.push(second);
                }
                BindLine::Tap(kind) => {
                    if tap_seen {
                        return Err(ConfigError::BadBind(
                            "more than one tap bind: keep exactly one bare `Mod` line".to_string(),
                        ));
                    }
                    tap_seen = true;
                    settings.tap_action = kind;
                }
                BindLine::PairBase(kind) => {
                    if pair_seen {
                        return Err(ConfigError::BadBind(
                            "more than one `double-shift` line: keep exactly one".to_string(),
                        ));
                    }
                    pair_seen = true;
                    settings.pair_base = kind;
                }
            }
        }
    }
    Ok(())
}

/// One parsed `binds` line.
enum BindLine {
    Bind(Bind),
    Binds(Bind, Bind),
    Tap(Option<GestureKind>),
    PairBase(GestureKind),
}

fn parse_bind(node: &kdl::KdlNode) -> Result<BindLine, ConfigError> {
    let action = match node_args(node).as_deref() {
        Some([action]) => *action,
        _ => {
            return Err(ConfigError::BadBind(format!(
                "{:?} wants exactly one action (`word`, `phrase`, `selection`, or `off` for tap)",
                node.name().value()
            )));
        }
    };
    let combo = node.name().value();
    let mut parts: Vec<&str> = combo.split('+').collect();
    let Some(key) = parts.pop() else {
        return Err(ConfigError::BadBind(format!("bad combo {combo:?}")));
    };
    if parts.is_empty() {
        if key.eq_ignore_ascii_case("double-shift") {
            let kind = parse_action(action).map_err(ConfigError::BadBind)?;
            return Ok(BindLine::PairBase(kind));
        }
        if !key.eq_ignore_ascii_case("mod") {
            return Err(ConfigError::BadBind(format!(
                "bad combo {combo:?}: need at least one modifier (e.g. `Mod+L`), or lone `Mod` for tap"
            )));
        }
        if action == "off" {
            return Ok(BindLine::Tap(None));
        }
        let kind = parse_action(action).map_err(ConfigError::BadBind)?;
        return Ok(BindLine::Tap(Some(kind)));
    }
    let kind = parse_action(action).map_err(ConfigError::BadBind)?;
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
    if let Some((left, right)) = reader::modifier_scancodes(&key.to_lowercase()) {
        return Ok(BindLine::Binds(
            Bind {
                mods,
                scancode: left,
                kind,
            },
            Bind {
                mods,
                scancode: right,
                kind,
            },
        ));
    }
    let Some(scancode) = reader::scancode_by_name(&key.to_lowercase()) else {
        return Err(ConfigError::BadBind(format!(
            "unknown key {key:?} in {combo:?}: want a letter, digit, or modifier"
        )));
    };
    Ok(BindLine::Bind(Bind {
        mods,
        scancode,
        kind,
    }))
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

pub fn config_path_for(home: &Path, xdg_config_home: Option<&str>) -> PathBuf {
    let base = match xdg_config_home {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => home.join(".config"),
    };
    base.join("niri-punto").join("config.kdl")
}

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
        | ConfigError::BadDoubleShift(_)
        | ConfigError::RenamedLayouts
        | ConfigError::BadTimings(_) => ConfigError::Io(format!("{}: {error}", path.display())),
        ConfigError::Io(_) => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_pair() {
        let pair = parse("layout \"us\" \"ru\"\n").unwrap();
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
            parse("// leading comment\nlayout \"us\" \"ru\" // trailing\nbind \"x\"\n").unwrap();
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
            parse("layout \"us\" \"ru\"\nlayout \"us\" \"de\"\n"),
            Err(ConfigError::DuplicateNode)
        );
    }

    #[test]
    fn wrong_arity_reports_count() {
        assert_eq!(
            parse("layout \"us\"\n"),
            Err(ConfigError::BadArity { found: 1 })
        );
        assert_eq!(
            parse("layout \"us\" \"ru\" \"de\"\n"),
            Err(ConfigError::BadArity { found: 3 })
        );
        assert_eq!(parse("layout\n"), Err(ConfigError::BadArity { found: 0 }));
    }

    #[test]
    fn old_layouts_node_fails_with_rename_hint() {
        assert_eq!(
            parse("layouts \"us\" \"ru\"\n"),
            Err(ConfigError::RenamedLayouts)
        );
    }

    #[test]
    fn non_string_arguments_report_arity() {
        assert_eq!(
            parse("layout \"us\" 42\n"),
            Err(ConfigError::BadArity { found: 2 })
        );
    }

    #[test]
    fn properties_are_rejected() {
        assert!(parse("layouts first=\"us\" second=\"ru\"\n").is_err());
    }

    #[test]
    fn empty_or_equal_codes_are_rejected() {
        assert_eq!(parse("layout \"\" \"ru\"\n"), Err(ConfigError::EmptyCode));
        assert_eq!(
            parse("layout \"us\" \"us\"\n"),
            Err(ConfigError::Duplicate("us".to_string()))
        );
    }

    #[test]
    fn invalid_kdl_reports_parse_error() {
        assert!(matches!(
            parse("layout \"us\"\nlayouts\n\"dangling"),
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
    fn latin_index_follows_the_pair_order() {
        assert_eq!(LayoutPair::new("us", "ru").unwrap().latin_index(), 0);
        assert_eq!(LayoutPair::new("ru", "us").unwrap().latin_index(), 1);
        assert_eq!(LayoutPair::new("de", "ru").unwrap().latin_index(), 0);
        assert_eq!(LayoutPair::new("ru", "de").unwrap().latin_index(), 1);
        assert_eq!(LayoutPair::new("RU", "US").unwrap().latin_index(), 1);
        assert_eq!(
            LayoutPair::new("us", "ru+phonetic").unwrap().latin_index(),
            0
        );
        assert_eq!(LayoutPair::new("us", "de").unwrap().latin_index(), 0);
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
    fn settings_default_to_tap_word_without_binds() {
        let settings = parse_settings("layout \"us\" \"ru\"\n").unwrap();
        assert_eq!(settings.tap_action, Some(GestureKind::Word));
        assert!(settings.binds.is_empty());
        assert_eq!(settings.pair_base, GestureKind::Word);
    }

    #[test]
    fn default_config_settings_parse() {
        let settings = parse_settings(DEFAULT_CONFIG).unwrap();
        assert_eq!(settings.tap_action, Some(GestureKind::Word));
        assert!(settings.binds.is_empty());
        assert_eq!(settings.pair_base, GestureKind::Selection);
    }

    #[test]
    fn readme_example_parses_to_readme_mapping() {
        let settings = parse_settings(
            "layout \"us\" \"ru\"\nbinds {\n Mod word\n double-shift selection\n}\ntimings {\n double-shift-ms 400\n undo-ms 3000\n debounce-ms 30\n pending-ms 2000\n tap-ms 300\n}\n",
        )
        .unwrap();
        assert_eq!(settings.tap_action, Some(GestureKind::Word));
        assert!(settings.binds.is_empty());
        assert_eq!(settings.pair_base, GestureKind::Selection);
        assert_eq!(settings.timing, TimingConfig::default());
    }

    #[test]
    fn bare_mod_sets_tap_action() {
        let settings =
            parse_settings("layout \"us\" \"ru\"\nbinds {\n Mod selection\n}\n").unwrap();
        assert_eq!(settings.tap_action, Some(GestureKind::Selection));
        assert!(settings.binds.is_empty());
    }

    #[test]
    fn bare_mod_off_disables_tap() {
        let settings = parse_settings("layout \"us\" \"ru\"\nbinds {\n Mod off\n}\n").unwrap();
        assert_eq!(settings.tap_action, None);
    }

    #[test]
    fn tap_node_fails_with_migration_hint() {
        let error = parse_settings("layout \"us\" \"ru\"\ntap \"meta\"\n").unwrap_err();
        assert!(error.to_string().contains("bare-`Mod`"), "{error}");
    }

    #[test]
    fn duplicate_bare_mod_fails() {
        assert!(
            parse_settings("layout \"us\" \"ru\"\nbinds {\n Mod word\n Mod selection\n}\n")
                .is_err()
        );
    }

    #[test]
    fn bare_non_mod_fails() {
        assert!(parse_settings("layout \"us\" \"ru\"\nbinds {\n Shift word\n}\n").is_err());
    }

    #[test]
    fn modifier_key_expands_to_both_sides() {
        let settings =
            parse_settings("layout \"us\" \"ru\"\nbinds {\n Mod+Shift selection\n}\n").unwrap();
        assert_eq!(settings.binds.len(), 2);
        assert_eq!(
            settings.binds[0].scancode,
            evdev::KeyCode::KEY_LEFTSHIFT.code()
        );
        assert_eq!(
            settings.binds[1].scancode,
            evdev::KeyCode::KEY_RIGHTSHIFT.code()
        );
        assert!(settings.binds.iter().all(|bind| bind.mods.meta));
    }

    #[test]
    fn double_shift_line_sets_pair_base() {
        let settings = parse_settings("layout \"us\" \"ru\"\n").unwrap();
        assert_eq!(settings.pair_base, GestureKind::Word);
        let settings =
            parse_settings("layout \"us\" \"ru\"\nbinds {\n double-shift selection\n}\n").unwrap();
        assert_eq!(settings.pair_base, GestureKind::Selection);
        assert!(parse_settings("layout \"us\" \"ru\"\nbinds {\n double-shift turbo\n}\n").is_err());
        assert!(
            parse_settings(
                "layout \"us\" \"ru\"\nbinds {\n double-shift word\n double-shift word\n}\n"
            )
            .is_err()
        );
    }

    #[test]
    fn top_level_double_shift_fails_with_migration_hint() {
        let error = parse_settings("layout \"us\" \"ru\"\ndouble-shift selection\n").unwrap_err();
        assert!(error.to_string().contains("binds"), "{error}");
    }

    #[test]
    fn top_level_double_shift_hint_is_case_insensitive() {
        let error = parse_settings("layout \"us\" \"ru\"\nDouble-Shift selection\n").unwrap_err();
        assert!(error.to_string().contains("binds"), "{error}");
    }

    #[test]
    fn binds_accept_capitalized_double_shift() {
        let settings =
            parse_settings("layout \"us\" \"ru\"\nbinds {\n Double-Shift selection\n}\n").unwrap();
        assert_eq!(settings.pair_base, GestureKind::Selection);
    }

    #[test]
    fn binds_parse_all_actions() {
        let settings = parse_settings(
            "layout \"us\" \"ru\"\nbinds {\n Mod+L word\n Mod+S selection\n Mod+Shift+P phrase\n}\n",
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
        let settings = parse_settings("layout \"us\" \"ru\"\nbinds {\n mod+l word\n}\n").unwrap();
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
            let text = format!("layout \"us\" \"ru\"\n{block}");
            assert!(parse_settings(&text).is_err(), "{block}");
        }
    }

    #[test]
    fn chord_lines_fail_with_migration_hint() {
        let error =
            parse_settings("layout \"us\" \"ru\"\nchord \"meta+l\" \"word\"\n").unwrap_err();
        assert!(error.to_string().contains("binds"), "{error}");
    }

    #[test]
    fn timings_default_without_block() {
        let settings = parse_settings("layout \"us\" \"ru\"\n").unwrap();
        assert_eq!(settings.timing, TimingConfig::default());
    }

    #[test]
    fn timings_full_block_parses() {
        let settings = parse_settings(
            "layout \"us\" \"ru\"\ntimings {\n double-shift-ms 1000\n undo-ms 2000\n debounce-ms 10\n pending-ms 500\n tap-ms 200\n}\n",
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
        let settings = parse_settings("layout \"us\" \"ru\"\ntimings { tap-ms 200 }\n").unwrap();
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
            let text = format!("layout \"us\" \"ru\"\n{block}");
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
        std::fs::write(&path, "layout \"only-one\"\n").unwrap();
        let error = load_from(&path).unwrap_err();
        assert!(error.to_string().contains("bad-config-test.kdl"), "{error}");
        std::fs::remove_file(&path).ok();
    }
}
