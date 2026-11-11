//! Finding and identifying Emotiv headsets: the GATT layout, scan and request
//! filters, resolving a [`DeviceInfo`] to a granted device, and inferring a
//! headset's model and serial from what it advertises.
//!
//! BLE goes through [`webbluetooth`], the Web Bluetooth API in Rust. The
//! adapter session is `Bluetooth::shared()`: one per process, cloned by every
//! caller. It has to be shared, because a device is only usable through the
//! session that found it, and discovery and connection run as separate calls.
//!
//! A Web Bluetooth grant covers exactly the services it names, so every grant
//! here lists the Emotiv vendor services.

use crate::raw::device::DeviceInfo;
use crate::raw::platform;
use crate::raw::types::HeadsetModel;
use anyhow::{anyhow, Context, Result};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;
use uuid::Uuid;
use webbluetooth::chooser::{Candidate, FirstMatch};
use webbluetooth::stream::StreamExt;
use webbluetooth::uuid::BluetoothUuid;
use webbluetooth::{
    Bluetooth, BluetoothDevice, DataPrefix, DeviceFilter, Grant, LeScanOptions,
    RequestDeviceOptions,
};

// ── GATT layout ───────────────────────────────────────────────────────────────

/// Vendor service carrying the encrypted EEG/motion notifications.
pub(crate) const EMOTIV_DATA_SERVICE: Uuid =
    Uuid::from_u128(0x00001100_d102_11e1_9b23_00025b00a5a5);

/// Vendor service whose writable characteristics accept the start-stream command.
pub(crate) const EMOTIV_CONTROL_SERVICE: Uuid =
    Uuid::from_u128(0x00001101_d102_11e1_9b23_00025b00a5a5);

pub(crate) const EMOTIV_SERVICE_1102: Uuid =
    Uuid::from_u128(0x00001102_d102_11e1_9b23_00025b00a5a5);

pub(crate) const EMOTIV_SERVICE_1103: Uuid =
    Uuid::from_u128(0x00001103_d102_11e1_9b23_00025b00a5a5);

/// EPOC X vendor service. Its EEG and auxiliary notification characteristics
/// are 81072f42 and 81072f41, respectively.
pub(crate) const EMOTIV_EPOC_X_SERVICE: Uuid =
    Uuid::from_u128(0x81072f40_9f3d_11e3_a9dc_0002a5d5c51b);
pub(crate) const EMOTIV_EPOC_X_AUX_CHARACTERISTIC: Uuid =
    Uuid::from_u128(0x81072f41_9f3d_11e3_a9dc_0002a5d5c51b);
pub(crate) const EMOTIV_EPOC_X_EEG_CHARACTERISTIC: Uuid =
    Uuid::from_u128(0x81072f42_9f3d_11e3_a9dc_0002a5d5c51b);

/// Every vendor service a headset may expose. Named in each grant, because
/// `get_characteristics` only reaches the services a grant lists.
pub(crate) const EMOTIV_SERVICES: [Uuid; 5] = [
    EMOTIV_DATA_SERVICE,
    EMOTIV_CONTROL_SERVICE,
    EMOTIV_SERVICE_1102,
    EMOTIV_SERVICE_1103,
    EMOTIV_EPOC_X_SERVICE,
];

/// Battery Level — excluded from the fallback notify set, since it is never EEG.
pub(crate) const BATTERY_LEVEL_CHARACTERISTIC: Uuid =
    Uuid::from_u128(0x00002a19_0000_1000_8000_00805f9b34fb);

/// Emotiv's Bluetooth SIG company identifier, as seen in benchmark mock adverts.
pub(crate) const EMOTIV_COMPANY_ID: u16 = 0x0422;

/// Advertised-name prefixes the scan reports. Web Bluetooth name filters are
/// case-sensitive, so the common casings are listed separately;
/// [`is_emotiv_candidate`] then re-checks case-insensitively.
pub(crate) const NAME_PREFIXES: [&str; 13] = [
    "EMOTIV", "Emotiv", "emotiv", "EPOC", "Epoc", "INSIGHT", "Insight", "MN8", "XTRODES",
    "Xtrodes", "xtrodes", "FLEX", "Flex",
];

/// What a scan reports: anything named like an Emotiv headset, advertising an
/// Emotiv service, or carrying Emotiv manufacturer data.
///
/// Listing the company identifier in a filter is also what makes the scan
/// hand that manufacturer data over at all.
pub(crate) fn emotiv_filters() -> Result<Vec<DeviceFilter>> {
    let mut filters: Vec<DeviceFilter> = NAME_PREFIXES
        .iter()
        .map(|p| DeviceFilter::new().name_prefix(*p))
        .collect();
    for service in EMOTIV_SERVICES {
        filters.push(DeviceFilter::new().service(service)?);
    }
    filters.push(DeviceFilter::new().manufacturer_data(EMOTIV_COMPANY_ID, DataPrefix::new(vec![])));
    Ok(filters)
}

pub(crate) fn scan_options() -> Result<LeScanOptions> {
    Ok(LeScanOptions::new().filters(emotiv_filters()?))
}

/// What a device request asks for: the same matches as a scan, plus access to
/// the Emotiv vendor services — a Web Bluetooth grant covers exactly the
/// services a request lists.
pub(crate) fn request_options() -> Result<RequestDeviceOptions> {
    let mut options = RequestDeviceOptions::new();
    for filter in emotiv_filters()? {
        options = options.filter(filter);
    }
    Ok(options.optional_services(EMOTIV_SERVICES)?)
}

/// What adopting a device grants: the Emotiv vendor services.
pub(crate) fn emotiv_grant() -> Result<Grant> {
    Ok(Grant::new().services(EMOTIV_SERVICES)?)
}

/// How long a discovery scan listens, from `EMOTIV_RAW_SCAN_SECS` (min 3 s).
pub(crate) fn scan_window() -> Duration {
    let secs = std::env::var("EMOTIV_RAW_SCAN_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(8)
        .max(3);
    Duration::from_secs(secs)
}

/// Scan for `window`, keeping one candidate per device id.
///
/// A headset advertises repeatedly; later sightings are merged into the first
/// so a name that arrives only in a scan response is not lost.
pub(crate) async fn scan_candidates(
    bluetooth: &Bluetooth,
    window: Duration,
) -> Result<BTreeMap<String, Candidate>> {
    let mut scan = bluetooth.request_le_scan(scan_options()?).await?;
    let mut seen: BTreeMap<String, Candidate> = BTreeMap::new();
    let _ = platform::timeout(window, async {
        while let Some(candidate) = scan.next().await {
            match seen.get_mut(&candidate.id) {
                Some(existing) => existing.merge(candidate),
                None => {
                    log::debug!("scan: saw {}  id={}", candidate.label(), candidate.id);
                    seen.insert(candidate.id.clone(), candidate);
                }
            }
        }
    })
    .await;
    scan.stop(); // stop the radio before connecting to anything
    Ok(seen)
}

/// Waits for the adapter's first state report, failing if it is unusable.
pub(crate) async fn shared_bluetooth() -> Result<Bluetooth> {
    let bluetooth = Bluetooth::shared();
    bluetooth
        .availability()
        .await
        .map_err(|e| anyhow!("Bluetooth is not usable: {e:?}"))?;
    Ok(bluetooth)
}

/// Resolve a [`DeviceInfo`] to a granted [`BluetoothDevice`].
///
/// A device this session already holds is used as is. Otherwise adopting by id
/// is tried: it needs no scan, and works for anything the platform already
/// knows — including whatever `discover()` just reported. Failing that, scan
/// with increasing windows and take the best match.
pub(crate) async fn resolve_device(
    bluetooth: &Bluetooth,
    info: &DeviceInfo,
) -> Result<BluetoothDevice> {
    // Already granted — by `request_device`, or an earlier connection. In a
    // browser this is the only route: a page cannot adopt by id.
    if let Some(device) = bluetooth
        .get_devices()
        .into_iter()
        .find(|d| d.id() == info.ble_id)
    {
        return Ok(device);
    }

    let grant = emotiv_grant()?;

    if !info.ble_id.is_empty() {
        match bluetooth.adopt_device(&info.ble_id, grant.clone()).await {
            Ok(device) => return Ok(device),
            Err(e) => log::debug!("adopt_device({}) failed, scanning: {e}", info.ble_id),
        }
    }

    for wait_secs in [3_u64, 5, 8] {
        let candidates = scan_candidates(bluetooth, Duration::from_secs(wait_secs)).await?;
        if let Some(candidate) = best_candidate(candidates.into_values(), info) {
            return bluetooth
                .adopt_candidate(&candidate, grant)
                .await
                .with_context(|| {
                    format!("could not adopt {} ({})", candidate.label(), candidate.id)
                });
        }
    }

    Err(anyhow!(
        "BLE device not found after scan retries: {} ({})",
        info.name,
        info.address
    ))
}

/// Ask for one Emotiv headset through the platform's device chooser.
///
/// In a browser this opens its Bluetooth picker, so it must be called from a
/// user gesture (a click handler) — and it is the way to find a device there,
/// since browsers do not generally offer the scanning [`RawDevice::discover`](crate::raw::RawDevice::discover)
/// relies on. Natively it takes the first matching headset to advertise within
/// `EMOTIV_RAW_SCAN_SECS` (default 8 s).
///
/// The returned device is already granted, so [`RawDevice::connect`](crate::raw::RawDevice::connect) uses it
/// directly without scanning again.
pub async fn request_device() -> Result<DeviceInfo> {
    let bluetooth = shared_bluetooth().await?;
    let chooser = FirstMatch::with_timeout(scan_window());
    let device = bluetooth
        .request_device_with(request_options()?, &chooser)
        .await
        .map_err(|e| anyhow!("No Emotiv headset chosen: {e}"))?;

    let mut info = DeviceInfo::ble(device.id(), device.name().unwrap_or_default());
    info.is_connected = device.gatt().connected();
    log::info!("Chose {}  id={}", info.name, info.ble_id);
    Ok(info)
}

/// Discover BLE devices.
pub(crate) async fn discover_ble_devices() -> Result<Vec<DeviceInfo>> {
    let bluetooth = shared_bluetooth().await?;
    let window = scan_window();
    log::info!(
        "Scanning for Emotiv BLE devices for {} s …",
        window.as_secs()
    );
    let candidates = scan_candidates(&bluetooth, window).await?;

    // Devices this session already holds, keyed by id, so `is_connected` can
    // be reported for a headset that is linked and still advertising.
    let held: HashMap<String, BluetoothDevice> = bluetooth
        .get_devices()
        .into_iter()
        .map(|d| (d.id().to_owned(), d))
        .collect();

    let mut matched_devices = Vec::new();
    for candidate in candidates.into_values() {
        let raw_name = candidate.label().to_owned();
        let name = if raw_name.trim().is_empty() || raw_name == candidate.id {
            "(unknown)".to_string()
        } else {
            raw_name
        };
        let lname = name.to_ascii_lowercase();
        if lname.contains("mock") || lname.contains("virtual") || lname.contains("sim") {
            continue;
        }
        let mut info = DeviceInfo::ble(candidate.id.clone(), name);
        info.is_connected = held.get(&info.ble_id).is_some_and(|d| d.gatt().connected());
        let emotiv_candidate = is_emotiv_candidate(
            &info.name,
            &candidate.advertisement.service_uuids,
            &candidate.advertisement.manufacturer_data,
            info.is_connected,
        );
        if !emotiv_candidate {
            continue;
        }
        log::info!("Found {}  id={}", info.name, info.ble_id);
        matched_devices.push(info);
    }

    if matched_devices.is_empty() {
        log::warn!("No explicit Emotiv BLE advertisements found; returning 0 devices.");
    }
    Ok(matched_devices)
}

/// Discover USB devices (not yet implemented).
pub(crate) async fn discover_usb_devices() -> Result<Vec<DeviceInfo>> {
    Ok(Vec::new())
}

/// The platform id is the Bluetooth address everywhere but Apple, where it is
/// a system-assigned UUID. Only the former is a MAC.
pub(crate) fn mac_from_id(id: &str) -> Option<String> {
    let parts: Vec<&str> = id.split(':').collect();
    let is_mac = parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()));
    (is_mac && !is_zero_mac(id)).then(|| id.to_ascii_uppercase())
}

/// Pick the scan candidate that best matches `info`.
///
/// An exact match on id, address, name or inferred serial wins outright;
/// otherwise the highest-scoring fuzzy match is returned.
pub(crate) fn best_candidate(
    candidates: impl IntoIterator<Item = Candidate>,
    info: &DeviceInfo,
) -> Option<Candidate> {
    let target_id = normalize_id(&info.address);
    let target_ble_id = normalize_id(&info.ble_id);
    let target_mac = info
        .ble_mac
        .as_deref()
        .map(normalize_id)
        .unwrap_or_default();
    let target_name = normalize_id(&info.name);
    let target_serial = normalize_serial_12(&info.serial);

    let mut scored: Vec<(i32, Candidate)> = Vec::new();

    for c in candidates {
        let pid_norm = normalize_id(&c.id);
        let mac_norm = mac_from_id(&c.id)
            .map(|m| normalize_id(&m))
            .unwrap_or_default();
        let name = c.name.clone().unwrap_or_default();
        let name_norm = normalize_id(&name);
        let inferred_serial = normalize_serial_12(&infer_serial(&name, &c.id));

        let exact = (!target_id.is_empty() && pid_norm == target_id)
            || (!target_ble_id.is_empty() && pid_norm == target_ble_id)
            || (!target_mac.is_empty() && !mac_norm.is_empty() && target_mac == mac_norm)
            || (!target_name.is_empty() && !name_norm.is_empty() && target_name == name_norm)
            || (!target_serial.is_empty() && inferred_serial == target_serial);

        if exact {
            return Some(c);
        }

        let mut score = 0i32;

        if !target_name.is_empty() && !name_norm.is_empty() {
            if target_name == name_norm {
                score += 80;
            } else if name_norm.contains(&target_name) || target_name.contains(&name_norm) {
                score += 45;
            }
        }

        if !target_serial.is_empty()
            && !inferred_serial.is_empty()
            && inferred_serial == target_serial
        {
            score += 85;
        }

        if !target_id.is_empty()
            && !pid_norm.is_empty()
            && (pid_norm.contains(&target_id) || target_id.contains(&pid_norm))
        {
            score += 25;
        }

        if !target_ble_id.is_empty()
            && !pid_norm.is_empty()
            && (pid_norm.contains(&target_ble_id) || target_ble_id.contains(&pid_norm))
        {
            score += 20;
        }

        if !target_mac.is_empty() && !mac_norm.is_empty() {
            if target_mac == mac_norm {
                score += 90;
            } else if mac_norm.contains(&target_mac) || target_mac.contains(&mac_norm) {
                score += 30;
            }
        }

        if advertises_emotiv_service(&c.advertisement.service_uuids, &EMOTIV_SERVICES) {
            score += 35;
        }

        if score > 0 {
            scored.push((score, c));
        }
    }

    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    scored.into_iter().next().map(|(_, c)| c)
}

pub(crate) fn advertises_emotiv_service(advertised: &[BluetoothUuid], services: &[Uuid]) -> bool {
    advertised
        .iter()
        .any(|u| services.iter().any(|s| *u == BluetoothUuid::from(*s)))
}

pub(crate) fn is_emotiv_candidate(
    name: &str,
    services: &[BluetoothUuid],
    manufacturer_data: &HashMap<u16, Vec<u8>>,
    is_connected: bool,
) -> bool {
    let lname = name.to_ascii_lowercase();
    let name_match = lname.starts_with("emotiv-")
        || lname.contains("emotiv")
        || lname.starts_with("epoc")
        || lname.starts_with("insight")
        || lname.starts_with("mn8")
        || lname.starts_with("xtrodes")
        || lname.starts_with("flex");

    if name_match {
        return true;
    }

    // 0x0422 from benchmark mock advertisement (EMOTIV vendor id)
    if manufacturer_data.contains_key(&EMOTIV_COMPANY_ID) {
        return true;
    }

    if advertises_emotiv_service(services, &EMOTIV_SERVICES) {
        return true;
    }

    // Accept already-connected peripherals as candidates to avoid missing headsets
    // whose advertising payload is sparse/stripped by platform BLE stacks.
    is_connected
}

pub(crate) fn infer_model_from_name(name: &str) -> Option<HeadsetModel> {
    let n = name.to_ascii_lowercase();
    if n.contains("insight 2") || n.contains("insight2") {
        Some(HeadsetModel::Insight2)
    } else if n.contains("insight") {
        Some(HeadsetModel::Insight)
    } else if n.contains("epoc flex") || n.contains("flex") {
        Some(HeadsetModel::EpocFlex)
    } else if n.contains("epoc+") || n.contains("epoc plus") {
        Some(HeadsetModel::EpocPlus)
    } else if n.contains("epoc x") || n.contains("epocx") {
        Some(HeadsetModel::EpocX)
    } else if n.contains("epoc") {
        Some(HeadsetModel::EpocStd)
    } else if n.contains("mn8") {
        Some(HeadsetModel::MN8)
    } else if n.contains("xtrodes") {
        Some(HeadsetModel::Xtrodes)
    } else {
        None
    }
}

pub(crate) fn infer_serial(name: &str, address: &str) -> String {
    let suffix_from_name: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .rev()
        .take(12)
        .collect::<String>()
        .chars()
        .rev()
        .collect();

    if suffix_from_name.len() >= 12 {
        return suffix_from_name;
    }

    normalize_serial_12(address)
}

pub(crate) fn normalize_serial_12(input: &str) -> String {
    let mut chars: String = input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_uppercase();

    if chars.len() >= 12 {
        chars.truncate(12);
        return chars;
    }

    while chars.len() < 12 {
        chars.push('0');
    }
    chars
}

/// The last 12 alphanumerics, upper-cased — or nothing if there are fewer.
pub(crate) fn serial_suffix_12(input: &str) -> String {
    let chars: Vec<char> = input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if chars.len() < 12 {
        return String::new();
    }
    chars[chars.len() - 12..].iter().collect()
}

pub(crate) fn normalize_id(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

pub(crate) fn is_zero_mac(mac: &str) -> bool {
    let only_hex: String = mac
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect::<String>()
        .to_lowercase();
    !only_hex.is_empty() && only_hex.chars().all(|c| c == '0')
}

pub(crate) fn serial_candidates(info: &DeviceInfo) -> Vec<String> {
    let mut out = Vec::new();

    let mut push_unique = |s: String| {
        if !s.is_empty() && !out.iter().any(|x| x == &s) {
            out.push(s);
        }
    };

    // An explicit serial goes first: it is the one thing that beats inference.
    if let Ok(serial) = std::env::var("EMOTIV_RAW_SERIAL") {
        push_unique(normalize_serial_12(&serial));
        push_unique(serial_suffix_12(&serial));
    }
    push_unique(normalize_serial_12(&info.serial));
    // A full serial typed in by hand can be longer than the 12 characters the
    // key schedules read; names carry it as a suffix, so try both ends.
    push_unique(serial_suffix_12(&info.serial));
    push_unique(normalize_serial_12(&info.address));
    push_unique(normalize_serial_12(&info.ble_id));
    if let Some(mac) = &info.ble_mac {
        push_unique(normalize_serial_12(mac));
    }
    push_unique(normalize_serial_12(&info.name));

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_inference() {
        assert_eq!(
            infer_model_from_name("EMOTIV EPOC X"),
            Some(HeadsetModel::EpocX)
        );
        assert_eq!(
            infer_model_from_name("EPOCX (E502133C)"),
            Some(HeadsetModel::EpocX)
        );
        assert_eq!(
            infer_model_from_name("Insight 2"),
            Some(HeadsetModel::Insight2)
        );
    }

    #[test]
    fn test_mac_from_id() {
        assert_eq!(
            mac_from_id("aa:bb:cc:dd:ee:ff").as_deref(),
            Some("AA:BB:CC:DD:EE:FF")
        );
        assert_eq!(mac_from_id("00:00:00:00:00:00"), None);
        // Apple's per-host UUID is not an address.
        assert_eq!(mac_from_id("6F1C2B9E-3D4A-4C5B-9E8F-0A1B2C3D4E5F"), None);
    }

    #[test]
    fn test_emotiv_candidate_by_service() {
        let services = vec![BluetoothUuid::from(EMOTIV_DATA_SERVICE)];
        assert!(is_emotiv_candidate(
            "(unknown)",
            &services,
            &HashMap::new(),
            false
        ));
        let epoc_x_services = vec![BluetoothUuid::from(EMOTIV_EPOC_X_SERVICE)];
        assert!(is_emotiv_candidate(
            "(unknown)",
            &epoc_x_services,
            &HashMap::new(),
            false
        ));
        assert!(!is_emotiv_candidate(
            "(unknown)",
            &[],
            &HashMap::new(),
            false
        ));
    }

    #[test]
    fn test_serial_candidates_try_both_ends() {
        let info = DeviceInfo::default().with_serial("ab12-cd34-ef56-7890");
        let candidates = serial_candidates(&info);
        assert!(candidates.contains(&"AB12CD34EF56".to_string()));
        assert!(candidates.contains(&"CD34EF567890".to_string()));
    }

    #[test]
    fn test_scan_options_are_valid() {
        scan_options().unwrap().validate().unwrap();
        let request = request_options().unwrap();
        request.validate().unwrap();
        assert!(request
            .allowed_services()
            .contains(&BluetoothUuid::from(EMOTIV_EPOC_X_SERVICE)));

        let grant = emotiv_grant().unwrap();
        grant.validate().unwrap();
        assert!(grant.permits(&BluetoothUuid::from(EMOTIV_EPOC_X_SERVICE)));
    }
}
