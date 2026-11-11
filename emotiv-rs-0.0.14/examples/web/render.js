// The worker half: ring buffers and every canvas operation.
//
// Nothing here knows about Bluetooth. It is handed an `OffscreenCanvas`, then a
// stream of `{ row }`, `{ samples }`, `{ size }`, `{ smooth }` and `{ reset }`
// messages, and it draws. Keeping it that ignorant is what
// makes it movable: the main thread has to own the radio (see the note at the
// top of `app.js`), but nothing about turning numbers into pixels needs to be
// there.
//
// A classic worker rather than a module one: it imports nothing, so there is
// nothing for `type: 'module'` to buy.

// How much history is on screen. Held in seconds rather than samples because
// headsets do not share a rate — EPOC X is 256 Hz, Insight 128 — so each row's
// buffer is sized from the rate the module declares for it.
const WINDOW_SECONDS = 4;

// The TUI smooths over 9 samples. Kept here as the duration that works out to
// at 256 Hz — ≈35 ms — so the same amount of *time* is averaged whatever the
// headset's rate.
const SMOOTH_SECONDS = 9 / 256;

// Distinct rather than pretty; fourteen for the EPOC family, cycled beyond.
const COLOURS = [
  '#4cc9f0', '#f4a261', '#90be6d', '#f28cb1', '#b8a1ff', '#ffd166', '#8ecae6',
  '#e07a5f', '#56cfe1', '#e9c46a', '#a7c957', '#ff8fab', '#cdb4db', '#bde0fe',
];

const LABEL = '#8a8a94';

// A flat electrode should read as flat rather than have its noise amplified
// to fill the row.
const FLOOR_UV = 20;

const rows = new Map();

function ring(capacity) {
  return { samples: new Float64Array(capacity), count: 0 };
}

const pick = (value, index) => (typeof value === 'function' ? value(index) : value);

/// Create a row, or return the one already there.
///
/// `hz` decides the buffer size and therefore the time span. A row that
/// receives a sample before its declaration is created at a default rate
/// rather than dropped, and the declaration then corrects it.
function row(index, hz, name) {
  const existing = rows.get(index);
  const capacity = Math.max(2, Math.round(hz * WINDOW_SECONDS));

  if (existing && existing.series[0].samples.length === capacity) {
    if (name) existing.name = name;
    return existing;
  }

  const created = {
    order: index,
    name: name || (existing && existing.name) || `ch${index}`,
    hz,
    unit: 'µV',
    digits: 0,
    floor: FLOOR_UV,
    axes: null,
    colours: [COLOURS[index % COLOURS.length]],
    // A rate correction starts the history again rather than reinterpreting
    // samples that were taken at a different spacing.
    series: [ring(capacity)],
  };
  rows.set(index, created);
  return created;
}

// Fallback for a sample that somehow precedes its declaration.
const DEFAULT_HZ = 128;

function rowFor(index) {
  return rows.get(index) || row(index, DEFAULT_HZ);
}

function push(target, value) {
  target.samples[target.count % target.samples.length] = value;
  target.count += 1;
}

let canvas = null;
let context = null;
// CSS pixels. The canvas is sized in device pixels and scaled once, which is
// what keeps the traces sharp on a retina display.
let width = 0;
let height = 0;
let ratio = 1;
let smooth = true;

// `requestAnimationFrame` is defined in a dedicated worker, and is the right
// thing to use: it stops when the page is hidden, where a timer would carry on
// drawing frames nobody sees. The fallback is for the case where it is not.
const schedule =
  typeof requestAnimationFrame === 'function'
    ? (callback) => requestAnimationFrame(callback)
    : (callback) => setTimeout(callback, 16);

// Kept apart from the message that carried it, so that a size arriving before
// the canvas — which message order rules out, but nothing else would — is
// remembered rather than thrown away.
function applySize() {
  if (!context) return;
  // Guarded: a layout can report a zero-sized box mid-pass, and a canvas
  // cannot have a zero dimension.
  canvas.width = Math.max(1, Math.round(width * ratio));
  canvas.height = Math.max(1, Math.round(height * ratio));
  context.setTransform(ratio, 0, 0, ratio, 0, 0);
}

self.onmessage = (event) => {
  const message = event.data;

  if (message.canvas) {
    canvas = message.canvas;
    context = canvas.getContext('2d');
    applySize();
    schedule(draw);
  }

  if (message.size) {
    ({ width, height, ratio } = message.size);
    applySize();
  }

  if (message.row) {
    const { index, hz, name } = message.row;
    row(index, hz, name);
  }

  if (message.samples) {
    const { channels, microvolts } = message.samples;
    for (let i = 0; i < channels.length; i += 1) {
      push(rowFor(channels[i]).series[0], microvolts[i]);
    }
  }

  if (message.smooth !== undefined) {
    smooth = message.smooth;
  }

  if (message.reset) {
    rows.clear();
  }
};

// ── Drawing ─────────────────────────────────────────────────────────────────

/// The window's samples, oldest first and mean removed.
///
/// The mean goes because raw EEG sits on a large per-electrode DC offset:
/// plotted against zero, the interesting part is a flat line jammed against
/// the edge of its row.
function windowOf(target) {
  const span = target.samples.length;
  const count = Math.min(target.count, span);
  const values = new Float64Array(count);
  let total = 0;
  for (let i = 0; i < count; i += 1) {
    // Oldest first: the ring buffer's newest sample is at `count - 1`.
    const value = target.samples[(target.count - count + i) % span];
    values[i] = value;
    total += value;
  }
  const mean = count > 0 ? total / count : 0;
  for (let i = 0; i < count; i += 1) values[i] -= mean;
  return values;
}

/// Centred moving average, edges clamped so the length is preserved — the same
/// shape as `smooth_signal` in the TUI, computed off a prefix sum so the window
/// size costs nothing.
function smoothed(values, window) {
  if (values.length < 3 || window < 2) return values;
  const half = window >> 1;
  const prefix = new Float64Array(values.length + 1);
  for (let i = 0; i < values.length; i += 1) prefix[i + 1] = prefix[i] + values[i];

  const out = new Float64Array(values.length);
  for (let i = 0; i < values.length; i += 1) {
    const start = Math.max(0, i - half);
    const end = Math.min(values.length, i + half + 1);
    out[i] = (prefix[end] - prefix[start]) / (end - start);
  }
  return out;
}

/// A dimmed version of a trace colour, for the raw signal drawn behind the
/// smoothed one. Derived rather than kept as a second palette, so adding a
/// colour above cannot leave a missing dim twin.
function dim(colour) {
  const value = parseInt(colour.slice(1), 16);
  const scale = (channel) => Math.round(channel * 0.42);
  return `rgb(${scale((value >> 16) & 255)} ${scale((value >> 8) & 255)} ${scale(value & 255)})`;
}

function trace(values, span, peak, middle, rowHeight, colour) {
  context.beginPath();
  for (let i = 0; i < values.length; i += 1) {
    // Against the row's own capacity, which is `WINDOW_SECONDS` of it.
    const x = (i / (span - 1)) * width;
    const y = middle - (values[i] / peak) * (rowHeight * 0.42);
    if (i === 0) context.moveTo(x, y);
    else context.lineTo(x, y);
  }
  context.lineWidth = 1;
  context.strokeStyle = colour;
  context.stroke();
}

function draw() {
  context.clearRect(0, 0, width, height);

  const visible = [...rows.values()].sort((a, b) => a.order - b.order);
  if (visible.length === 0) {
    schedule(draw);
    return;
  }

  const rowHeight = height / visible.length;

  visible.forEach((entry, index) => {
    const middle = index * rowHeight + rowHeight / 2;
    const span = entry.series[0].samples.length;
    const windows = entry.series.map(windowOf);

    let peak = entry.floor;
    for (const values of windows) {
      for (let i = 0; i < values.length; i += 1) {
        peak = Math.max(peak, Math.abs(values[i]));
      }
    }

    // The same averaged duration whatever the rate.
    const window = Math.max(2, Math.round(SMOOTH_SECONDS * entry.hz));

    windows.forEach((values, series) => {
      const colour = entry.colours[series % entry.colours.length];
      if (smooth) {
        // Raw behind, smoothed in front — the TUI's arrangement, so that
        // smoothing never hides what the headset actually sent.
        trace(values, span, peak, middle, rowHeight, dim(colour));
        trace(smoothed(values, window), span, peak, middle, rowHeight, colour);
      } else {
        trace(values, span, peak, middle, rowHeight, colour);
      }
    });

    // ── Label ────────────────────────────────────────────────────────────
    context.font = '11px ui-monospace, SFMono-Regular, monospace';
    context.fillStyle = LABEL;
    const caption = `${entry.name}  ±${peak.toFixed(entry.digits)} ${entry.unit}`;
    context.fillText(caption, 8, index * rowHeight + 14);

    // For a multi-series row, say which colour is which axis.
    if (entry.axes) {
      let x = 8 + context.measureText(`${caption}  `).width;
      entry.axes.forEach((axis, series) => {
        context.fillStyle = entry.colours[series];
        context.fillText(axis, x, index * rowHeight + 14);
        x += context.measureText(`${axis} `).width;
      });
    }
  });

  schedule(draw);
}
