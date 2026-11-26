// SPDX-FileCopyrightText: 2026 BigLinux contributors
// SPDX-License-Identifier: MIT

//! The KDE Connect daemon (`kdeconnectd`) over D-Bus: which phones can take a
//! file, and where a phone's storage is mounted.
//!
//! The daemon is D-Bus activatable, but no method here asks the bus to start it
//! (`no_autostart`): a machine without KDE Connect, or with it stopped, never
//! starts it by being asked.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use futures_util::{Stream, StreamExt};
use zbus::zvariant::OwnedValue;
use zbus::{Connection, proxy};

/// The daemon's bus name.
pub const SERVICE: &str = "org.kde.kdeconnect";
/// The daemon's object, under which each device has its own.
pub const DAEMON_PATH: &str = "/modules/kdeconnect";
/// The plugin that receives files.
pub const SHARE_PLUGIN: &str = "kdeconnect_share";
/// The plugin that mounts a phone's storage.
pub const SFTP_PLUGIN: &str = "kdeconnect_sftp";
/// The plugin that sends this desktop's clipboard.
pub const CLIPBOARD_PLUGIN: &str = "kdeconnect_clipboard";

// Qt exports members in camelCase; zbus would ask for PascalCase without the
// explicit names, and `kdeconnectd` answers that with UnknownMethod.
#[proxy(
    interface = "org.kde.kdeconnect.daemon",
    default_service = "org.kde.kdeconnect",
    default_path = "/modules/kdeconnect",
    gen_blocking = false
)]
/// The daemon object.
pub trait KdeConnectDaemon {
    /// Device ids. Both flags false means "everything the daemon remembers".
    #[zbus(name = "devices", no_autostart)]
    fn devices(&self, only_reachable: bool, only_paired: bool) -> zbus::Result<Vec<String>>;

    /// Emitted when a device appears, goes away or changes reachability.
    #[zbus(signal, name = "deviceListChanged")]
    fn device_list_changed(&self) -> zbus::Result<()>;
}

#[proxy(
    interface = "org.kde.kdeconnect.device",
    default_service = "org.kde.kdeconnect",
    gen_blocking = false
)]
/// One device object.
pub trait KdeConnectDevice {
    /// The name the phone calls itself.
    #[zbus(property, name = "name")]
    fn name(&self) -> zbus::Result<String>;
    /// Whether the phone answers right now.
    #[zbus(property, name = "isReachable")]
    fn is_reachable(&self) -> zbus::Result<bool>;
    /// KDE Connect calls it `isPaired`; older versions said `isTrusted`. Read
    /// as a property, so a version without it fails this read alone, not the
    /// whole list.
    #[zbus(property, name = "isPaired")]
    fn is_paired(&self) -> zbus::Result<bool>;
    /// The plugins running on this device.
    #[zbus(name = "loadedPlugins", no_autostart)]
    fn loaded_plugins(&self) -> zbus::Result<Vec<String>>;
    /// Whether `plugin` runs on this device.
    #[zbus(name = "hasPlugin", no_autostart)]
    fn has_plugin(&self, plugin: &str) -> zbus::Result<bool>;
}

#[proxy(
    interface = "org.kde.kdeconnect.device.battery",
    default_service = "org.kde.kdeconnect",
    gen_blocking = false
)]
/// A device's battery, while its battery plugin runs.
pub trait KdeConnectBattery {
    /// The charge, in percent.
    #[zbus(property, name = "charge")]
    fn charge(&self) -> zbus::Result<i32>;
    /// Whether the phone is on its charger.
    #[zbus(property, name = "isCharging")]
    fn is_charging(&self) -> zbus::Result<bool>;
}

#[proxy(
    interface = "org.kde.kdeconnect.device.sftp",
    default_service = "org.kde.kdeconnect",
    gen_blocking = false
)]
trait KdeConnectSftp {
    #[zbus(name = "mountAndWait", no_autostart)]
    fn mount_and_wait(&self) -> zbus::Result<bool>;
    #[zbus(name = "getMountError", no_autostart)]
    fn mount_error(&self) -> zbus::Result<String>;
    /// Absolute storage path to display name (`a{sv}` on the wire).
    #[zbus(name = "getDirectories", no_autostart)]
    fn directories(&self) -> zbus::Result<HashMap<String, OwnedValue>>;
    #[zbus(name = "mountPoint", no_autostart)]
    fn mount_point(&self) -> zbus::Result<String>;
}

#[proxy(
    interface = "org.kde.kdeconnect.device.share",
    default_service = "org.kde.kdeconnect",
    gen_blocking = false
)]
trait KdeConnectShare {
    #[zbus(name = "shareUrls", no_autostart)]
    fn share_urls(&self, urls: &[String]) -> zbus::Result<()>;
}

#[proxy(
    interface = "org.kde.kdeconnect.device.clipboard",
    default_service = "org.kde.kdeconnect",
    gen_blocking = false
)]
trait KdeConnectClipboard {
    #[zbus(name = "sendClipboard", no_autostart)]
    fn send_clipboard(&self) -> zbus::Result<()>;
}

/// A device id is spliced into an object path: only what a path element holds.
fn is_device_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// A proxy for one device, its properties read fresh: the daemon changes them
/// without telling the bus.
///
/// # Errors
///
/// [`zbus::Error::Failure`] for an `id` that is not a KDE Connect device id, and
/// the bus's error when the proxy cannot be built.
pub async fn device_proxy(
    connection: &Connection,
    id: &str,
) -> zbus::Result<KdeConnectDeviceProxy<'static>> {
    KdeConnectDeviceProxy::builder(connection)
        .path(device_path(id)?)?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
}

fn device_path(id: &str) -> zbus::Result<String> {
    if !is_device_id(id) {
        return Err(zbus::Error::Failure(
            "unusable KDE Connect device id".into(),
        ));
    }
    Ok(format!("{DAEMON_PATH}/devices/{id}"))
}

/// Whether `kdeconnectd` is running now.
pub async fn running(connection: &Connection) -> bool {
    let Ok(bus) = zbus::fdo::DBusProxy::new(connection).await else {
        return false;
    };
    let Ok(name) = zbus::names::BusName::try_from(SERVICE) else {
        return false;
    };
    bus.name_has_owner(name).await.unwrap_or(false)
}

/// Why a phone's plugin cannot be called, in the order a person fixes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// `kdeconnectd` is not running.
    NotRunning,
    /// The daemon does not know the device.
    Unknown,
    /// The device is known but not paired.
    NotPaired,
    /// The device does not answer now.
    NotReachable,
    /// The device runs without `plugin`.
    PluginOff,
}

/// Why a call to `plugin` on the device `id` failed, when the daemon's own
/// state explains it: a device that is out of reach or unpaired has no plugin
/// objects, so the call fails with an `UnknownObject` bus error. `None` when
/// the device looks fine and the failure is something else.
pub async fn unavailable(connection: &Connection, id: &str, plugin: &str) -> Option<Unavailable> {
    if !running(connection).await {
        return Some(Unavailable::NotRunning);
    }
    let known = KdeConnectDaemonProxy::new(connection)
        .await
        .ok()?
        .devices(false, false)
        .await
        .ok()?;
    if !known.iter().any(|known| known == id) {
        return Some(Unavailable::Unknown);
    }
    let device = device_proxy(connection, id).await.ok()?;
    if !device.is_paired().await.ok()? {
        return Some(Unavailable::NotPaired);
    }
    if !device.is_reachable().await.ok()? {
        return Some(Unavailable::NotReachable);
    }
    (!device.has_plugin(plugin).await.ok()?).then_some(Unavailable::PluginOff)
}

/// A paired, reachable phone with a plugin running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Phone {
    /// The daemon's device id.
    pub id: String,
    /// The name the phone calls itself.
    pub name: String,
}

/// The phones running `plugin` now, sorted by name. Empty when the daemon is
/// not running.
///
/// # Errors
/// The daemon is running but did not answer the device list.
pub async fn phones_with(connection: &Connection, plugin: &str) -> zbus::Result<Vec<Phone>> {
    if !running(connection).await {
        return Ok(Vec::new());
    }
    let ids = KdeConnectDaemonProxy::new(connection)
        .await?
        .devices(true, true)
        .await?;
    let mut phones = Vec::new();
    // A device that vanishes between the list and its questions is left out.
    for id in ids.into_iter().take(64) {
        let Ok(device) = device_proxy(connection, &id).await else {
            continue;
        };
        if !device.has_plugin(plugin).await.unwrap_or(false) {
            continue;
        }
        if let Ok(name) = device.name().await {
            phones.push(Phone { id, name });
        }
    }
    phones.sort_by_key(|phone| phone.name.to_lowercase());
    Ok(phones)
}

/// Send `urls` (`file://` URIs) to the phone `id`.
///
/// # Errors
/// The phone is gone, lost the share plugin, or the daemon refused.
pub async fn share_urls(connection: &Connection, id: &str, urls: &[String]) -> zbus::Result<()> {
    KdeConnectShareProxy::builder(connection)
        .path(format!("{}/share", device_path(id)?))?
        .build()
        .await?
        .share_urls(urls)
        .await
}

/// Send this desktop's clipboard to the phone `id`.
///
/// # Errors
/// The phone is gone, lost the clipboard plugin, or the daemon refused.
pub async fn send_clipboard(connection: &Connection, id: &str) -> zbus::Result<()> {
    KdeConnectClipboardProxy::builder(connection)
        .path(format!("{}/clipboard", device_path(id)?))?
        .build()
        .await?
        .send_clipboard()
        .await
}

/// Ticks whenever the list of phones may have changed: a device appeared,
/// went away or changed reachability, or the daemon started or stopped.
///
/// # Errors
/// The bus refused the subscriptions.
pub async fn phones_changed(connection: &Connection) -> zbus::Result<impl Stream<Item = ()>> {
    let devices = KdeConnectDaemonProxy::new(connection)
        .await?
        .receive_device_list_changed()
        .await?
        .map(|_| ());
    let owner = zbus::fdo::DBusProxy::new(connection)
        .await?
        .receive_name_owner_changed_with_args(&[(0, SERVICE)])
        .await?
        .map(|_| ());
    Ok(futures_util::stream::select(devices, owner))
}

/// Call `changed` with the phones running `plugin` now, and again after each
/// change, while it returns `true`. Returns when the bus goes away.
pub async fn follow_phones(
    connection: &Connection,
    plugin: &str,
    mut changed: impl FnMut(Vec<Phone>) -> bool,
) {
    let Ok(mut ticks) = phones_changed(connection).await else {
        return;
    };
    // The list is read after the subscription, so a change in between is not lost.
    loop {
        // A failed read keeps the last list: it says nothing about the phones.
        if let Ok(phones) = phones_with(connection, plugin).await
            && !changed(phones)
        {
            return;
        }
        if ticks.next().await.is_none() {
            return;
        }
    }
}

/// One storage of a mounted phone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Storage {
    /// The directory where the storage is mounted.
    pub path: PathBuf,
    /// The name the phone gives it ("Internal storage").
    pub name: String,
}

/// Why a phone's storage did not come up.
#[derive(Debug, thiserror::Error)]
pub enum MountError {
    /// `kdeconnectd` is not running.
    #[error("KDE Connect is not running")]
    NotRunning,
    /// The daemon tried and gave `0` as the reason; empty when it said nothing.
    #[error("{0}")]
    Refused(String),
    /// The phone has no storage plugin or the bus call failed.
    #[error("{0}")]
    Bus(#[from] zbus::Error),
}

/// Mount the phone `id` over SSH and list its storages. The mount point can
/// appear a moment after this returns: the daemon answers when the `sshfs`
/// process starts.
///
/// # Errors
/// See [`MountError`].
pub async fn mount(connection: &Connection, id: &str) -> Result<Vec<Storage>, MountError> {
    if !running(connection).await {
        return Err(MountError::NotRunning);
    }
    let sftp = KdeConnectSftpProxy::builder(connection)
        .path(format!("{}/sftp", device_path(id)?))?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await?;
    if !sftp.mount_and_wait().await? {
        return Err(MountError::Refused(
            sftp.mount_error().await.unwrap_or_default(),
        ));
    }
    let mount_point = PathBuf::from(sftp.mount_point().await?);
    let mut storages: Vec<Storage> = sftp
        .directories()
        .await?
        .into_iter()
        .filter_map(|(key, name)| {
            let name = String::try_from(name).ok()?;
            // The daemon builds the keys as `mountPoint + path`: absolute, and
            // a key that is not strictly below the mount point is no storage.
            let below = Path::new(&key).strip_prefix(&mount_point).ok()?;
            let inside = !below.as_os_str().is_empty()
                && below
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)));
            inside.then(|| Storage {
                path: mount_point.join(below),
                name,
            })
        })
        .collect();
    storages.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(storages)
}

/// Where `rest` of a phone is, among its `storages`: a first component naming
/// a storage selects it, otherwise the internal storage holds `rest`. `None`
/// for a `rest` that is absolute or climbs with `..`, which would leave the
/// phone.
#[must_use]
pub fn place(storages: &[Storage], rest: &Path) -> Option<PathBuf> {
    if !rest.components().all(|part| {
        matches!(
            part,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    }) {
        return None;
    }
    let mut parts = rest.components();
    let named = parts.next().and_then(|first| {
        storages
            .iter()
            .find(|s| first.as_os_str() == s.name.as_str())
    });
    if let Some(storage) = named {
        return Some(storage.path.join(parts.as_path()));
    }
    // Android's internal storage is `emulated/0`; a card is named by its serial.
    let internal = storages
        .iter()
        .find(|s| s.path.ends_with("emulated/0"))
        .or_else(|| storages.first())?;
    Some(internal.path.join(rest))
}

/// A place on a phone, as another program names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    /// The daemon's device id.
    pub device: String,
    /// The folder under the storage; empty for the storage itself.
    pub rest: PathBuf,
}

impl Location {
    /// `kdeconnect://<id>[/<path>]`, or the kio-fuse mount of that place,
    /// `<runtime_dir>/kio-fuse-*/kdeconnect/<id>[/<path>]`. `None` for anything
    /// else, and for a path that climbs out of the phone.
    #[must_use]
    pub fn parse(text: &str, runtime_dir: &Path) -> Option<Self> {
        if let Some((scheme, rest)) = text.split_once("://") {
            if !scheme.eq_ignore_ascii_case("kdeconnect") {
                return None;
            }
            if rest.contains(['?', '#']) {
                return None;
            }
            let (device, path) = rest.split_once('/').unwrap_or((rest, ""));
            let path = String::from_utf8(crate::uri_path::percent_decoded(path)?).ok()?;
            return Self::new(device, &path);
        }
        let below = Path::new(text).strip_prefix(runtime_dir).ok()?;
        let mut parts = below.components();
        let fuse = parts.next()?.as_os_str().to_str()?;
        if !fuse.starts_with("kio-fuse-") || parts.next()?.as_os_str() != "kdeconnect" {
            return None;
        }
        let device = parts.next()?.as_os_str().to_str()?;
        Self::new(device, parts.as_path().to_str()?)
    }

    fn new(device: &str, rest: &str) -> Option<Self> {
        if !is_device_id(device) {
            return None;
        }
        let mut path = PathBuf::new();
        for part in rest
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
        {
            if part == ".." || part.chars().any(char::is_control) {
                return None;
            }
            path.push(part);
        }
        Some(Self {
            device: device.to_owned(),
            rest: path,
        })
    }
}

#[cfg(test)]
mod tests;
