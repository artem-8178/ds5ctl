//! Bluetooth actions that go through BlueZ instead of the controller.

use std::collections::HashMap;

use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

type ManagedObjects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

/// Drops the Bluetooth link to `mac`. A DualSense switches itself off when
/// the host disconnects it.
pub fn disconnect(mac: &str) -> Result<(), String> {
    let conn = Connection::system().map_err(|e| format!("cannot reach the system D-Bus: {e}"))?;
    let objects: ManagedObjects = conn
        .call_method(
            Some("org.bluez"),
            "/",
            Some("org.freedesktop.DBus.ObjectManager"),
            "GetManagedObjects",
            &(),
        )
        .and_then(|reply| reply.body().deserialize())
        .map_err(|e| format!("cannot list Bluetooth devices: {e}"))?;

    // BlueZ names device objects /org/bluez/hciN/dev_AA_BB_CC_DD_EE_FF.
    let suffix = format!("/dev_{}", mac.to_uppercase().replace(':', "_"));
    let path = objects
        .into_iter()
        .find(|(path, interfaces)| {
            interfaces.contains_key("org.bluez.Device1") && path.as_str().ends_with(&suffix)
        })
        .map(|(path, _)| path)
        .ok_or_else(|| format!("BlueZ does not know {mac}"))?;

    conn.call_method(
        Some("org.bluez"),
        &path,
        Some("org.bluez.Device1"),
        "Disconnect",
        &(),
    )
    .map_err(|e| format!("disconnect failed: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    /// Needs a running BlueZ; checks the D-Bus round trip without touching a device.
    #[test]
    #[ignore]
    fn unknown_device_is_reported() {
        let err = super::disconnect("00:00:00:00:00:00").unwrap_err();
        assert!(err.contains("does not know"), "{err}");
    }
}
