use serde::Serialize;
use theseus::{InstallSource, PackageFrontend};

/// How this installation should be updated, for the frontend's update UI.
///
/// This is deliberately independent of the `updater` feature: development
/// builds compile the no-op updater, but the settings panel still needs to
/// know that, for example, an AUR-installed launcher is updated by pacman.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCapability {
    source: &'static str,
    /// Whether the launcher may update itself: either it owns its files or it
    /// can drive the package manager that owns them.
    self_update: bool,
    /// Shell command users can run to update, when one can be named safely.
    command: Option<String>,
    /// Claimed package names when several databases claim this executable.
    owners: Vec<String>,
}

pub fn current() -> UpdateCapability {
    let source = theseus::detect_install_source();
    UpdateCapability {
        source: source.as_str(),
        self_update: source.self_update_supported(),
        command: update_command(source),
        owners: conflict_owners(source),
    }
}

fn update_command(source: &InstallSource) -> Option<String> {
    match source {
        InstallSource::Managed(package) => {
            if !is_safe_package_name(&package.name) {
                return None;
            }
            Some(match package.frontend? {
                PackageFrontend::Apt => {
                    format!("sudo apt install --only-upgrade {}", package.name)
                }
                PackageFrontend::Dnf => {
                    format!("sudo dnf upgrade {}", package.name)
                }
                PackageFrontend::Zypper => {
                    format!("sudo zypper update {}", package.name)
                }
                PackageFrontend::Pacman => {
                    // `pacman -S` cannot see an update published after the
                    // last database refresh, while `-Syu` would upgrade every
                    // package on the system; refresh and upgrade only the
                    // launcher.
                    format!("sudo pacman -Sy {}", package.name)
                }
                PackageFrontend::Yay => format!("yay -S {}", package.name),
                PackageFrontend::Paru => format!("paru -S {}", package.name),
            })
        }
        // The app ID comes from the environment, so only accept the
        // restricted character set Flatpak IDs may contain before showing a
        // command the user is likely to paste into a shell.
        InstallSource::Flatpak => Some(
            std::env::var("FLATPAK_ID")
                .ok()
                .filter(|id| is_valid_flatpak_id(id))
                .map(|id| format!("flatpak update {id}"))
                .unwrap_or_else(|| "flatpak update".to_string()),
        ),
        _ => None,
    }
}

fn conflict_owners(source: &InstallSource) -> Vec<String> {
    match source {
        InstallSource::Conflict(packages) => packages
            .iter()
            .map(|package| {
                format!("{} ({})", package.name, package.database.as_str())
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Package names are copied into a command the user is told to run, so
/// anything outside the character set package databases use is rejected.
fn is_safe_package_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+')
        })
}

fn is_valid_flatpak_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 255
        && id.contains('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Reading the capability runs the local package-database queries, which can
/// take a few hundred milliseconds on a large system, so it runs off the main
/// thread.
#[tauri::command]
pub async fn get_update_capability() -> UpdateCapability {
    match tauri::async_runtime::spawn_blocking(current).await {
        Ok(capability) => capability,
        Err(error) => {
            tracing::warn!("Failed to read the update capability: {error}");
            UpdateCapability {
                source: "unknown",
                self_update: false,
                command: None,
                owners: Vec::new(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{is_safe_package_name, is_valid_flatpak_id, update_command};
    use theseus::{
        InstallSource, OwnedPackage, PackageDatabase, PackageFrontend,
    };

    fn managed(frontend: PackageFrontend, name: &str) -> InstallSource {
        let database = match frontend {
            PackageFrontend::Apt => PackageDatabase::Dpkg,
            PackageFrontend::Dnf | PackageFrontend::Zypper => {
                PackageDatabase::Rpm
            }
            PackageFrontend::Pacman
            | PackageFrontend::Yay
            | PackageFrontend::Paru => PackageDatabase::Pacman,
        };
        InstallSource::Managed(OwnedPackage {
            database,
            frontend: Some(frontend),
            name: name.to_string(),
            version: None,
            architecture: None,
        })
    }

    #[test]
    fn pacman_refreshes_the_database_for_the_launcher_only() {
        // `-Syu` would upgrade every package, and `-S` alone cannot see an
        // update published after the last refresh.
        assert_eq!(
            update_command(&managed(
                PackageFrontend::Pacman,
                "axolotl-launcher-bin"
            ))
            .as_deref(),
            Some("sudo pacman -Sy axolotl-launcher-bin")
        );
        assert_eq!(
            update_command(&managed(
                PackageFrontend::Yay,
                "axolotl-launcher-bin"
            ))
            .as_deref(),
            Some("yay -S axolotl-launcher-bin")
        );
        assert_eq!(
            update_command(&managed(
                PackageFrontend::Paru,
                "axolotl-launcher-bin"
            ))
            .as_deref(),
            Some("paru -S axolotl-launcher-bin")
        );
    }

    #[test]
    fn other_frontends_upgrade_the_named_package() {
        assert_eq!(
            update_command(&managed(PackageFrontend::Apt, "axolotl"))
                .as_deref(),
            Some("sudo apt install --only-upgrade axolotl")
        );
        assert_eq!(
            update_command(&managed(PackageFrontend::Dnf, "axolotl"))
                .as_deref(),
            Some("sudo dnf upgrade axolotl")
        );
        assert_eq!(
            update_command(&managed(PackageFrontend::Zypper, "axolotl"))
                .as_deref(),
            Some("sudo zypper update axolotl")
        );
    }

    #[test]
    fn a_source_without_a_frontend_or_command_stays_silent() {
        let no_frontend = InstallSource::Managed(OwnedPackage {
            database: PackageDatabase::Dpkg,
            frontend: None,
            name: "axolotl".to_string(),
            version: None,
            architecture: None,
        });
        for source in [
            no_frontend,
            InstallSource::Portable,
            InstallSource::AppImage,
            InstallSource::Nix,
            InstallSource::Unconfirmed,
            InstallSource::QueryFailed,
            InstallSource::Conflict(Vec::new()),
        ] {
            assert_eq!(update_command(&source), None);
        }
    }

    #[test]
    fn a_package_name_that_could_escape_the_command_is_rejected() {
        assert!(!is_safe_package_name("axolotl; rm -rf ~"));
        assert!(!is_safe_package_name("axolotl && reboot"));
        assert!(!is_safe_package_name(""));
        assert!(is_safe_package_name("axolotl-launcher-bin"));
        assert_eq!(
            update_command(&managed(PackageFrontend::Pacman, "axolotl;reboot")),
            None
        );
    }

    #[test]
    fn flatpak_ids_must_look_like_an_application_id() {
        assert!(is_valid_flatpak_id("red.ghs.axolotl"));
        assert!(!is_valid_flatpak_id("axolotl"));
        assert!(!is_valid_flatpak_id("red.ghs.axolotl; reboot"));
    }
}
