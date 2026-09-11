use super::*;
use ::windows::core::{HRESULT, HSTRING};
use ::windows::Data::Xml::Dom::XmlDocument;
use ::windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};
use ::windows::UI::Notifications::{
    NotificationSetting, ToastNotification, ToastNotificationManager,
};

struct Apartment(bool);
impl Apartment {
    fn new() -> Result<Self, Error> {
        // SAFETY: balanced per-thread WinRT initialization. An existing STA is usable;
        // do not uninitialize an apartment we did not initialize.
        match unsafe { RoInitialize(RO_INIT_MULTITHREADED) } {
            Ok(()) => Ok(Self(true)),
            Err(e) if e.code() == HRESULT(0x80010106u32 as i32) => Ok(Self(false)),
            Err(_) => Err(Error::Unavailable),
        }
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe { RoUninitialize() }
        }
    }
}
pub(super) struct Native {
    identity: Identity,
}
impl Native {
    pub fn new(identity: Identity) -> Self {
        Self { identity }
    }
    pub fn permission(&self, _request: bool) -> Result<Permission, Error> {
        let _apartment = Apartment::new()?;
        let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(
            &self.identity.application_id,
        ))
        .map_err(|_| Error::Unavailable)?;
        let setting = notifier.Setting().map_err(|_| Error::Unavailable)?;
        Ok(if setting == NotificationSetting::Enabled {
            Permission::Granted
        } else {
            Permission::Denied
        })
    }
    pub fn submit(&self, request: Request<'_>) -> Result<(), Error> {
        let _apartment = Apartment::new()?;
        let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(
            &self.identity.application_id,
        ))
        .map_err(|_| Error::Unavailable)?;
        if notifier.Setting().map_err(|_| Error::Unavailable)? != NotificationSetting::Enabled {
            return Err(Error::PermissionDenied);
        }
        let document = XmlDocument::new().map_err(|_| Error::Unavailable)?;
        document
            .LoadXml(&HSTRING::from(toast_xml(&self.identity, &request)?))
            .map_err(|_| Error::InvalidRequest)?;
        let toast = ToastNotification::CreateToastNotification(&document)
            .map_err(|_| Error::Unavailable)?;
        // Both fields are <=16 characters even on older supported Windows notification APIs.
        // The pair preserves all 128 ID bits and replaces only this exact request on retries.
        let id = request.id.simple().to_string();
        toast
            .SetTag(&HSTRING::from(&id[..16]))
            .map_err(|_| Error::Unavailable)?;
        toast
            .SetGroup(&HSTRING::from(&id[16..]))
            .map_err(|_| Error::Unavailable)?;
        // Protocol activation is handled by the installed scheme + Tauri single-instance
        // forwarding, including a cold start; no transient WinRT event callback is relied on.
        notifier.Show(&toast).map_err(|_| Error::Unavailable)
    }
}
