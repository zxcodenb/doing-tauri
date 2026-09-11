//! Native OS acceptance, real permission state and opaque activation identities.
//! No task persistence, credentials, Tauri handle, arbitrary URL launching or OS impersonation.
use std::sync::Arc;
use uuid::Uuid;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    Granted,
    Denied,
    NotDetermined,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("native notifications are unavailable")]
    Unavailable,
    #[error("notification permission was denied")]
    PermissionDenied,
    #[error("a matching application bundle is required")]
    BundleRequired,
    #[error("invalid notification request")]
    InvalidRequest,
    #[error("native notification operation timed out; outcome may be unknown")]
    Timeout,
}

#[derive(Clone)]
pub struct Identity {
    pub application_id: String,
    pub activation_scheme: String,
}
impl Identity {
    fn validate(&self) -> Result<(), Error> {
        if self.application_id.is_empty()
            || self.application_id.len() > 200
            || !self
                .application_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b".-_".contains(&c))
            || self.activation_scheme.is_empty()
            || self.activation_scheme.len() > 63
            || !self.activation_scheme.as_bytes()[0].is_ascii_lowercase()
            || !self
                .activation_scheme
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        {
            return Err(Error::InvalidRequest);
        }
        Ok(())
    }
}

pub struct Request<'a> {
    pub id: Uuid,
    pub title: &'a str,
    pub body: &'a str,
    pub sound: bool,
}
impl Request<'_> {
    fn validate(&self) -> Result<(), Error> {
        let xml_char = |c: char| {
            matches!(c, '\t' | '\r' | '\n') || (c >= ' ' && c != '\u{fffe}' && c != '\u{ffff}')
        };
        if self.id.is_nil()
            || self.title.len() > 64 * 1024
            || self.body.len() > 64 * 1024
            || !self.title.chars().chain(self.body.chars()).all(xml_char)
        {
            return Err(Error::InvalidRequest);
        }
        Ok(())
    }
}

/// A valid user click can still outlive its local route or use a legacy request identifier.
/// OpenWorkspace never guesses a task and does not bypass the application's login gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activation {
    Route(Uuid),
    OpenWorkspace,
}
/// Called for a user activation, not delivery or dismissal. Must durably enqueue before returning.
pub type Activated = Arc<dyn Fn(Activation) + Send + Sync>;
pub struct Native {
    #[cfg(target_os = "macos")]
    inner: macos::Native,
    #[cfg(windows)]
    inner: windows::Native,
}
impl Native {
    /// On macOS call during setup before starting delivery. This never requests authorization.
    pub fn new(identity: Identity, activated: Activated) -> Result<Self, Error> {
        identity.validate()?;
        #[cfg(target_os = "macos")]
        {
            Ok(Self {
                inner: macos::Native::new(identity, activated)?,
            })
        }
        #[cfg(windows)]
        {
            let _ = activated;
            Ok(Self {
                inner: windows::Native::new(identity),
            })
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            let _ = (identity, activated);
            Err(Error::Unavailable)
        }
    }
    /// Blocking OS completion wait; use a worker, never the UI/event-loop thread.
    pub fn permission(&self, request: bool) -> Result<Permission, Error> {
        #[cfg(any(target_os = "macos", windows))]
        {
            self.inner.permission(request)
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            let _ = request;
            Err(Error::Unavailable)
        }
    }
    /// Ok means the real platform call accepted the request, not merely an enqueued Rust task.
    pub fn submit(&self, request: Request<'_>) -> Result<(), Error> {
        request.validate()?;
        #[cfg(any(target_os = "macos", windows))]
        {
            self.inner.submit(request)
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        Err(Error::Unavailable)
    }
}

pub fn activation_uri(scheme: &str, id: Uuid) -> String {
    format!("{scheme}://notification/{id}")
}
/// A protocol launch is untrusted input. No query, fragments, userinfo, percent escapes or aliases.
pub fn parse_activation_uri(scheme: &str, input: &str) -> Option<Uuid> {
    canonical_id(input.strip_prefix(&format!("{scheme}://notification/"))?)
}
fn canonical_id(value: &str) -> Option<Uuid> {
    let id = Uuid::parse_str(value).ok()?;
    (!id.is_nil() && id.to_string() == value).then_some(id)
}
#[cfg(any(target_os = "macos", test))]
fn activation_from_identifier(identifier: &str) -> Activation {
    identifier
        .strip_prefix("doing-due-")
        .and_then(canonical_id)
        .map(Activation::Route)
        .unwrap_or(Activation::OpenWorkspace)
}

#[cfg(any(windows, test))]
fn toast_xml(identity: &Identity, request: &Request<'_>) -> Result<String, Error> {
    identity.validate()?;
    request.validate()?;
    let escape = |input: &str| {
        input
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    };
    let uri = activation_uri(&identity.activation_scheme, request.id);
    Ok(format!("<toast activationType=\"protocol\" launch=\"{uri}\"><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual>{}</toast>",
        escape(request.title), escape(request.body), if request.sound { "" } else { "<audio silent=\"true\"/>" }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn activation_urls_are_canonical_and_carry_no_task_or_account_data() {
        let id = Uuid::new_v4();
        let scheme = "doing-dev-notification";
        let uri = activation_uri(scheme, id);
        assert_eq!(parse_activation_uri(scheme, &uri), Some(id));
        for invalid in [
            format!("{uri}?x=1"),
            format!("{uri}#x"),
            format!("{uri}/"),
            uri.to_uppercase(),
            uri.replace(scheme, "https"),
            activation_uri(scheme, Uuid::nil()),
            format!("{scheme}://notification/%31{}", &id.to_string()[1..]),
        ] {
            assert_eq!(parse_activation_uri(scheme, &invalid), None);
        }
    }
    #[test]
    fn unrecognized_native_identifiers_open_only_without_guessing_a_task() {
        let id = Uuid::parse_str("11111111-1111-4111-8111-aaaaaaaaaaaa").unwrap();
        assert_eq!(
            activation_from_identifier(&format!("doing-due-{id}")),
            Activation::Route(id)
        );
        for identifier in [
            "legacy-plugin-42".into(),
            id.to_string(),
            format!("doing-due-{}", Uuid::nil()),
            format!("doing-due-{}", id.to_string().to_uppercase()),
            format!("doing-due-{id}/extra"),
            format!("other-app-{id}"),
        ] {
            assert_eq!(
                activation_from_identifier(&identifier),
                Activation::OpenWorkspace
            );
        }
    }
    #[test]
    fn toast_xml_preserves_text_without_markup_injection_and_uses_protocol_activation() {
        let identity = Identity {
            application_id: "dev.local.chmod777.Doing".into(),
            activation_scheme: "doing-dev-notification".into(),
        };
        let request = Request {
            id: Uuid::new_v4(),
            title: "已截止",
            body: "<tag a=\"x\">中文 & 'emoji 🦀'",
            sound: false,
        };
        let xml = toast_xml(&identity, &request).unwrap();
        assert!(xml.contains("&lt;tag a=&quot;x&quot;&gt;中文 &amp; &apos;emoji 🦀&apos;"));
        assert!(
            xml.contains("activationType=\"protocol\"") && xml.contains("<audio silent=\"true\"/>")
        );
        assert!(!xml.contains(&identity.application_id));
        assert_eq!(
            toast_xml(
                &identity,
                &Request {
                    body: "bad\0text",
                    ..request
                }
            )
            .unwrap_err(),
            Error::InvalidRequest
        );
    }
}

#[cfg(all(test, target_os = "macos"))]
#[test]
fn mismatched_bundle_is_rejected_before_accessing_notification_center_or_permissions() {
    let identity = Identity {
        application_id: format!(
            "dev.local.Doing.notification-test.{}",
            Uuid::new_v4().simple()
        ),
        activation_scheme: "doing-dev-notification".into(),
    };
    assert!(matches!(
        Native::new(identity, Arc::new(|_| panic!("must not activate"))),
        Err(Error::BundleRequired)
    ));
}
