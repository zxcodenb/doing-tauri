//! Read-only legacy process/defaults adapters. No process termination, defaults writes, old Keychain or login-item migration.
use super::{
    legacy_preferences as prefs, problem,
    source::{Environment, PreferenceSnapshot},
};
use crate::error::CommandError;
use std::path::Path;
#[cfg(target_os = "macos")]
use std::path::PathBuf;

pub struct NativeEnvironment {
    pub app: Option<tauri::AppHandle>,
    pub allow_preferences: bool,
}
impl Environment for NativeEnvironment {
    fn check_legacy_stopped(&self) -> Result<(), CommandError> {
        #[cfg(target_os = "macos")]
        {
            use objc2_app_kit::NSWorkspace;
            for application in NSWorkspace::sharedWorkspace().runningApplications().iter() {
                if application.processIdentifier() as u32 == std::process::id() {
                    continue;
                }
                if application
                    .bundleIdentifier()
                    .is_some_and(|id| id.to_string() == prefs::DOMAIN)
                {
                    return Err(problem(
                        "legacyRunning",
                        "旧版 Doing 仍在运行，请手动退出后重试；不会自动结束进程",
                    ));
                }
            }
            // Also cover the unbundled SwiftPM Doing executable; no arguments (which might contain secrets) are read.
            let output = std::process::Command::new("/bin/ps")
                .args(["-axo", "pid=,comm="])
                .output()
                .map_err(|_| problem("processCheckFailed", "无法确认旧版是否退出，已停止导入"))?;
            if !output.status.success() {
                return Err(problem(
                    "processCheckFailed",
                    "无法确认旧版是否退出，已停止导入",
                ));
            }
            let text = std::str::from_utf8(&output.stdout)
                .map_err(|_| problem("processCheckFailed", "无法解析进程预检结果"))?;
            if legacy_executable_present(text, std::process::id()) {
                return Err(problem(
                    "legacyRunning",
                    "检测到旧版 Doing 可执行进程，请手动退出后重试",
                ));
            }
        }
        Ok(())
    }
    fn preferences(&self) -> Result<PreferenceSnapshot, CommandError> {
        if !self.allow_preferences {
            return Err(problem(
                "invalidSource",
                "只有本机旧版来源允许导入 UserDefaults 偏好",
            ));
        }
        #[cfg(target_os = "macos")]
        {
            use objc2_foundation::{NSString, NSUserDefaults};
            let defaults = NSUserDefaults::new();
            let domain = defaults.persistentDomainForName(&NSString::from_str(prefs::DOMAIN));
            let bytes = selected_domain_bytes(domain.as_deref())?;
            let backing = std::env::var_os("HOME").map(|home| {
                PathBuf::from(home)
                    .join("Library/Preferences")
                    .join(format!("{}.plist", prefs::DOMAIN))
            });
            PreferenceSnapshot::selected(&bytes, backing.as_deref())
        }
        #[cfg(not(target_os = "macos"))]
        Err(problem(
            "unsupportedPlatform",
            "此系统没有旧版 macOS UserDefaults 应用域",
        ))
    }
    fn geometry(&self) -> prefs::Geometry {
        #[cfg(target_os = "macos")]
        if let Some(app) = &self.app {
            if let Some(marker) = objc2::MainThreadMarker::new() {
                return geometry_on_main(app, marker);
            }
            let (send, receive) = std::sync::mpsc::sync_channel(1);
            let app_for_read = app.clone();
            if app
                .run_on_main_thread(move || {
                    if let Some(marker) = objc2::MainThreadMarker::new() {
                        let _ = send.send(geometry_on_main(&app_for_read, marker));
                    }
                })
                .is_ok()
            {
                if let Ok(value) = receive.recv_timeout(std::time::Duration::from_secs(5)) {
                    return value;
                }
            }
        }
        prefs::Geometry::default()
    }
}

/// Serialize only allowlisted values BEFORE they can be backed up. Kept separate from
/// NSUserDefaults access so the native serializer is testable without any real user domain.
#[cfg(target_os = "macos")]
fn selected_domain_bytes(
    domain: Option<
        &objc2_foundation::NSDictionary<objc2_foundation::NSString, objc2::runtime::AnyObject>,
    >,
) -> Result<Vec<u8>, CommandError> {
    use objc2_foundation::{
        NSDictionary, NSPropertyListFormat, NSPropertyListSerialization, NSString,
    };
    let pairs: Vec<_> = prefs::KEYS
        .iter()
        .filter_map(|name| {
            let key = NSString::from_str(name);
            domain
                .and_then(|dict| dict.objectForKey(&key))
                .map(|value| (key, value))
        })
        .collect();
    let keys: Vec<_> = pairs.iter().map(|(key, _)| &**key).collect();
    let values: Vec<_> = pairs.iter().map(|(_, value)| &**value).collect();
    let selected = NSDictionary::from_slices(&keys, &values);
    // SAFETY: immutable Foundation dictionary. UserDefaults values (and synthetic test
    // values) are property-list objects; serialization errors are handled, never ignored.
    let data = unsafe {
        NSPropertyListSerialization::dataWithPropertyList_format_options_error(
            &selected,
            NSPropertyListFormat::XMLFormat_v1_0,
            0,
        )
    }
    .map_err(|_| problem("invalidLegacyPreferences", "无法读取旧版偏好应用域"))?;
    Ok(data.to_vec())
}

#[cfg(any(target_os = "macos", test))]
fn legacy_executable_present(text: &str, own_pid: u32) -> bool {
    text.lines().any(|line| {
        let line = line.trim();
        let Some(split) = line.find(char::is_whitespace) else {
            return false;
        };
        let Ok(pid) = line[..split].parse::<u32>() else {
            return false;
        };
        pid != own_pid
            && Path::new(line[split..].trim())
                .file_name()
                .is_some_and(|name| name == "Doing")
    })
}

#[cfg(target_os = "macos")]
fn geometry_on_main(app: &tauri::AppHandle, marker: objc2::MainThreadMarker) -> prefs::Geometry {
    use objc2_app_kit::NSScreen;
    use tauri::Manager;
    let Some(window) = app.get_webview_window(crate::desktop::MAIN_WINDOW) else {
        return prefs::Geometry::default();
    };
    let (Ok(size), Ok(monitors)) = (window.outer_size(), window.available_monitors()) else {
        return prefs::Geometry::default();
    };
    let rect = |r: objc2_foundation::NSRect| prefs::Rect {
        x: r.origin.x,
        y: r.origin.y,
        width: r.size.width,
        height: r.size.height,
    };
    prefs::Geometry {
        displays: NSScreen::screens(marker)
            .iter()
            .map(|screen| prefs::Display {
                name: screen.localizedName().to_string(),
                cocoa: rect(screen.frame()),
                cocoa_work_area: rect(screen.visibleFrame()),
                scale: screen.backingScaleFactor(),
            })
            .collect(),
        monitors: monitors
            .iter()
            .map(|monitor| prefs::Monitor {
                name: monitor.name().cloned().unwrap_or_default(),
                physical: prefs::Rect {
                    x: monitor.position().x as f64,
                    y: monitor.position().y as f64,
                    width: monitor.size().width as f64,
                    height: monitor.size().height as f64,
                },
                scale: monitor.scale_factor(),
            })
            .collect(),
        window_logical_width: size.width as f64 / window.scale_factor().unwrap_or(1.0),
        window_logical_height: size.height as f64 / window.scale_factor().unwrap_or(1.0),
    }
}

pub fn free_space(directory: &Path) -> Result<u64, CommandError> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(directory.as_os_str().as_bytes())
            .map_err(|_| problem("migrationIO", "数据目录路径无效"))?;
        let mut info = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: path is NUL-terminated; statvfs initializes the output on success only.
        if unsafe { libc::statvfs(path.as_ptr(), info.as_mut_ptr()) } != 0 {
            return Err(problem("spaceCheckFailed", "无法预检可用磁盘空间"));
        }
        // SAFETY: statvfs returned success and initialized every field.
        let info = unsafe { info.assume_init() };
        u64::try_from(u128::from(info.f_bavail) * u128::from(info.f_frsize))
            .map_err(|_| problem("spaceCheckFailed", "可用磁盘空间超出支持范围"))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let directory: Vec<_> = directory.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut available = 0;
        // SAFETY: NUL-terminated path and valid output; unused optional outputs are null.
        if unsafe {
            windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
                directory.as_ptr(),
                &mut available,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(problem("spaceCheckFailed", "无法预检可用磁盘空间"));
        }
        Ok(available)
    }
    #[cfg(not(any(unix, windows)))]
    Err(problem("unsupportedPlatform", "此系统不支持迁移空间预检"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_preflight_matches_exact_executable_and_ignores_the_owned_pid() {
        let text = " 42 /Applications/Doing.app/Contents/MacOS/Doing\n43 /tmp/a folder/Doing\n44 /tmp/doing-desktop\n45 /tmp/Doing Helper\n";
        assert!(legacy_executable_present(text, 42));
        assert!(!legacy_executable_present(
            "42 /tmp/Doing\n43 /tmp/Doing Helper\n44 /tmp/doing-desktop",
            42
        ));
        assert!(legacy_executable_present("99 Doing", 42));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_dictionary_serializer_excludes_credentials_and_os_state_without_reading_defaults() {
        use objc2::runtime::AnyObject;
        use objc2_foundation::{NSDictionary, NSString};
        let keys: Vec<_> = [
            "mode",
            "appearance",
            "tokens",
            "launchAtLogin",
            "notificationAuthorization",
        ]
        .map(NSString::from_str)
        .into();
        let values: Vec<_> = [
            "panel",
            "dark",
            "synthetic-never-back-up",
            "enabled",
            "granted",
        ]
        .map(NSString::from_str)
        .into();
        let names: Vec<_> = keys.iter().map(|key| &**key).collect();
        let objects: Vec<&AnyObject> = values.iter().map(|value| -> &AnyObject { value }).collect();
        let domain = NSDictionary::from_slices(&names, &objects);
        let bytes = selected_domain_bytes(Some(&domain)).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("synthetic-never-back-up"));
        let parsed = plist::Value::from_reader(std::io::Cursor::new(&bytes)).unwrap();
        let dict = parsed.as_dictionary().unwrap();
        assert_eq!(dict.len(), 2);
        assert_eq!(dict["mode"].as_string(), Some("panel"));
        assert_eq!(dict["appearance"].as_string(), Some("dark"));
        let empty = selected_domain_bytes(None).unwrap();
        assert!(plist::Value::from_reader(std::io::Cursor::new(empty))
            .unwrap()
            .as_dictionary()
            .unwrap()
            .is_empty());
    }
}
