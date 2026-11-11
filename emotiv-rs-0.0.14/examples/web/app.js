// The page half of the web example: two threads, split where the browser
// forces the split.
//
// Here, on the main thread: Web Bluetooth and the wasm module that drives it.
// That is not a preference. `navigator.bluetooth` is exposed on `Window` and
// not on `WorkerNavigator`, so a worker cannot reach the radio at all; and
// `requestDevice` has to be called from a user gesture, which only this thread
// has. The shim marshals directly into the module's memory besides, so the
// module has to share a realm with it.
//
// In `render.js`: the ring buffers and every canvas operation, through an
// `OffscreenCanvas`. Fourteen traces redrawn at 60 Hz is the part that costs
// frames, so the drawing is what moves off.
//
// `webbluetooth.js` is the shim from `webbluetooth-wasm`, copied in by
// `build.sh` rather than vendored, so it cannot drift from the module's ABI.
import { instantiate } from './webbluetooth.js';

const connect = document.querySelector('#connect');
const disconnect = document.querySelector('#disconnect');
const serialInput = document.querySelector('#serial');
const decodeLine = document.querySelector('#decode');
const keyLine = document.querySelector('#key');
const statusLine = document.querySelector('#status');
const batteryLine = document.querySelector('#battery');
const signalLine = document.querySelector('#signal');
const rateLine = document.querySelector('#rate');
const smoothing = document.querySelector('#smooth');
const canvas = document.querySelector('#traces');

// Room for one frame of samples, with margin: 256 Hz across fourteen channels
// is about 60 a frame at 60 Hz. `emotiv_frame` flushes early rather than
// dropping anything if the page stalls.
const BATCH = 4096;

const channels = new Uint8Array(BATCH);
const microvolts = new Float64Array(BATCH);
let pending = 0;

let worker = null;

// Counted here, where packets cross out of the module: a zero here says
// nothing arrived (or decryption never locked), not that nothing was drawn.
let packets = 0;
let packetsAtLastTick = 0;

const SIGNAL = ['none', 'poor', 'fair', 'good', 'excellent'];

// Latest battery and signal. They arrive with every packet — hundreds a
// second — but are written to the page once a second, with the rest.
let battery = null;
let signal = null;

// ── The module's imports ────────────────────────────────────────────────────

let instance = null;
const decoder = new TextDecoder();

// Views built per call: the buffer is detached whenever linear memory grows,
// so a cached view would go stale.
function text(pointer, length) {
  return decoder.decode(new Uint8Array(instance.exports.memory.buffer, pointer, length));
}

// Batched rather than posted per packet, and copied rather than transferred:
// at this size the copy is nothing, and one reused pair of buffers keeps the
// allocation rate flat.
function flush() {
  if (pending === 0) return;
  worker.postMessage({
    samples: {
      channels: channels.slice(0, pending),
      microvolts: microvolts.slice(0, pending),
    },
  });
  pending = 0;
}

// What `examples/web/lib.rs` declares as its `emotiv` import namespace. The
// shim spreads this alongside its own `webbluetooth` namespace.
const imports = {
  emotiv: {
    emotiv_status(pointer, length) {
      statusLine.textContent = text(pointer, length);
    },

    emotiv_row(index, hz, pointer, length) {
      worker.postMessage({ row: { index, hz, name: text(pointer, length) } });
    },

    // One decrypted packet: a sample per channel, as f64 µV.
    emotiv_frame(pointer, length) {
      packets += 1;
      if (pending + length > BATCH) flush();
      // `pointer` is a byte offset; f64s are 8-byte aligned in Rust's Vec.
      const values = new Float64Array(instance.exports.memory.buffer, pointer, length);
      for (let i = 0; i < length; i += 1) {
        channels[pending] = i;
        microvolts[pending] = values[i];
        pending += 1;
      }
    },

    emotiv_quality(percent, quality) {
      battery = percent;
      signal = quality;
    },

    // Decode statistics, in answer to `emotiv_poll`. Received-but-not-decoded
    // is the signature of a wrong serial: notifications arrive, and no key
    // derived from the candidates opens them.
    emotiv_stats(received, decoded, failures, pointer, length) {
      decodeLine.textContent = `${received} rx · ${decoded} ok · ${failures} failed`;
      keyLine.textContent = length > 0 ? text(pointer, length) : 'searching…';
    },

    // Copy the serial field into the module's buffer; return the byte count.
    emotiv_serial(pointer, capacity) {
      const bytes = new TextEncoder().encode(serialInput.value.trim());
      const length = Math.min(bytes.length, capacity);
      new Uint8Array(instance.exports.memory.buffer, pointer, length).set(bytes.subarray(0, length));
      return length;
    },

    // `SystemTime::now()` panics on wasm32-unknown-unknown, so the library
    // asks the page for the time instead.
    emotiv_now() {
      return Date.now();
    },

    // The library's own logs. Silent otherwise in a browser.
    emotiv_log(level, pointer, length) {
      const line = text(pointer, length);
      // log::Level is 1..=5. `trace` is mapped to `debug` rather than
      // `console.trace`, which would print a JS stack that means nothing here.
      const method = [, 'error', 'warn', 'info', 'debug', 'debug'][level] || 'log';
      console[method](`[emotiv] ${line}`);
    },

    // Bracketed around the whole streaming task on the Rust side, so this is
    // also how the button comes back after a failure.
    emotiv_active(active) {
      connect.disabled = active === 1;
      disconnect.disabled = active !== 1;
      serialInput.disabled = active === 1;
      // Cleared when a stream starts, not when one ends: a finished run stays
      // on screen to be looked at.
      if (active === 1) worker.postMessage({ reset: true });
    },
  },
};

// ── Wiring ──────────────────────────────────────────────────────────────────

function postSize() {
  const bounds = canvas.getBoundingClientRect();
  worker.postMessage({
    size: {
      width: bounds.width,
      height: bounds.height,
      ratio: window.devicePixelRatio || 1,
    },
  });
}

function fail(message) {
  statusLine.textContent = message;
  connect.disabled = true;
}

if (!navigator.bluetooth) {
  // The usual cause is Safari or Firefox, where Web Bluetooth is not
  // implemented, and no amount of retrying will help.
  fail('This browser has no Web Bluetooth. Chrome or Edge, over localhost or HTTPS.');
} else if (!canvas.transferControlToOffscreen) {
  fail('This browser cannot hand a canvas to a worker (OffscreenCanvas missing).');
} else {
  worker = new Worker('./render.js');

  // Transferred, not copied: after this the worker draws and this thread
  // cannot, which is the point of it.
  const offscreen = canvas.transferControlToOffscreen();
  worker.postMessage({ canvas: offscreen }, [offscreen]);

  postSize();
  new ResizeObserver(postSize).observe(canvas);

  const postSmoothing = () => worker.postMessage({ smooth: smoothing.checked });
  smoothing.addEventListener('change', postSmoothing);
  postSmoothing();

  instance = await instantiate(fetch('./web.wasm'), imports);

  // Hand samples over a frame at a time.
  const pump = () => {
    flush();
    requestAnimationFrame(pump);
  };
  requestAnimationFrame(pump);

  // Once a second: the useful question is whether packets are arriving at
  // all, and at roughly the headset's sampling rate.
  setInterval(() => {
    const perSecond = packets - packetsAtLastTick;
    packetsAtLastTick = packets;
    rateLine.textContent = packets === 0 ? '—' : `${packets} (${perSecond}/s)`;
    if (battery !== null) batteryLine.textContent = `${battery.toFixed(0)}%`;
    if (signal !== null) signalLine.textContent = SIGNAL[signal] ?? String(signal);
    // Answered asynchronously through `emotiv_stats`; a no-op when idle.
    instance.exports.emotiv_poll();
  }, 1000);

  connect.disabled = false;
  statusLine.textContent = 'Ready.';

  connect.addEventListener('click', () => {
    // Straight through to the module, still inside the click: the chooser has
    // to open while the gesture is live.
    instance.exports.emotiv_connect();
  });

  disconnect.addEventListener('click', () => instance.exports.emotiv_disconnect());

  // Remembered across reloads: a serial that works is worth not retyping.
  serialInput.value = localStorage.getItem('emotiv-serial') ?? '';
  serialInput.addEventListener('change', () =>
    localStorage.setItem('emotiv-serial', serialInput.value.trim()),
  );
}
