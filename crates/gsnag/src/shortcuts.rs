use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

const BEGIN: &str = "<!-- gsnag:begin -->";
const END: &str = "<!-- gsnag:end -->";

#[derive(Args)]
pub struct Options {
    #[command(subcommand)]
    action: Action,
    /// Override the labwc rc.xml path (for a custom configuration or testing).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Do not ask the running compositor to reload its configuration.
    #[arg(long, global = true)]
    no_reload: bool,
}
#[derive(Subcommand)]
enum Action {
    Install,
    Uninstall,
    Status,
}

#[derive(Serialize, Deserialize)]
struct State {
    original: Option<String>,
    installed: String,
    block: String,
    replacements: Vec<(String, String)>,
    backup: PathBuf,
}

fn validate(text: &str) -> Result<roxmltree::Document<'_>> {
    let doc = roxmltree::Document::parse(text)
        .context("Invalid labwc XML; no configuration was changed")?;
    ensure!(
        matches!(
            doc.root_element().tag_name().name(),
            "labwc_config" | "openbox_config"
        ),
        "Expected labwc_config or openbox_config root"
    );
    Ok(doc)
}

fn canonical(key: &str) -> String {
    let mut parts: Vec<_> = key.split('-').collect();
    let name = parts.pop().unwrap_or_default().to_ascii_lowercase();
    let mut modifiers: Vec<_> = parts
        .iter()
        .map(|s| match s.to_ascii_lowercase().as_str() {
            "w" | "mod4" => "w".to_string(),
            other => other.to_string(),
        })
        .collect();
    modifiers.sort();
    format!("{}:{name}", modifiers.join("-"))
}

fn plan(original: Option<String>, base: &str, executable: &Path) -> Result<State> {
    ensure!(
        !base.contains("gsnag:"),
        "Existing gsnag markers found; inspect shortcuts status first"
    );
    let doc = validate(base)?;
    let root = doc.root_element();
    let keyboards: Vec<_> = root
        .children()
        .filter(|n| n.has_tag_name("keyboard"))
        .collect();
    ensure!(
        keyboards.len() <= 1,
        "Multiple keyboard sections are not supported; no changes made"
    );
    let bindings = [
        ("Print", "capture region --edit"),
        ("S-Print", "capture output --all --edit"),
        ("W-S-s", "capture region --edit"),
    ];
    let quoted = format!(
        "'{}'",
        executable
            .to_str()
            .context("Executable path must be UTF-8")?
            .replace('\'', "'\\''")
    );
    let mut block = format!("\n{BEGIN}\n");
    let mut replacements = Vec::new();
    let mut edits = Vec::new();
    if let Some(keyboard) = keyboards.first() {
        for node in keyboard.children().filter(|n| n.has_tag_name("keybind")) {
            if node.attribute("key").is_some_and(|k| {
                bindings
                    .iter()
                    .any(|(ours, _)| canonical(k) == canonical(ours))
            }) {
                let marker = format!("<!-- gsnag:replaced:{} -->", replacements.len());
                replacements.push((marker.clone(), base[node.range()].to_string()));
                edits.push((node.range(), marker));
            }
        }
    }
    let created_keyboard = keyboards.is_empty();
    if created_keyboard {
        block.push_str("<keyboard>\n<default/>\n");
    }
    for (key, args) in bindings {
        let command = format!("{quoted} {args}")
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        block.push_str(&format!("<keybind key=\"{key}\"><action name=\"Execute\"><command>{command}</command></action></keybind>\n"));
    }
    if created_keyboard {
        block.push_str("</keyboard>\n");
    }
    block.push_str(&format!("{END}\n"));
    let parent = keyboards.first().copied().unwrap_or(root);
    let range = parent.range();
    let source = &base[range.clone()];
    ensure!(
        !source.trim_end().ends_with("/>"),
        "Expand the self-closing {} element before installing shortcuts",
        parent.tag_name().name()
    );
    let insert = range.start + source.rfind("</").context("Missing closing XML element")?;
    edits.push((insert..insert, block.clone()));
    edits.sort_by_key(|(r, _)| r.start);
    let mut installed = base.to_string();
    for (range, text) in edits.into_iter().rev() {
        installed.replace_range(range, &text);
    }
    validate(&installed)?;
    Ok(State {
        original,
        installed,
        block,
        replacements,
        backup: PathBuf::new(),
    })
}

fn restore(current: &str, state: &State) -> Result<Option<String>> {
    if current == state.installed || state.original.as_deref() == Some(current) {
        return Ok(state.original.clone());
    }
    ensure!(
        current.matches(&state.block).count() == 1,
        "Managed shortcut block was edited; restore from {} or resolve the edits manually",
        state.backup.display()
    );
    let mut restored = current.replacen(&state.block, "", 1);
    for (marker, original) in &state.replacements {
        ensure!(
            restored.matches(marker).count() == 1,
            "A saved binding marker was changed; no changes made"
        );
        restored = restored.replacen(marker, original, 1);
    }
    validate(&restored)?;
    Ok(Some(restored))
}

fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp = tempfile::NamedTempFile::new_in(path.parent().context("Path has no parent")?)?;
    if let Ok(metadata) = fs::metadata(path) {
        tmp.as_file().set_permissions(metadata.permissions())?;
    }
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn run(options: Options) -> Result<()> {
    let path = match options.config {
        Some(p) => std::path::absolute(p)?,
        None => {
            let config = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
                });
            ensure!(
                config.is_absolute(),
                "HOME/XDG_CONFIG_HOME must identify an absolute configuration directory"
            );
            config.join("labwc/rc.xml")
        }
    };
    let state_path = path.with_file_name("gsnag-shortcuts.json");
    if matches!(options.action, Action::Status) {
        if let Some(saved) = read_optional(&state_path)? {
            let state: State = serde_json::from_str(&saved)?;
            let current = read_optional(&path)?.unwrap_or_default();
            ensure!(
                current.contains(&state.block),
                "Shortcut state exists, but bindings were changed or removed"
            );
            println!(
                "Installed in {}\nPrint: region\nShift+Print: all outputs\nSuper+Shift+S: region\nBackup: {}",
                path.display(),
                state.backup.display()
            );
        } else {
            println!("Not installed in {}", path.display());
        }
        return Ok(());
    }
    let parent = path.parent().context("Config path has no parent")?;
    fs::create_dir_all(parent)?;
    // OS lock is released even if the process crashes. Keep the harmless lock file.
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(parent.join("gsnag-shortcuts.lock"))?;
    lock.lock()?;
    ensure!(
        !fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()),
        "Refusing to replace a symlink; use --config with the real path"
    );
    let current = read_optional(&path)?;
    let saved = read_optional(&state_path)?;
    match options.action {
        Action::Install => {
            if let Some(saved) = saved {
                let state: State = serde_json::from_str(&saved)?;
                ensure!(
                    current.as_deref().is_some_and(|c| c.contains(&state.block)),
                    "Incomplete or modified installation; run uninstall before retrying"
                );
                println!("Shortcuts already installed");
                return Ok(());
            }
            let base = if let Some(text) = &current {
                text.clone()
            } else {
                let dirs = std::env::var("XDG_CONFIG_DIRS").unwrap_or_else(|_| "/etc/xdg".into());
                let system = dirs
                    .split(':')
                    .map(|p| Path::new(p).join("labwc/rc.xml"))
                    .find(|p| p.is_file());
                match system {
                    Some(p) => fs::read_to_string(p)?,
                    None => {
                        "<labwc_config>\n<keyboard><default/></keyboard>\n</labwc_config>\n".into()
                    }
                }
            };
            let exe = if Path::new("/usr/bin/gsnag").is_file() {
                PathBuf::from("/usr/bin/gsnag")
            } else {
                std::env::current_exe()?
            };
            let mut state = plan(current.clone(), &base, &exe)?;
            let mut backup = tempfile::Builder::new()
                .prefix("rc.xml.gsnag-backup-")
                .tempfile_in(parent)?;
            backup.write_all(base.as_bytes())?;
            backup.as_file().sync_all()?;
            state.backup = backup.keep()?.1;
            atomic(&state_path, &serde_json::to_vec_pretty(&state)?)?;
            ensure!(
                read_optional(&path)? == current,
                "Configuration changed during install; retry after uninstall"
            );
            atomic(&path, state.installed.as_bytes())?;
            println!(
                "Installed Print, Shift+Print and Super+Shift+S\nBackup: {}",
                state.backup.display()
            );
        }
        Action::Uninstall => {
            let Some(saved) = saved else {
                println!("Shortcuts are not installed");
                return Ok(());
            };
            let state: State = serde_json::from_str(&saved)?;
            let restored = if current == state.original {
                current.clone()
            } else {
                restore(
                    current.as_deref().context(
                        "Configuration was removed; inspect the backup before continuing",
                    )?,
                    &state,
                )?
            };
            ensure!(
                read_optional(&path)? == current,
                "Configuration changed during uninstall; retry"
            );
            match restored {
                Some(text) => atomic(&path, text.as_bytes())?,
                None => {
                    if path.exists() {
                        fs::remove_file(&path)?;
                    }
                }
            }
            fs::remove_file(&state_path)?;
            println!("Removed gsnag shortcuts; original bindings restored");
        }
        Action::Status => unreachable!(),
    }
    if !options.no_reload {
        let result = Command::new("labwc")
            .arg("--reconfigure")
            .status()
            .context("Changes saved, but could not run labwc --reconfigure")?;
        if !result.success() {
            bail!(
                "Changes saved, but labwc reload failed; run labwc --reconfigure in your desktop session"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restores_conflicts_exactly_and_preserves_unrelated_edits() {
        let base = "<labwc_config><keyboard><default/><keybind key='Print'><action name='Execute'><command>grim</command></action></keybind></keyboard><theme><name>A</name></theme></labwc_config>";
        let state = plan(Some(base.into()), base, Path::new("/tmp/a'b&c/gsnag")).unwrap();
        assert_eq!(
            restore(&state.installed, &state).unwrap().as_deref(),
            Some(base)
        );
        let edited = state.installed.replace("<name>A</name>", "<name>B</name>");
        assert_eq!(
            restore(&edited, &state).unwrap().unwrap(),
            base.replace("<name>A</name>", "<name>B</name>")
        );
        let doc = validate(&state.installed).unwrap();
        let command = doc
            .descendants()
            .find(|n| n.has_tag_name("command"))
            .unwrap()
            .text()
            .unwrap();
        assert!(command.starts_with("'/tmp/a'\\''b&c/gsnag' "));
    }
    #[test]
    fn refuses_malformed_or_modified_managed_configuration() {
        assert!(plan(None, "<broken>", Path::new("/bin/gsnag")).is_err());
        let base = "<labwc_config><keyboard><default/></keyboard></labwc_config>";
        let state = plan(None, base, Path::new("/bin/gsnag")).unwrap();
        assert!(restore(&state.installed.replace("--edit", "--cursor"), &state).is_err());
        assert!(plan(None, &state.installed, Path::new("/bin/gsnag")).is_err());
        assert_eq!(restore(&state.installed, &state).unwrap(), None);
    }
    #[test]
    fn adds_keyboard_when_missing_and_normalizes_modifier_order() {
        let base = "<labwc_config><theme/></labwc_config>";
        let state = plan(Some(base.into()), base, Path::new("/bin/gsnag")).unwrap();
        assert!(state.installed.contains("<default/>"));
        assert_eq!(restore(&state.installed, &state).unwrap().unwrap(), base);
        assert_eq!(canonical("S-Mod4-s"), canonical("W-S-s"));
    }

    #[test]
    fn supports_raspberry_pi_openbox_namespace() {
        let base = "<?xml version='1.0'?><openbox_config xmlns='http://openbox.org/3.4/rc'><keyboard><default/><keybind key='Print'/></keyboard></openbox_config>";
        let state = plan(None, base, Path::new("/usr/bin/gsnag")).unwrap();
        assert_eq!(state.replacements.len(), 1);
        assert_eq!(
            validate(&state.installed)
                .unwrap()
                .descendants()
                .filter(|n| n.has_tag_name("keyboard"))
                .count(),
            1
        );
        assert_eq!(restore(&state.installed, &state).unwrap(), None);
    }
}
