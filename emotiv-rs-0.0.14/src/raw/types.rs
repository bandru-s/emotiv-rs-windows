//! Data types for raw device streaming.

use serde::{Deserialize, Serialize};

/// Emotiv headset model with BLE or USB transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HeadsetModel {
    EpocX,
    EpocPlus,
    EpocFlex,
    EpocStd,
    Insight,
    Insight2,
    MN8,
    Xtrodes,
}

impl std::fmt::Display for HeadsetModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// A model name [`HeadsetModel`]'s `FromStr` does not recognise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownModel(pub String);

impl std::fmt::Display for UnknownModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown headset model: {:?}", self.0)
    }
}

impl std::error::Error for UnknownModel {}

impl std::str::FromStr for HeadsetModel {
    type Err = UnknownModel;

    /// Accepts the display name (`"EPOC X"`) and snake/upper-case forms
    /// (`"epoc_x"`, `"EPOC_X"`).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_name(s).ok_or_else(|| UnknownModel(s.to_owned()))
    }
}

impl HeadsetModel {
    /// Deprecated alias for [`Self::from_name`]; prefer `s.parse::<HeadsetModel>()`.
    #[deprecated(since = "0.0.14", note = "use `from_name` or `str::parse`")]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        Self::from_name(s)
    }

    /// Look up a model by name: the display name (`"EPOC X"`) or the
    /// snake/upper-case forms (`"epoc_x"`, `"EPOC_X"`).
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "EPOC X" | "epoc_x" | "EPOC_X" => Some(HeadsetModel::EpocX),
            "EPOC+" | "epoc_plus" | "EPOC_PLUS" => Some(HeadsetModel::EpocPlus),
            "EPOC Flex" | "epoc_flex" | "EPOC_FLEX" => Some(HeadsetModel::EpocFlex),
            "EPOC" | "epoc_std" | "EPOC_STD" => Some(HeadsetModel::EpocStd),
            "Insight" | "insight" => Some(HeadsetModel::Insight),
            "Insight 2" | "insight_2" | "INSIGHT_2" => Some(HeadsetModel::Insight2),
            "MN8" | "mn8" => Some(HeadsetModel::MN8),
            "Xtrodes" | "xtrodes" => Some(HeadsetModel::Xtrodes),
            _ => None,
        }
    }

    /// Human-readable name.
    pub fn name(&self) -> &str {
        match self {
            HeadsetModel::EpocX => "EPOC X",
            HeadsetModel::EpocPlus => "EPOC+",
            HeadsetModel::EpocFlex => "EPOC Flex",
            HeadsetModel::EpocStd => "EPOC",
            HeadsetModel::Insight => "Insight",
            HeadsetModel::Insight2 => "Insight 2",
            HeadsetModel::MN8 => "MN8",
            HeadsetModel::Xtrodes => "Xtrodes",
        }
    }

    /// Number of EEG channels.
    pub fn channel_count(&self) -> usize {
        match self {
            HeadsetModel::EpocX | HeadsetModel::EpocPlus | HeadsetModel::EpocStd => 14,
            HeadsetModel::EpocFlex => 32,
            HeadsetModel::Insight | HeadsetModel::Insight2 => 5,
            HeadsetModel::MN8 => 2,
            HeadsetModel::Xtrodes => 8,
        }
    }

    /// EEG channel names.
    pub fn channels(&self) -> Vec<&'static str> {
        match self {
            HeadsetModel::EpocX | HeadsetModel::EpocPlus | HeadsetModel::EpocStd => vec![
                "AF3", "F7", "F3", "FC5", "T7", "P7", "O1", "O2", "P8", "T8", "FC6", "F4", "F8",
                "AF4",
            ],
            HeadsetModel::EpocFlex => vec![
                "Fp1", "Fpz", "Fp2", "AF7", "AF3", "AFz", "AF4", "AF8", "F7", "F5", "F3", "F1",
                "Fz", "F2", "F4", "F6", "F8", "FT7", "FC5", "FC3", "FC1", "FCz", "FC2", "FC4",
                "FC6", "FT8", "T7", "C5", "C3", "C1", "Cz", "C2", "C4", "C6", "T8",
            ],
            HeadsetModel::Insight | HeadsetModel::Insight2 => {
                vec!["AF3", "AF4", "T7", "T8", "Pz"]
            }
            HeadsetModel::MN8 => vec!["TP9", "TP10"],
            _ => vec![],
        }
    }

    /// Sampling rate (Hz).
    pub fn sampling_rate(&self) -> u32 {
        match self {
            HeadsetModel::EpocX | HeadsetModel::EpocPlus => 256,
            HeadsetModel::EpocStd => 128,
            HeadsetModel::EpocFlex => 256,
            HeadsetModel::Insight => 128,
            HeadsetModel::Insight2 => 256,
            HeadsetModel::MN8 => 256,
            HeadsetModel::Xtrodes => 250,
        }
    }

    /// Physical voltage range in microvolts.
    pub fn eeg_physical_range(&self) -> (f64, f64) {
        match self {
            HeadsetModel::EpocX | HeadsetModel::EpocPlus | HeadsetModel::EpocStd => {
                (-8399.0, 8399.0)
            }
            HeadsetModel::EpocFlex => (-8399.0, 8399.0),
            HeadsetModel::Insight | HeadsetModel::Insight2 => (-4199.5, 4199.5),
            HeadsetModel::MN8 => (-4199.5, 4199.5),
            HeadsetModel::Xtrodes => (-32767.0, 32767.0),
        }
    }
}

/// Raw decrypted EEG/motion packet data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecryptedData {
    /// Counter (packet sequence number).
    pub counter: u32,
    /// EEG samples in microvolts per channel.
    pub eeg_uv: Vec<f64>,
    /// Raw ADC values (before conversion).
    pub eeg_adc: Vec<i32>,
    /// Contact quality per channel (0-4 scale, 4=best).
    pub contact_quality: Vec<u8>,
    /// Overall signal quality (0-4).
    pub signal_quality: u8,
    /// Motion sample received since the previous EEG packet, if any.
    ///
    /// **Tentative layout, not yet verified against hardware:**
    /// `[counter, accel_x, accel_y, accel_z, gyro_x, gyro_y, gyro_z, mag_x,
    /// mag_y, mag_z]`, as raw sensor counts (a big-endian `u16` counter
    /// followed by nine big-endian `i16`). [`Self::motion_raw`] carries the
    /// decrypted bytes this was read from, so the layout can be checked.
    pub motion: Option<Vec<f64>>,
    /// The decrypted motion packet behind [`Self::motion`], unmodified.
    #[serde(default)]
    pub motion_raw: Option<Vec<u8>>,
    /// Battery percentage (0-100).
    pub battery_percent: u8,
    /// When the sample was taken, in Unix seconds — reconstructed from the
    /// packet counter and sampling rate, so Bluetooth delivery jitter does not
    /// show up in it. Re-anchored to the receive time after a gap the counter
    /// cannot account for.
    pub timestamp: f64,
    /// When the packet arrived, in Unix seconds.
    pub receive_time: f64,
}

impl DecryptedData {
    /// Create new decrypted data.
    pub fn new(
        counter: u32,
        eeg_uv: Vec<f64>,
        eeg_adc: Vec<i32>,
        contact_quality: Vec<u8>,
        signal_quality: u8,
        battery_percent: u8,
    ) -> Self {
        // Both start as "now"; the decode pipeline replaces `timestamp` with
        // the counter-based reconstruction.
        let now = crate::raw::platform::now_secs();
        Self {
            counter,
            eeg_uv,
            eeg_adc,
            contact_quality,
            signal_quality,
            motion: None,
            motion_raw: None,
            battery_percent,
            timestamp: now,
            receive_time: now,
        }
    }
}

/// Battery info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatteryInfo {
    pub percent: u8,
    pub voltage_mv: u16,
    pub is_low: bool,
}

/// Device connection state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    Disconnected,
    Connecting,
    Connected,
    Streaming,
    /// The link dropped; waiting to reconnect under the device's
    /// [`ReconnectPolicy`](crate::raw::device::ReconnectPolicy).
    Reconnecting,
    Disconnecting,
}

impl std::fmt::Display for DeviceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disconnected => write!(f, "Disconnected"),
            Self::Connecting => write!(f, "Connecting"),
            Self::Connected => write!(f, "Connected"),
            Self::Streaming => write!(f, "Streaming"),
            Self::Reconnecting => write!(f, "Reconnecting"),
            Self::Disconnecting => write!(f, "Disconnecting"),
        }
    }
}
