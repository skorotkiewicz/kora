use std::{collections::HashMap, env, ffi::OsString, fs, path::PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WallpaperMode {
    #[default]
    Transparent,
    Replace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub path: PathBuf,
    pub wallpaper_mode: WallpaperMode,
    pub wallpaper_image: Option<PathBuf>,
    pub background_color: String,
    pub icon_size: i32,
}

#[derive(Debug, Clone)]
pub struct Config {
    home: PathBuf,
    defaults: Settings,
    monitors: HashMap<String, PartialSettings>,
}

#[derive(Debug, Clone, Default)]
struct PartialSettings {
    path: Option<PathBuf>,
    wallpaper_mode: Option<WallpaperMode>,
    wallpaper_image: Option<Option<PathBuf>>,
    background_color: Option<String>,
    icon_size: Option<i32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    defaults: RawSettings,
    #[serde(default)]
    monitors: HashMap<String, RawSettings>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSettings {
    path: Option<String>,
    wallpaper_mode: Option<WallpaperMode>,
    wallpaper_image: Option<String>,
    background_color: Option<String>,
    icon_size: Option<i32>,
}

impl Config {
    pub fn home(&self) -> &std::path::Path {
        &self.home
    }

    pub fn effective(&self, connector: Option<&str>) -> Settings {
        let mut settings = self.defaults.clone();
        let Some(overrides) = connector.and_then(|name| self.monitors.get(name)) else {
            return settings;
        };
        apply(&mut settings, overrides);
        settings
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Run(Option<PathBuf>),
    Help,
    Version,
}

pub fn command(args: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut args = args.into_iter();
    args.next();
    let mut config = None;
    while let Some(argument) = args.next() {
        if argument == "--help" || argument == "-h" {
            return Ok(Command::Help);
        } else if argument == "--version" || argument == "-V" {
            return Ok(Command::Version);
        } else if argument == "--config" {
            if config.is_some() {
                return Err("--config may be specified only once".into());
            }
            config = Some(PathBuf::from(
                args.next().ok_or("--config requires a path")?,
            ));
        } else {
            return Err(format!("unknown argument: {}", argument.to_string_lossy()));
        }
    }
    Ok(Command::Run(config))
}

pub fn load(explicit: Option<PathBuf>) -> Result<Config, String> {
    let home = env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or("HOME is not set")?;
    let xdg = env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    load_from(explicit, &home, xdg)
}

fn load_from(
    explicit: Option<PathBuf>,
    home: &std::path::Path,
    xdg: Option<PathBuf>,
) -> Result<Config, String> {
    let explicit_file = explicit.is_some();
    let path = explicit.unwrap_or_else(|| {
        xdg.unwrap_or_else(|| home.join(".config"))
            .join("kora/config.toml")
    });
    let source = match fs::read_to_string(&path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !explicit_file => {
            return Ok(default_config(home));
        }
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    parse(&source, &path, home)
}

fn default_config(home: &std::path::Path) -> Config {
    Config {
        home: home.to_path_buf(),
        defaults: Settings {
            path: home.to_path_buf(),
            wallpaper_mode: WallpaperMode::Transparent,
            wallpaper_image: None,
            background_color: "#202020".into(),
            icon_size: 48,
        },
        monitors: HashMap::new(),
    }
}

fn parse(source: &str, path: &std::path::Path, home: &std::path::Path) -> Result<Config, String> {
    let raw: RawConfig = toml::from_str(source).map_err(|error| {
        let line = error.span().map(|span| {
            source[..span.start]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count()
                + 1
        });
        match line {
            Some(line) => format!("{}:{line}: {error}", path.display()),
            None => format!("{}: {error}", path.display()),
        }
    })?;
    let directory = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let defaults = resolve_partial(raw.defaults, "defaults", directory, home)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let mut config = default_config(home);
    apply(&mut config.defaults, &defaults);
    for (name, settings) in raw.monitors {
        config.monitors.insert(
            name.clone(),
            resolve_partial(settings, &format!("monitors.{name}"), directory, home)
                .map_err(|error| format!("{}: {error}", path.display()))?,
        );
    }
    Ok(config)
}

fn resolve_partial(
    raw: RawSettings,
    context: &str,
    directory: &std::path::Path,
    home: &std::path::Path,
) -> Result<PartialSettings, String> {
    let path = raw
        .path
        .map(|path| {
            if path.is_empty() {
                Err(format!("{context}.path must not be empty"))
            } else {
                Ok(resolve_path(&path, directory, home))
            }
        })
        .transpose()?;
    let wallpaper_image = raw.wallpaper_image.map(|image| {
        if image.is_empty() {
            None
        } else {
            Some(resolve_path(&image, directory, home))
        }
    });
    if let Some(color) = &raw.background_color
        && !valid_color(color)
    {
        return Err(format!(
            "{context}.background_color must use #RRGGBB format"
        ));
    }
    if let Some(size) = raw.icon_size
        && !(24..=128).contains(&size)
    {
        return Err(format!(
            "{context}.icon_size must be an integer from 24 to 128"
        ));
    }
    Ok(PartialSettings {
        path,
        wallpaper_mode: raw.wallpaper_mode,
        wallpaper_image,
        background_color: raw.background_color,
        icon_size: raw.icon_size,
    })
}

fn resolve_path(value: &str, directory: &std::path::Path, home: &std::path::Path) -> PathBuf {
    if let Some(relative) = value.strip_prefix("~/") {
        home.join(relative)
    } else {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            path
        } else {
            directory.join(path)
        }
    }
}

fn valid_color(value: &str) -> bool {
    value.len() == 7
        && value.starts_with('#')
        && value[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn apply(settings: &mut Settings, overrides: &PartialSettings) {
    if let Some(path) = &overrides.path {
        settings.path = path.clone();
    }
    if let Some(mode) = overrides.wallpaper_mode {
        settings.wallpaper_mode = mode;
    }
    if let Some(image) = &overrides.wallpaper_image {
        settings.wallpaper_image = image.clone();
    }
    if let Some(color) = &overrides.background_color {
        settings.background_color = color.clone();
    }
    if let Some(size) = overrides.icon_size {
        settings.icon_size = size;
    }
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, fs};

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let path = env::temp_dir().join(format!("kora-{name}-{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn parses_config_argument() {
        assert_eq!(
            command([
                OsString::from("kora"),
                OsString::from("--config"),
                OsString::from("a.toml")
            ])
            .unwrap(),
            Command::Run(Some(PathBuf::from("a.toml")))
        );
        assert!(command([OsString::from("kora"), OsString::from("--config")]).is_err());
        assert_eq!(
            command(["kora".into(), "--help".into()]).unwrap(),
            Command::Help
        );
        assert_eq!(
            command(["kora".into(), "--version".into()]).unwrap(),
            Command::Version
        );
        assert!(command(["kora".into(), "--unknown".into()]).is_err());
        assert!(
            command([
                "kora".into(),
                "--config".into(),
                "a".into(),
                "--config".into(),
                "b".into()
            ])
            .is_err()
        );
    }

    #[test]
    fn missing_default_uses_home_but_missing_explicit_fails() {
        let home = temp_dir("missing");
        let config = load_from(None, &home, Some(home.join("xdg"))).unwrap();
        assert_eq!(config.effective(None).path, home);
        assert!(load_from(Some(home.join("missing.toml")), &home, None).is_err());
    }

    #[test]
    fn rejects_unknown_and_invalid_values_with_location() {
        let home = temp_dir("invalid");
        let path = home.join("config.toml");
        let unknown = parse("[defaults]\nextra = true", &path, &home).unwrap_err();
        assert!(unknown.contains("config.toml:2"));
        assert!(parse("[defaults]\nwallpaper_mode = \"other\"", &path, &home).is_err());
        assert!(parse("[defaults]\nbackground_color = \"red\"", &path, &home).is_err());
        assert!(parse("[defaults]\npath = \"\"", &path, &home).is_err());
    }

    #[test]
    fn resolves_only_documented_path_forms() {
        let home = temp_dir("paths");
        let path = home.join("config/config.toml");
        let config = parse(
            "[defaults]\npath = \"~/Desktop\"\nwallpaper_image = \"images/$HOME;still-literal.png\"",
            &path,
            &home,
        )
        .unwrap();
        let settings = config.effective(None);
        assert_eq!(settings.path, home.join("Desktop"));
        assert_eq!(
            settings.wallpaper_image,
            Some(home.join("config/images/$HOME;still-literal.png"))
        );
    }

    #[test]
    fn icon_size_defaults_inherits_and_validates() {
        let home = PathBuf::from("/home/test");
        let path = home.join("config.toml");
        assert_eq!(default_config(&home).effective(None).icon_size, 48);
        assert_eq!(
            parse("", &path, &home).unwrap().effective(None).icon_size,
            48
        );
        let config = parse(
            "[defaults]\nicon_size = 64\n[monitors.DP-1]\nicon_size = 96\n[monitors.HDMI-A-1]\npath = 'Desktop'",
            &path, &home,
        ).unwrap();
        assert_eq!(config.effective(None).icon_size, 64);
        assert_eq!(config.effective(Some("DP-1")).icon_size, 96);
        assert_eq!(config.effective(Some("HDMI-A-1")).icon_size, 64);
        assert_eq!(config.effective(Some("UNKNOWN")).icon_size, 64);
        for size in [24, 48, 128] {
            assert_eq!(
                parse(&format!("[defaults]\nicon_size = {size}"), &path, &home)
                    .unwrap()
                    .effective(None)
                    .icon_size,
                size
            );
        }
        for value in [
            "0",
            "23",
            "129",
            "-1",
            "48.5",
            "'large'",
            "true",
            "2147483648",
        ] {
            for section in ["defaults", "monitors.DP-1"] {
                let error =
                    parse(&format!("[{section}]\nicon_size = {value}"), &path, &home).unwrap_err();
                assert!(error.contains("config.toml"));
                assert!(error.contains("icon_size"));
            }
        }
    }

    #[test]
    fn example_config_parses() {
        let home = temp_dir("example");
        parse(
            include_str!("../config.example.toml"),
            &home.join("config.toml"),
            &home,
        )
        .unwrap();
    }

    #[test]
    fn monitor_overrides_inherit_and_can_clear_image() {
        let home = temp_dir("overrides");
        let path = home.join("config.toml");
        let config = parse(
            "[defaults]\npath = \"Desktop\"\nwallpaper_mode = \"replace\"\nwallpaper_image = \"wall.png\"\nbackground_color = \"#112233\"\n\n[monitors.DP-1]\npath = \"Projects\"\n\n[monitors.HDMI-A-1]\nwallpaper_image = \"\"",
            &path,
            &home,
        )
        .unwrap();
        let dp = config.effective(Some("DP-1"));
        assert_eq!(dp.path, home.join("Projects"));
        assert_eq!(dp.wallpaper_mode, WallpaperMode::Replace);
        assert_eq!(dp.wallpaper_image, Some(home.join("wall.png")));
        let hdmi = config.effective(Some("HDMI-A-1"));
        assert_eq!(hdmi.path, home.join("Desktop"));
        assert_eq!(hdmi.wallpaper_image, None);
        assert_eq!(config.effective(Some("UNKNOWN")), config.effective(None));
    }
}
