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

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Default config written by `setup`. Never overwrites an existing file.
pub const DEFAULT_CONFIG: &str = r#"// niri-punto config. The ordered layout pair: position maps to the niri
// layout index, so the order must match the `layout` line in your niri
// config. `setup` never overwrites this file once created.
layouts "us" "ru"
"#;

/// File name of the udev rule, shared with `setup` and `doctor`.
pub const RULE_FILE_NAME: &str = "99-niri-punto.rules";

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
    let text = std::fs::read_to_string(path)
        .map_err(|error| ConfigError::Io(format!("cannot read {}: {error}", path.display())))?;
    parse(&text).map_err(|error| match error {
        ConfigError::Parse(_)
        | ConfigError::MissingLayouts
        | ConfigError::DuplicateNode
        | ConfigError::BadArity { .. }
        | ConfigError::EmptyCode
        | ConfigError::Duplicate(_) => ConfigError::Io(format!("{}: {error}", path.display())),
        ConfigError::Io(_) => error,
    })
}

/// Read and parse the default-location config.
pub fn load() -> Result<LayoutPair, ConfigError> {
    load_from(&config_path())
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
