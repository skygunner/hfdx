//! Explicit portable toast identity preparation. Never called by notification delivery.
use super::{NativePlatform, NotificationError};

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Frontend {
    Gui,
    Tui,
}
#[cfg(any(windows, test))]
impl Frontend {
    fn from_executable(executable: &std::path::Path) -> Result<Self, NotificationError> {
        match executable.file_name().and_then(|name| name.to_str()) {
            Some("hfdx-gui.exe") => Ok(Self::Gui),
            Some("hfdx.exe") => Ok(Self::Tui),
            _ => Err(NotificationError::IdentityUnavailable),
        }
    }
    fn app_id(self) -> &'static str {
        match self {
            Self::Gui => "Hfdx.Hfdx.GUI",
            Self::Tui => "Hfdx.Hfdx.TUI",
        }
    }
    fn shortcut_name(self) -> &'static str {
        match self {
            Self::Gui => "Hfdx GUI.lnk",
            Self::Tui => "Hfdx TUI.lnk",
        }
    }
}

/// Call before presenting UI. `false` only reads registration and sets process identity.
/// Only an explicit install/user action may pass `true`; it never replaces a shortcut.
/// A timed-out Windows call can still finish: do not automatically retry registration.
pub async fn prepare_portable_identity(
    register: bool,
) -> Result<NativePlatform, NotificationError> {
    #[cfg(windows)]
    {
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            tokio::task::spawn_blocking(move || {
                let app_id = prepare_windows(register)?;
                Ok(NativePlatform::new(Some(app_id.to_owned())))
            }),
        )
        .await
        .map_err(|_| NotificationError::TimedOut)?
        .map_err(|_| NotificationError::IdentityUnavailable)?
    }
    #[cfg(not(windows))]
    {
        if register {
            return Err(NotificationError::UnsupportedPlatform);
        }
        Ok(NativePlatform::new(None))
    }
}

#[cfg(windows)]
fn prepare_windows(register: bool) -> Result<&'static str, NotificationError> {
    use std::{os::windows::ffi::OsStringExt, path::PathBuf};
    use windows::{
        Win32::{
            Storage::EnhancedStorage::PKEY_AppUserModel_ID,
            System::{
                Com::{
                    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance,
                    CoInitializeEx, CoTaskMemFree, CoUninitialize, IPersistFile, STGM_READ,
                    StructuredStorage::{PROPVARIANT, PVCHF_DEFAULT, PropVariantChangeType},
                },
                Variant::VT_LPWSTR,
            },
            UI::Shell::{
                FOLDERID_Programs, IShellLinkW, KF_FLAG_DEFAULT, PropertiesSystem::IPropertyStore,
                SHGetKnownFolderPath, SetCurrentProcessExplicitAppUserModelID, ShellLink,
            },
        },
        core::{BSTR, HSTRING, Interface, PCWSTR},
    };
    let executable = std::env::current_exe()
        .and_then(|path| path.canonicalize())
        .map_err(|_| NotificationError::IdentityUnavailable)?;
    let frontend = Frontend::from_executable(&executable)?;
    let expected_app_id = frontend.app_id();
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            // SAFETY: guard exists only after successful COM initialization on this same thread.
            unsafe { CoUninitialize() };
        }
    }
    // SAFETY: no reserved pointer; COM objects never leave this owned blocking thread.
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
        .ok()
        .map_err(|_| NotificationError::IdentityUnavailable)?;
    let _apartment = Apartment;
    let prepare = || -> windows::core::Result<()> {
        // SAFETY: known-folder GUID/current user; successful call returns task-allocated UTF-16.
        let raw = unsafe { SHGetKnownFolderPath(&FOLDERID_Programs, KF_FLAG_DEFAULT, None) }?;
        // SAFETY: successful API result is NUL-terminated; copy before freeing the allocation.
        let folder = unsafe { raw.to_string() };
        // SAFETY: exactly the returned allocation; freed once, including string conversion failure.
        unsafe { CoTaskMemFree(Some(raw.0.cast())) };
        let folder = PathBuf::from(folder?);
        let shortcut = folder.join(frontend.shortcut_name());
        let shortcut_string = HSTRING::from(shortcut.as_os_str());
        // SAFETY: in-process ShellLink COM class, no aggregation, generated typed interface.
        let link: IShellLinkW =
            unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }?;
        let persist: IPersistFile = link.cast()?;
        let properties: IPropertyStore = link.cast()?;
        if shortcut.try_exists()? {
            // SAFETY: valid COM interface and NUL-terminated path live through synchronous call.
            unsafe { persist.Load(PCWSTR(shortcut_string.as_ptr()), STGM_READ) }?;
            // SAFETY: fixed metadata key; binding returns an initialized owned variant.
            let app_id = unsafe { properties.GetValue(&PKEY_AppUserModel_ID) }?;
            let mut target = [0u16; 32768];
            // SAFETY: bounded initialized output; optional find-data pointer is null by API contract.
            unsafe { link.GetPath(&mut target, std::ptr::null_mut(), 0) }?;
            let end = target.iter().position(|value| *value == 0).ok_or_else(|| {
                windows::core::Error::from_hresult(windows::core::HRESULT::from_win32(122))
            })?;
            let target = PathBuf::from(std::ffi::OsString::from_wide(&target[..end]));
            if BSTR::try_from(&app_id)?.to_string() != expected_app_id
                || target.canonicalize()? != executable
            {
                return Err(windows::core::Error::from_hresult(
                    windows::core::HRESULT::from_win32(183),
                ));
            }
        } else {
            if !register {
                return Err(windows::core::Error::from_hresult(
                    windows::core::HRESULT::from_win32(1168),
                ));
            }
            // The exclusive staging directory is removed by RAII on any failure.
            // Publish directly in Programs: no persistent directory is created by validation.
            let staging = tempfile::Builder::new()
                .prefix("hfdx-identity-")
                .tempdir_in(&folder)?;
            let staged_link = staging.path().join(frontend.shortcut_name());
            let target = HSTRING::from(executable.as_os_str());
            // SAFETY: retained terminated executable string, no interpolated command or arguments.
            unsafe { link.SetPath(PCWSTR(target.as_ptr())) }?;
            let source = PROPVARIANT::from(expected_app_id);
            let mut app_id = PROPVARIANT::default();
            // SAFETY: distinct valid variants; Shell property requires a string value.
            unsafe { PropVariantChangeType(&mut app_id, &source, PVCHF_DEFAULT, VT_LPWSTR) }?;
            // SAFETY: fixed property key and initialized variant, retained through the call.
            unsafe { properties.SetValue(&PKEY_AppUserModel_ID, &app_id) }?;
            // SAFETY: commits the new in-memory property store, not an existing shortcut.
            unsafe { properties.Commit() }?;
            let staged_string = HSTRING::from(staged_link.as_os_str());
            // SAFETY: exclusive staging directory owns this new path; never saves over final path.
            unsafe { persist.Save(PCWSTR(staged_string.as_ptr()), true) }?;
            // Same-volume publication is atomic and fails if another installer created the path.
            std::fs::hard_link(&staged_link, &shortcut)?;
        }
        let app_id = HSTRING::from(expected_app_id);
        // SAFETY: fixed terminated identifier; caller awaits before creating any UI.
        unsafe { SetCurrentProcessExplicitAppUserModelID(PCWSTR(app_id.as_ptr())) }?;
        Ok(())
    };
    prepare().map(|()| expected_app_id).map_err(|error| {
        if error.code() == windows::core::HRESULT::from_win32(183)
            || error.code() == windows::core::HRESULT::from_win32(80)
        {
            NotificationError::IdentityConflict
        } else {
            NotificationError::IdentityUnavailable
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn frontend_identities_have_independent_machine_values() {
        let cases = [
            (
                "/portable/hfdx-gui.exe",
                Frontend::Gui,
                "Hfdx.Hfdx.GUI",
                "Hfdx GUI.lnk",
            ),
            (
                "/portable/hfdx.exe",
                Frontend::Tui,
                "Hfdx.Hfdx.TUI",
                "Hfdx TUI.lnk",
            ),
        ];
        for (executable, expected, app_id, shortcut) in cases {
            let frontend = Frontend::from_executable(Path::new(executable)).unwrap();
            assert_eq!(frontend, expected);
            assert_eq!(frontend.app_id(), app_id);
            assert_eq!(frontend.shortcut_name(), shortcut);
        }
        assert_ne!(Frontend::Gui.app_id(), Frontend::Tui.app_id());
        assert_ne!(Frontend::Gui.shortcut_name(), Frontend::Tui.shortcut_name());
    }

    #[test]
    fn unrecognized_executables_cannot_claim_an_identity() {
        for executable in [
            "",
            "/",
            "/portable/other.exe",
            "/portable/hfdx-gui",
            "/portable/hfdx.exe.bak",
            "/portable/hfdx-gui.exe.exe",
        ] {
            assert_eq!(
                Frontend::from_executable(Path::new(executable)),
                Err(NotificationError::IdentityUnavailable)
            );
        }
    }
}
