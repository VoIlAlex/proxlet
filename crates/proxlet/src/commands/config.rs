//! `proxlet config [KEY [VALUE]]`.

use proxlet_core::{Change, ConfigFile, Locations, SETTINGS, Settings, settings};

use super::{note_not_running, submit};
use crate::control::WireOutcome;
use crate::output::{self, BOLD, CmdResult, DIM};

/// Lists, reads or changes settings.
pub fn run(locations: &Locations, key: Option<&str>, value: Option<&str>) -> CmdResult {
    match (key, value) {
        (None, _) => list(locations),
        (Some(key), None) => {
            let def = settings::lookup(key)?;
            let (_, config) = ConfigFile::new(locations.config_file()).read()?;
            println!("{}", config.settings.get(def));
            Ok(())
        }
        (Some(key), Some(raw)) => set(locations, key, raw),
    }
}

fn list(locations: &Locations) -> CmdResult {
    let (_, config) = ConfigFile::new(locations.config_file()).read()?;
    let defaults = Settings::default();
    if !output::stdout_is_terminal() {
        for def in SETTINGS {
            println!("{}\t{}", def.key, config.settings.get(def));
        }
        return Ok(());
    }
    let key_w = SETTINGS.iter().map(|d| d.key.len()).max().unwrap_or(0);
    let val_w = SETTINGS
        .iter()
        .map(|d| config.settings.get(d).to_string().len())
        .max()
        .unwrap_or(0);
    for def in SETTINGS {
        let value = config.settings.get(def).to_string();
        if config.settings.is_changed(def) {
            anstream::println!(
                "{BOLD}{:<key_w$}{BOLD:#}  {BOLD}{value:<val_w$}{BOLD:#}  {DIM}* changed (default {}){DIM:#}",
                def.key,
                defaults.get(def)
            );
        } else {
            anstream::println!(
                "{:<key_w$}  {value:<val_w$}  {DIM}{}{DIM:#}",
                def.key,
                def.help
            );
        }
    }
    Ok(())
}

fn set(locations: &Locations, key: &str, raw: &str) -> CmdResult {
    let def = settings::lookup(key)?;
    let value = settings::Value::parse(def, raw)?;
    let submitted = submit(locations, &Change::SetSetting(def, value))?;
    let restart_needed = match &submitted.outcome {
        WireOutcome::SettingChanged {
            previous,
            restart_needed,
        } => {
            if *previous == value.to_string() {
                output::success(format!("{} is already {value}", def.key));
            } else {
                output::success(format!("{} = {value} (was {previous})", def.key));
            }
            *restart_needed
        }
        _ => false,
    };
    if submitted.live && restart_needed {
        output::warn("This setting takes effect after a restart: proxlet stop && proxlet start");
    }
    if !submitted.live {
        note_not_running();
    }
    Ok(())
}
