//! From raw BLE notifications to [`DecryptedData`]: reassembly, key lock-on,
//! motion routing and timestamps — everything between the radio and the
//! channel, with no Bluetooth in it.
//!
//! Kept apart from [`crate::raw::device`] so it can be tested with synthetic
//! packets, and so recorded notifications can be replayed through exactly the
//! code a live connection uses.
//!
//! # Locking onto a key
//!
//! The AES key is derived from the headset's serial, which is only ever
//! *inferred*, so the pipeline holds a candidate decryptor per (serial guess,
//! model) pair and has to find the right one. Decoding alone cannot tell:
//! for 16-bit layouts any 32 bytes "decode" into a full set of channels. The
//! packet counter can. Under the right key it advances by a constant step;
//! under a wrong key it is noise. A candidate is locked once its counter shows
//! the same non-zero step [`LOCK_STREAK`] + 1 times in a row — a false lock
//! needs two consecutive 1-in-65,536 coincidences.
//!
//! If nothing confirms within [`UNCONFIRMED_LOCK_AFTER`] packets, the best
//! decodable candidate is locked anyway (flagged `unconfirmed`), which is
//! where the earlier first-decodable-wins behaviour would have landed.
//!
//! A confirmed lock keeps watching the step, and is dropped after
//! [`RELOCK_AFTER_MISMATCHES`] consecutive breaks — what a changed serial or a
//! wrong key looks like once streaming.

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use uuid::Uuid;

use crate::raw::decryption::Decryptor;
use crate::raw::platform;
use crate::raw::types::{DecryptedData, HeadsetModel};

/// Consecutive repeats of the same counter step needed to lock (after the
/// first step is seen): 2 means four packets.
pub const LOCK_STREAK: u8 = 2;
/// Packets to search before locking without counter confirmation.
pub const UNCONFIRMED_LOCK_AFTER: u64 = 64;
/// Consecutive packets that break a confirmed lock's counter step (or fail to
/// decode) after which the lock is dropped and the search starts again. A
/// dropped or late packet breaks the step once or twice; a wrong key breaks it
/// every time.
pub const RELOCK_AFTER_MISMATCHES: u32 = 64;

/// Shortest decrypted motion packet the tentative layout can be read from:
/// a `u16` counter and nine `i16`.
const MOTION_MIN_LEN: usize = 2 + 9 * 2;

/// How far a reconstructed timestamp may run ahead of the receive time before
/// the clock re-anchors. A sample cannot arrive before it was taken, so this
/// only absorbs host scheduling noise.
const CLOCK_AHEAD_LIMIT: f64 = 0.25;
/// How far it may fall behind: delivery latency and short stalls are normal,
/// a couple of seconds means the counter and the clock disagree.
const CLOCK_BEHIND_LIMIT: f64 = 2.0;

/// Running totals, as the pipeline sees them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PipelineStats {
    /// Notifications pushed in.
    pub received: u64,
    /// EEG packets decoded and emitted.
    pub decoded: u64,
    /// Packets that produced no output: searched without locking, or failed
    /// to decode under the locked key.
    pub failures: u64,
    /// Motion packets decrypted.
    pub motion_packets: u64,
}

struct KeyCandidate {
    serial: String,
    model: HeadsetModel,
    decryptor: Decryptor,
}

/// Counter history for one candidate on one characteristic, while searching.
#[derive(Debug, Clone, Copy, Default)]
struct Track {
    last: Option<u16>,
    step: Option<u16>,
    streak: u8,
}

struct Lock {
    index: usize,
    uuid: Uuid,
    step: u16,
    confirmed: bool,
    partial: bool,
    last_counter: Option<u16>,
}

/// The decode pipeline for one headset. See the [module docs](self).
pub struct DecodePipeline {
    candidates: Vec<KeyCandidate>,
    tracks: HashMap<Uuid, Vec<Track>>,
    lock: Option<Lock>,
    searched: u64,
    mismatches: u32,
    buffers: HashMap<Uuid, Vec<u8>>,
    packet_len: usize,
    min_payload_len: usize,
    required_channels: usize,
    fallback_channels: usize,
    clock: SampleClock,
    pending_motion: Option<(Vec<f64>, Vec<u8>)>,
    stats: PipelineStats,
}

impl DecodePipeline {
    /// A pipeline for a headset believed to be `model`, trying every serial in
    /// `serials` (in order) against `model` first and then the other models.
    pub fn new(model: HeadsetModel, serials: &[String]) -> Result<Self> {
        let candidates = build_candidates(model, serials);
        if candidates.is_empty() {
            return Err(anyhow!("No usable serial candidates for decryption"));
        }
        let packet_len = packet_len_for_model(model);
        Ok(Self {
            candidates,
            tracks: HashMap::new(),
            lock: None,
            searched: 0,
            mismatches: 0,
            buffers: HashMap::new(),
            packet_len,
            min_payload_len: packet_len.min(20),
            required_channels: model.channel_count(),
            fallback_channels: fallback_channels_for_model(model),
            clock: SampleClock::new(f64::from(model.sampling_rate())),
            pending_motion: None,
            stats: PipelineStats::default(),
        })
    }

    /// Feed one notification from characteristic `uuid`; returns the EEG
    /// packets it completed, in order.
    pub fn push(&mut self, uuid: Uuid, payload: &[u8]) -> Vec<DecryptedData> {
        self.push_at(uuid, payload, platform::now_secs())
    }

    /// [`Self::push`], with the receive time given rather than read from the
    /// clock — for replaying recordings and for tests.
    pub fn push_at(&mut self, uuid: Uuid, payload: &[u8], now: f64) -> Vec<DecryptedData> {
        self.stats.received += 1;
        // Too short to start a packet — but a short payload after a partial
        // one is its tail: at the default MTU a 32-byte packet arrives as
        // 20 + 12.
        let continues = self.buffers.get(&uuid).is_some_and(|b| !b.is_empty());
        if payload.len() < self.min_payload_len && !continues {
            return Vec::new();
        }

        let mut out = Vec::new();
        for packet in self.reassemble(uuid, payload) {
            let locked_uuid = self.lock.as_ref().map(|l| l.uuid);
            match locked_uuid {
                // Once the EEG characteristic is known, the data service's
                // other notifications are motion.
                Some(eeg) if eeg != uuid => self.decode_motion(&packet),
                Some(_) => out.extend(self.decode_locked(&packet, now)),
                None => out.extend(self.search(uuid, &packet, now)),
            }
        }
        out
    }

    /// The locked candidate as `serial/model`, with `:partial` and
    /// `:unconfirmed` flags — or `None` while still searching.
    pub fn locked_key(&self) -> Option<String> {
        self.lock.as_ref().map(|lock| {
            let candidate = &self.candidates[lock.index];
            format!(
                "{}/{}{}{}",
                candidate.serial,
                candidate.model.name(),
                if lock.partial { ":partial" } else { "" },
                if lock.confirmed { "" } else { ":unconfirmed" },
            )
        })
    }

    /// The characteristic carrying EEG, once locked.
    pub fn eeg_uuid(&self) -> Option<Uuid> {
        self.lock.as_ref().map(|l| l.uuid)
    }

    pub fn stats(&self) -> &PipelineStats {
        &self.stats
    }

    /// Prepare for a new connection to the same headset: drop partial packets
    /// and re-anchor the clock, but keep the key — the serial has not changed.
    pub fn resume(&mut self) {
        self.buffers.clear();
        self.tracks.clear();
        self.clock.reset();
        self.pending_motion = None;
        self.mismatches = 0;
        if let Some(lock) = &mut self.lock {
            lock.last_counter = None;
        }
    }

    /// Append to the characteristic's buffer and cut out every complete packet.
    fn reassemble(&mut self, uuid: Uuid, payload: &[u8]) -> Vec<Vec<u8>> {
        let len = self.packet_len;
        let buffer = self.buffers.entry(uuid).or_default();
        buffer.extend_from_slice(payload);

        let mut packets = Vec::new();
        while buffer.len() >= len {
            packets.push(buffer.drain(..len).collect());
        }
        // Unreachable while every notification is a whole number of packets;
        // a guard against a stream that never realigns.
        if buffer.len() > len * 4 {
            let excess = buffer.len() - len * 2;
            buffer.drain(..excess);
        }
        packets
    }

    /// Try every candidate on an unlocked packet; lock and emit if one
    /// confirms.
    fn search(&mut self, uuid: Uuid, packet: &[u8], now: f64) -> Option<DecryptedData> {
        self.searched += 1;
        let count = self.candidates.len();
        let tracks = self
            .tracks
            .entry(uuid)
            .or_insert_with(|| vec![Track::default(); count]);

        let mut decoded: Vec<Option<DecryptedData>> = Vec::with_capacity(count);
        let mut confirmed = None;
        for (index, candidate) in self.candidates.iter_mut().enumerate() {
            let data = match candidate.decryptor.decrypt_eeg_packet(packet) {
                Ok(data) if data.eeg_uv.len() >= self.fallback_channels => data,
                _ => {
                    decoded.push(None);
                    continue;
                }
            };

            let track = &mut tracks[index];
            let counter = data.counter as u16;
            if let Some(last) = track.last {
                let step = counter.wrapping_sub(last);
                if step != 0 && track.step == Some(step) {
                    track.streak = track.streak.saturating_add(1);
                } else {
                    track.streak = 0;
                }
                track.step = Some(step);
            }
            track.last = Some(counter);

            if confirmed.is_none() && track.streak >= LOCK_STREAK {
                confirmed = Some(index);
            }
            decoded.push(Some(data));
        }

        let (index, is_confirmed) = match confirmed {
            Some(index) => (index, true),
            None if self.searched >= UNCONFIRMED_LOCK_AFTER => {
                // Best streak among the decodable, earliest candidate on a tie
                // — with no streak anywhere, the first decodable one.
                let best = decoded
                    .iter()
                    .enumerate()
                    .filter(|(_, d)| d.is_some())
                    .max_by_key(|(i, _)| (tracks[*i].streak, std::cmp::Reverse(*i)))
                    .map(|(i, _)| i);
                match best {
                    Some(index) => (index, false),
                    None => {
                        self.fail();
                        return None;
                    }
                }
            }
            None => {
                self.fail();
                return None;
            }
        };

        let data = decoded[index].take()?;
        let track = tracks[index];
        let partial = data.eeg_uv.len() < self.required_channels;
        self.lock = Some(Lock {
            index,
            uuid,
            step: if is_confirmed {
                track.step.unwrap_or(1)
            } else {
                1
            },
            confirmed: is_confirmed,
            partial,
            last_counter: Some(data.counter as u16),
        });
        self.tracks.clear();
        self.searched = 0;
        self.mismatches = 0;
        self.clock.reset();

        let key = self.locked_key().unwrap_or_default();
        if is_confirmed {
            log::info!(
                "Decryption locked: {key} on {uuid} (counter step {})",
                track.step.unwrap_or(1)
            );
        } else {
            log::warn!(
                "Decryption locked without counter confirmation after {UNCONFIRMED_LOCK_AFTER} packets: \
                 {key} on {uuid}. Samples may be garbage — check the serial (EMOTIV_RAW_SERIAL)."
            );
        }
        Some(self.finish(data, now))
    }

    fn decode_locked(&mut self, packet: &[u8], now: f64) -> Option<DecryptedData> {
        let lock = self.lock.as_mut()?;
        let decoded = match self.candidates[lock.index]
            .decryptor
            .decrypt_eeg_packet(packet)
        {
            Ok(data) if data.eeg_uv.len() >= self.fallback_channels => Some(data),
            _ => None,
        };

        let in_step = match &decoded {
            Some(data) => {
                let counter = data.counter as u16;
                let in_step = !lock.confirmed
                    || lock
                        .last_counter
                        .is_none_or(|last| counter.wrapping_sub(last) == lock.step);
                lock.last_counter = Some(counter);
                in_step
            }
            None => false,
        };
        if in_step {
            self.mismatches = 0;
        } else {
            self.mismatches += 1;
        }

        if self.mismatches >= RELOCK_AFTER_MISMATCHES {
            log::warn!(
                "{RELOCK_AFTER_MISMATCHES} packets in a row broke the counter under {}; searching again",
                self.locked_key().unwrap_or_default()
            );
            self.lock = None;
            self.mismatches = 0;
            self.fail();
            return None;
        }

        match decoded {
            Some(data) => Some(self.finish(data, now)),
            None => {
                self.fail();
                None
            }
        }
    }

    /// Decrypt a motion packet with the locked key and hold it for the next
    /// EEG packet.
    fn decode_motion(&mut self, packet: &[u8]) {
        let Some(lock) = &self.lock else { return };
        let Ok(raw) = self.candidates[lock.index].decryptor.decrypt_raw(packet) else {
            return;
        };
        if raw.len() < MOTION_MIN_LEN {
            return;
        }
        self.stats.motion_packets += 1;
        self.pending_motion = Some((decode_motion_tentative(&raw), raw));
    }

    /// Stamp, attach pending motion, count.
    fn finish(&mut self, mut data: DecryptedData, now: f64) -> DecryptedData {
        let step = self.lock.as_ref().map_or(1, |l| l.step);
        data.receive_time = now;
        data.timestamp = self.clock.stamp(data.counter as u16, step, now);
        if let Some((motion, raw)) = self.pending_motion.take() {
            data.motion = Some(motion);
            data.motion_raw = Some(raw);
        }
        self.stats.decoded += 1;
        data
    }

    fn fail(&mut self) {
        self.stats.failures += 1;
        if self.stats.failures.is_multiple_of(50) {
            log::warn!(
                "{} of {} BLE notifications not decoded ({}). Check the serial (EMOTIV_RAW_SERIAL).",
                self.stats.failures,
                self.stats.received,
                if self.lock.is_some() { "locked key failing" } else { "no key confirmed yet" },
            );
        }
    }
}

/// **Tentative**: a big-endian `u16` counter, then accelerometer, gyroscope
/// and magnetometer as nine big-endian `i16`, all as raw counts. Not verified
/// against hardware — `DecryptedData::motion_raw` keeps the bytes so it can be.
fn decode_motion_tentative(raw: &[u8]) -> Vec<f64> {
    let counter = f64::from(u16::from_be_bytes([raw[0], raw[1]]));
    std::iter::once(counter)
        .chain(
            raw[2..MOTION_MIN_LEN]
                .chunks_exact(2)
                .map(|b| f64::from(i16::from_be_bytes([b[0], b[1]]))),
        )
        .collect()
}

// ── Timestamps ────────────────────────────────────────────────────────────────

/// Turns packet counters into sample times.
///
/// The first packet anchors the clock to its receive time; every later one is
/// placed `counter delta / step` sample periods from there, so jitter in
/// delivery does not reach the timestamps, dropped packets leave a gap of the
/// right size, and a late packet is back-dated rather than stamped late. If
/// the result strays from the receive time by more than the limits above —
/// a gap longer than the counter can span, clock drift, a counter that does
/// not mean what it is assumed to — it re-anchors.
struct SampleClock {
    period: f64,
    anchor: Option<Anchor>,
}

struct Anchor {
    time: f64,
    last_counter: u16,
    /// Samples since the anchor; fractional if a delta is not a whole number
    /// of steps.
    position: f64,
}

impl SampleClock {
    fn new(rate_hz: f64) -> Self {
        Self {
            period: 1.0 / rate_hz.max(1.0),
            anchor: None,
        }
    }

    fn reset(&mut self) {
        self.anchor = None;
    }

    fn stamp(&mut self, counter: u16, step: u16, now: f64) -> f64 {
        let Some(anchor) = &mut self.anchor else {
            self.anchor = Some(Anchor {
                time: now,
                last_counter: counter,
                position: 0.0,
            });
            return now;
        };

        let delta = counter.wrapping_sub(anchor.last_counter);
        // Within half the counter space, forward; beyond it, a late packet.
        let signed = if delta < 0x8000 {
            f64::from(delta)
        } else {
            f64::from(delta) - 65536.0
        };
        anchor.last_counter = counter;
        anchor.position += signed / f64::from(step.max(1));

        let time = anchor.time + anchor.position * self.period;
        if time > now + CLOCK_AHEAD_LIMIT || time < now - CLOCK_BEHIND_LIMIT {
            self.anchor = Some(Anchor {
                time: now,
                last_counter: counter,
                position: 0.0,
            });
            return now;
        }
        time
    }
}

// ── Per-model parameters ──────────────────────────────────────────────────────

fn packet_len_for_model(model: HeadsetModel) -> usize {
    match model {
        HeadsetModel::Insight
        | HeadsetModel::Insight2
        | HeadsetModel::MN8
        | HeadsetModel::Xtrodes => 16,
        HeadsetModel::EpocX
        | HeadsetModel::EpocPlus
        | HeadsetModel::EpocStd
        | HeadsetModel::EpocFlex => 32,
    }
}

fn fallback_channels_for_model(model: HeadsetModel) -> usize {
    match model {
        HeadsetModel::Insight | HeadsetModel::Insight2 => 5,
        HeadsetModel::MN8 => 2,
        HeadsetModel::Xtrodes => 8,
        HeadsetModel::EpocX
        | HeadsetModel::EpocPlus
        | HeadsetModel::EpocStd
        | HeadsetModel::EpocFlex => 10,
    }
}

/// Every (serial, model) pair, `model` first within each serial.
fn build_candidates(model: HeadsetModel, serials: &[String]) -> Vec<KeyCandidate> {
    let mut models = vec![model];
    for m in [
        HeadsetModel::EpocX,
        HeadsetModel::EpocPlus,
        HeadsetModel::EpocStd,
        HeadsetModel::Insight,
        HeadsetModel::Insight2,
        HeadsetModel::MN8,
        HeadsetModel::Xtrodes,
    ] {
        if !models.contains(&m) {
            models.push(m);
        }
    }

    let mut out = Vec::new();
    for serial in serials {
        for &m in &models {
            if let Ok(decryptor) = Decryptor::new(m, serial.clone()) {
                out.push(KeyCandidate {
                    serial: serial.clone(),
                    model: m,
                    decryptor,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raw::decryption::encrypt_for_test;

    const EEG: Uuid = Uuid::from_u128(0x00001101_0000_0000_0000_000000000000);
    const MOTION: Uuid = Uuid::from_u128(0x00001102_0000_0000_0000_000000000000);
    const RIGHT: &str = "RIGHTSERIAL2";
    const WRONG: &str = "WRONGSERIAL1";
    const RATE: f64 = 256.0;

    /// A 32-byte EPOC X plaintext: counter, 14 channels of 16-bit samples
    /// (channel `i` = `base + i`), battery.
    fn eeg_plain(counter: u16, base: u16) -> Vec<u8> {
        let mut p = vec![0u8; 32];
        p[..2].copy_from_slice(&counter.to_be_bytes());
        for i in 0..14 {
            let v = (base + i as u16) & 0x3FFF;
            p[2 + i * 2..4 + i * 2].copy_from_slice(&v.to_be_bytes());
        }
        p[31] = 200;
        p
    }

    fn eeg_packet(counter: u16) -> Vec<u8> {
        encrypt_for_test(HeadsetModel::EpocX, RIGHT, &eeg_plain(counter, 1000))
    }

    fn pipeline() -> DecodePipeline {
        DecodePipeline::new(HeadsetModel::EpocX, &[WRONG.to_string(), RIGHT.to_string()]).unwrap()
    }

    /// Push counters `from..from+n` one period apart from `t0`.
    fn feed(p: &mut DecodePipeline, from: u16, n: u16, t0: f64) -> Vec<DecryptedData> {
        (0..n)
            .flat_map(|i| {
                let c = from.wrapping_add(i);
                p.push_at(EEG, &eeg_packet(c), t0 + f64::from(i) / RATE)
            })
            .collect()
    }

    #[test]
    fn locks_on_the_right_key_not_the_first() {
        let mut p = pipeline();
        let out = feed(&mut p, 100, 8, 1000.0);

        // Four packets to confirm, then every packet emits.
        assert_eq!(out.len(), 5);
        assert_eq!(p.locked_key().as_deref(), Some("RIGHTSERIAL2/EPOC X"));
        assert_eq!(p.eeg_uuid(), Some(EEG));
        assert_eq!(out[0].counter, 103);
        assert_eq!(out[0].eeg_adc[..3], [1000, 1001, 1002]);
        assert_eq!(p.stats().decoded, 5);
        assert_eq!(p.stats().failures, 3);
    }

    #[test]
    fn locks_with_a_non_unit_counter_step() {
        let mut p = pipeline();
        let out: Vec<_> = (0..6u16)
            .flat_map(|i| p.push_at(EEG, &eeg_packet(i * 256), 1000.0 + f64::from(i) / RATE))
            .collect();
        assert_eq!(out.len(), 3);
        assert_eq!(p.locked_key().as_deref(), Some("RIGHTSERIAL2/EPOC X"));
        // Step-aware timestamps: one period per packet, not 256.
        let dt = out[1].timestamp - out[0].timestamp;
        assert!((dt - 1.0 / RATE).abs() < 1e-9, "dt = {dt}");
    }

    #[test]
    fn falls_back_to_an_unconfirmed_lock() {
        // A counter that never moves cannot confirm anything.
        let mut p = pipeline();
        let mut out = Vec::new();
        for i in 0..UNCONFIRMED_LOCK_AFTER {
            out.extend(p.push_at(EEG, &eeg_packet(7), 1000.0 + i as f64 / RATE));
        }
        assert_eq!(out.len(), 1);
        let key = p.locked_key().unwrap();
        assert!(key.ends_with(":unconfirmed"), "{key}");
    }

    #[test]
    fn reassembles_split_notifications() {
        let mut p = pipeline();
        let mut out = Vec::new();
        for c in 0..8u16 {
            let packet = eeg_packet(c);
            out.extend(p.push_at(EEG, &packet[..20], 1000.0));
            out.extend(p.push_at(EEG, &packet[20..], 1000.0));
        }
        assert_eq!(out.len(), 5);
    }

    #[test]
    fn timestamps_follow_the_counter_not_arrival() {
        let mut p = pipeline();
        let mut out = feed(&mut p, 0, 4, 1000.0);
        // Arrival jitter of ±10 ms must not show.
        for (i, jitter) in [0.010, -0.008, 0.004, -0.010, 0.0].into_iter().enumerate() {
            let c = 4 + i as u16;
            let now = 1000.0 + f64::from(c) / RATE + jitter;
            out.extend(p.push_at(EEG, &eeg_packet(c), now));
        }
        for pair in out.windows(2) {
            let dt = pair[1].timestamp - pair[0].timestamp;
            assert!((dt - 1.0 / RATE).abs() < 1e-9, "dt = {dt}");
        }
        assert!(out.iter().all(|d| d.receive_time > 0.0));
    }

    #[test]
    fn dropped_packets_leave_a_gap_of_the_right_size() {
        let mut p = pipeline();
        let first = feed(&mut p, 0, 5, 1000.0);
        let anchor = first.last().unwrap();
        // Ten packets lost in the air.
        let after = p.push_at(EEG, &eeg_packet(15), 1000.0 + 15.0 / RATE);
        let gap = after[0].timestamp - anchor.timestamp;
        assert!((gap - 11.0 / RATE).abs() < 1e-9, "gap = {gap}");
    }

    #[test]
    fn clock_re_anchors_when_the_counter_cannot_explain_the_gap() {
        let mut p = pipeline();
        feed(&mut p, 0, 5, 1000.0);
        // Ten seconds of silence, but the counter only moved by one.
        let out = p.push_at(EEG, &eeg_packet(5), 1010.0);
        assert_eq!(out[0].timestamp, 1010.0);
    }

    #[test]
    fn motion_rides_on_the_next_eeg_packet() {
        let mut p = pipeline();
        feed(&mut p, 0, 4, 1000.0);

        let mut plain = vec![0u8; 32];
        plain[..2].copy_from_slice(&42u16.to_be_bytes());
        for (i, v) in [1i16, -2, 3, -4, 5, -6, 7, -8, 9].into_iter().enumerate() {
            plain[2 + i * 2..4 + i * 2].copy_from_slice(&v.to_be_bytes());
        }
        let motion = encrypt_for_test(HeadsetModel::EpocX, RIGHT, &plain);
        assert!(p.push_at(MOTION, &motion, 1000.1).is_empty());
        assert_eq!(p.stats().motion_packets, 1);

        let out = p.push_at(EEG, &eeg_packet(4), 1000.0 + 4.0 / RATE);
        assert_eq!(
            out[0].motion.as_deref(),
            Some(&[42.0, 1.0, -2.0, 3.0, -4.0, 5.0, -6.0, 7.0, -8.0, 9.0][..])
        );
        assert_eq!(out[0].motion_raw.as_deref(), Some(&plain[..]));

        // Attached once, not repeated.
        let next = p.push_at(EEG, &eeg_packet(5), 1000.0 + 5.0 / RATE);
        assert!(next[0].motion.is_none());
    }

    #[test]
    fn relocks_when_the_counter_stops_making_sense() {
        let mut p = pipeline();
        feed(&mut p, 0, 4, 1000.0);
        assert_eq!(p.locked_key().as_deref(), Some("RIGHTSERIAL2/EPOC X"));

        // The headset now encrypts with the other serial: under the locked key
        // its counters are noise.
        let mut relocked = Vec::new();
        for c in 0..(RELOCK_AFTER_MISMATCHES as u16 + 8) {
            let packet = encrypt_for_test(HeadsetModel::EpocX, WRONG, &eeg_plain(c, 2000));
            relocked.extend(p.push_at(EEG, &packet, 1001.0 + f64::from(c) / RATE));
        }
        assert_eq!(p.locked_key().as_deref(), Some("WRONGSERIAL1/EPOC X"));
        assert_eq!(relocked.last().unwrap().eeg_adc[0], 2000);
    }

    #[test]
    fn a_dropped_packet_does_not_break_the_lock() {
        let mut p = pipeline();
        feed(&mut p, 0, 4, 1000.0);
        for c in (4..400u16).filter(|c| c % 10 != 0) {
            p.push_at(EEG, &eeg_packet(c), 1000.0 + f64::from(c) / RATE);
        }
        assert_eq!(p.locked_key().as_deref(), Some("RIGHTSERIAL2/EPOC X"));
    }

    #[test]
    fn resume_keeps_the_key() {
        let mut p = pipeline();
        feed(&mut p, 0, 4, 1000.0);
        p.resume();
        let out = p.push_at(EEG, &eeg_packet(500), 2000.0);
        assert_eq!(out.len(), 1, "no re-search after a reconnect");
        assert_eq!(out[0].timestamp, 2000.0, "clock re-anchored");
    }
}
