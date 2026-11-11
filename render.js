const statusElement = document.getElementById('connection-status');
const deviceSelect = document.getElementById('device-select');
const scanButton = document.getElementById('scan-button');
const connectButton = document.getElementById('connect-button');
const disconnectButton = document.getElementById('disconnect-button');
const readings = document.getElementById('readings');
const channelSelect = document.getElementById('chart-channel');
const chart = document.getElementById('eeg-chart');
const chartContext = chart.getContext('2d');
const batteryElement = document.getElementById('battery');
const signalElement = document.getElementById('signal-quality');
const channelHistory = new Map();
const historyLimit = 500;
const qualityLabels = ['Nincs jel', 'Gyenge', 'Közepes', 'Jó', 'Kiváló'];
let channels = [];
let currentDevices = [];
let connected = false;
let latestPacket;
let lastUiUpdate = 0;
let lastDiagnostic = '';

function diagnosticHint(message) {
  const missingDevice = message.match(/NetworkError: no device at (.+)$/i);
  if (!missingDevice) return message;
  return `A Windows Bluetooth nem tudta feloldani a headset címét (${missingDevice[1].trim()}). Mivel ez a hiba a külön emotiv-rs CLI-ben is jelentkezik, nem az Electron felületről ered. Kapcsold ki, majd vissza a headsetet; zárd be a Cortexet/Launchert és minden más, headsetet használó alkalmazást; ezután keress újra. Ha továbbra is fennáll, távolítsd el a headsetet a Windows Bluetooth-beállításaiban, párosítsd újra, majd próbáld meg ismét a CLI-ből.`;
}

function setStatus(message, state) {
  statusElement.textContent = message;
  statusElement.dataset.state = state || 'idle';
}

function setDevices(devices) {
  currentDevices = devices;
  deviceSelect.replaceChildren();
  if (devices.length === 0) {
    deviceSelect.add(new Option('Nem található headset', ''));
    connectButton.disabled = true;
    return;
  }
  for (const device of devices) {
    const description = `${device.model} · ${device.serial || device.name} · ${device.battery_percent}%`;
    deviceSelect.add(new Option(description, device.ble_id));
  }
  connectButton.disabled = connected;
}

function setChannels(names) {
  channels = names;
  channelSelect.replaceChildren();
  channelHistory.clear();
  for (const name of names) {
    channelSelect.add(new Option(name, name));
    channelHistory.set(name, []);
  }
  if (names.length) channelSelect.value = names[0];
  renderReadings();
  drawChart();
}

function renderReadings() {
  if (!latestPacket || channels.length === 0) return;
  readings.replaceChildren();
  channels.forEach((name, index) => {
    const value = latestPacket.eeg_uv[index];
    const quality = latestPacket.contact_quality[index] ?? 0;
    const card = document.createElement('article');
    card.className = 'reading';
    const heading = document.createElement('strong');
    heading.textContent = name;
    const measurement = document.createElement('span');
    measurement.textContent = Number.isFinite(value) ? `${value.toFixed(1)} µV` : '— µV';
    const qualityText = document.createElement('small');
    qualityText.textContent = qualityLabels[quality] || `Jelminőség: ${quality}/4`;
    const qualityBar = document.createElement('span');
    qualityBar.className = 'quality-bar';
    qualityBar.style.setProperty('--quality', `${Math.min(quality, 4) * 25}%`);
    card.append(heading, measurement, qualityText, qualityBar);
    readings.append(card);
  });
}

function drawChart() {
  const width = chart.clientWidth;
  const height = chart.clientHeight;
  if (!width || !height) return;
  const ratio = window.devicePixelRatio || 1;
  chart.width = Math.round(width * ratio);
  chart.height = Math.round(height * ratio);
  chartContext.setTransform(ratio, 0, 0, ratio, 0, 0);
  chartContext.clearRect(0, 0, width, height);
  chartContext.strokeStyle = '#28333d';
  chartContext.lineWidth = 1;
  chartContext.beginPath();
  chartContext.moveTo(0, height / 2);
  chartContext.lineTo(width, height / 2);
  chartContext.stroke();

  const values = channelHistory.get(channelSelect.value) || [];
  if (values.length < 2) return;
  const scale = height / 2 / 250;
  chartContext.strokeStyle = '#59d5bd';
  chartContext.lineWidth = 2;
  chartContext.beginPath();
  values.forEach((value, index) => {
    const x = (index / (values.length - 1)) * width;
    const y = height / 2 - Math.max(-height / 2, Math.min(height / 2, value * scale));
    if (index === 0) chartContext.moveTo(x, y);
    else chartContext.lineTo(x, y);
  });
  chartContext.stroke();
}

function handlePacket(packet) {
  latestPacket = packet;
  channels.forEach((name, index) => {
    const values = channelHistory.get(name);
    const value = packet.eeg_uv[index];
    if (!values || !Number.isFinite(value)) return;
    values.push(value);
    if (values.length > historyLimit) values.shift();
  });
  const now = performance.now();
  if (now - lastUiUpdate < 100) return;
  lastUiUpdate = now;
  batteryElement.textContent = `${packet.battery_percent}%`;
  signalElement.textContent = qualityLabels[packet.signal_quality] || `${packet.signal_quality}/4`;
  renderReadings();
  drawChart();
}

async function sendCommand(command) {
  try {
    await window.emotiv.sendCommand(command);
  } catch (error) {
    setStatus(error.message, 'error');
  }
}

scanButton.addEventListener('click', () => {
  lastDiagnostic = '';
  scanButton.disabled = true;
  setStatus('EMOTIV headsetek keresése Bluetoothon...', 'connecting');
  void sendCommand({ command: 'discover' });
});

connectButton.addEventListener('click', () => {
  const device = currentDevices.find((item) => item.ble_id === deviceSelect.value);
  if (device) {
    lastDiagnostic = '';
    void sendCommand({ command: 'connect', device: device.ble_id });
  }
});

disconnectButton.addEventListener('click', () => {
  void sendCommand({ command: 'disconnect' });
});

channelSelect.addEventListener('change', drawChart);
window.addEventListener('resize', drawChart);

window.emotiv.onEvent((event) => {
  if (event.type === 'diagnostic') {
    lastDiagnostic = event.message;
    setStatus(diagnosticHint(lastDiagnostic), 'error');
    return;
  }
  if (event.type === 'error') {
    setStatus(event.message, 'error');
    scanButton.disabled = false;
    return;
  }
  if (event.type === 'status') {
    const isDisconnected = event.state === 'disconnected';
    const message = isDisconnected && lastDiagnostic
      ? `${diagnosticHint(lastDiagnostic)}`
      : event.message;
    setStatus(message, event.state);
    connected = event.state === 'streaming' || event.state === 'connecting' || event.state === 'reconnecting';
    disconnectButton.disabled = !connected;
    connectButton.disabled = connected || currentDevices.length === 0;
    if (event.state === 'disconnected' || event.state === 'ready') {
      setChannels([]);
      latestPacket = undefined;
      batteryElement.textContent = '—';
      signalElement.textContent = '—';
    }
    return;
  }
  if (event.type === 'devices') {
    scanButton.disabled = false;
    setDevices(event.devices);
    setStatus(event.devices.length ? `${event.devices.length} headset található.` : 'Nem található headset. Kapcsold be, majd próbáld újra.', event.devices.length ? 'ready' : 'idle');
    return;
  }
  if (event.type === 'connected') {
    setChannels(event.channels);
    setStatus(`${event.model} csatlakoztatva. Várakozás az EEG-adatokra...`, 'connecting');
    connected = true;
    connectButton.disabled = true;
    disconnectButton.disabled = false;
    return;
  }
  if (event.type === 'data') handlePacket(event.data);
});

window.emotiv.getStatus().then((status) => {
  if (status) setStatus(status.message, status.state);
}).catch((error) => setStatus(error.message, 'error'));

setDevices([]);
drawChart();
