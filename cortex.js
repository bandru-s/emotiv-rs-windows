const fs = require('node:fs');
const path = require('node:path');
const WebSocket = require('ws');

const CORTEX_URL = 'wss://localhost:6868';
const REQUEST_TIMEOUT_MS = 15000;
// Az EMOTIV saját gyökértanúsítványa; a Node nem a Windows tanúsítványtárat használja.
const EMOTIV_ROOT_CA = fs.readFileSync(path.join(__dirname, 'rootCA.pem'));

class CortexClient {
  constructor() {
    this.socket = null;
    this.nextId = 1;
    this.pendingRequests = new Map();
  }

  async connect() {
    if (this.socket && this.socket.readyState === WebSocket.OPEN) {
      return;
    }

    const socket = new WebSocket(CORTEX_URL, { ca: EMOTIV_ROOT_CA });
    this.socket = socket;

    await new Promise((resolve, reject) => {
      let opened = false;
      const timeout = setTimeout(() => {
        socket.close();
        reject(new Error('Időtúllépés a Cortex szolgáltatáshoz csatlakozáskor.'));
      }, REQUEST_TIMEOUT_MS);

      socket.once('open', () => {
        opened = true;
        clearTimeout(timeout);
        resolve();
      });

      socket.on('error', (cause) => {
        clearTimeout(timeout);
        console.error('Cortex WebSocket error:', cause);
        const error = new Error(`Nem sikerült csatlakozni a Cortexhez (${cause.code || cause.message}). Ellenőrizd, hogy fut-e az EMOTIV Launcher.`);
        if (this.socket === socket) {
          this.socket = null;
        }
        this.rejectPendingRequests(error);
        if (!opened) {
          reject(error);
        }
        socket.close();
      });

      socket.on('message', (data) => this.handleMessage(data));
      socket.on('close', () => {
        clearTimeout(timeout);
        if (this.socket === socket) {
          this.socket = null;
        }
        this.rejectPendingRequests(new Error('A Cortex-kapcsolat megszakadt.'));
      });
    });
  }

  async call(method, params) {
    await this.connect();

    const id = this.nextId++;
    const request = { id, jsonrpc: '2.0', method };
    if (params) {
      request.params = params;
    }

    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.pendingRequests.delete(id);
        reject(new Error(`A Cortex API időtúllépést jelzett (${method}).`));
      }, REQUEST_TIMEOUT_MS);

      this.pendingRequests.set(id, { resolve, reject, timeout });
      try {
        this.socket.send(JSON.stringify(request));
      } catch (error) {
        clearTimeout(timeout);
        this.pendingRequests.delete(id);
        reject(error);
      }
    });
  }

  handleMessage(data) {
    let response;
    try {
      response = JSON.parse(String(data));
    } catch (error) {
      console.error('Cortex returned invalid JSON:', error);
      this.rejectPendingRequests(new Error('Érvénytelen választ küldött a Cortex.'));
      return;
    }

    const pending = this.pendingRequests.get(response.id);
    if (!pending) {
      return;
    }

    clearTimeout(pending.timeout);
    this.pendingRequests.delete(response.id);

    if (response.error) {
      pending.reject(new Error(response.error.message || 'A Cortex API-hívás sikertelen.'));
      return;
    }

    pending.resolve(response.result);
  }

  rejectPendingRequests(error) {
    for (const [id, pending] of this.pendingRequests) {
      clearTimeout(pending.timeout);
      pending.reject(error);
      this.pendingRequests.delete(id);
    }
  }
}

const cortex = new CortexClient();

async function checkCortexAccess({ clientId, clientSecret }) {
  const cortexInfo = await cortex.call('getCortexInfo');
  const users = await cortex.call('getUserLogin');

  if (!Array.isArray(users) || users.length === 0) {
    return {
      status: 'login-required',
      cortexVersion: cortexInfo.version,
      message: 'Jelentkezz be az EMOTIV Launcherbe, majd próbáld újra.'
    };
  }

  if (users[0].currentOSUId !== users[0].loggedInOSUId) {
    return {
      status: 'wrong-os-user',
      cortexVersion: cortexInfo.version,
      message: 'Az EMOTIV Launcherben másik Windows-felhasználó van bejelentkezve. Használd ugyanazt a Windows-fiókot, majd próbáld újra.'
    };
  }

  const access = await cortex.call('requestAccess', { clientId, clientSecret });
  if (!access.accessGranted) {
    return {
      status: 'approval-required',
      cortexVersion: cortexInfo.version,
      message: 'Engedélyezd ezt az alkalmazást az EMOTIV Launcherben, majd kattints újra az ellenőrzésre.'
    };
  }

  const authorization = await cortex.call('authorize', { clientId, clientSecret });
  if (authorization.warning) {
    return {
      status: 'eula-required',
      cortexVersion: cortexInfo.version,
      message: 'Az EMOTIV Launcherben fogadd el a Cortex EULA-t, majd próbáld újra.'
    };
  }

  return {
    status: 'authorized',
    cortexVersion: cortexInfo.version,
    message: 'A Cortex-kapcsolat és az alkalmazásengedély rendben van.'
  };
}

async function queryHeadsets() {
  await cortex.call('controlDevice', { command: 'refresh' });
  await new Promise((resolve) => setTimeout(resolve, 3000));
  const headsets = await cortex.call('queryHeadsets');

  return headsets.map((headset) => ({
    id: headset.id,
    status: headset.status,
    connectedBy: headset.connectedBy,
    firmware: headset.firmwareDisplay || headset.firmware,
    mode: headset.settings && headset.settings.mode,
    eegRate: headset.settings && headset.settings.eegRate,
    sensors: headset.sensors || []
  }));
}

module.exports = { checkCortexAccess, queryHeadsets };
