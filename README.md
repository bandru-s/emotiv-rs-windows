# EMOTIV EEG live viewer

The Electron app connects to EMOTIV headsets directly over Bluetooth using the
`emotiv` Rust crate (version 0.0.14). It does not use Cortex or the EMOTIV
Launcher.

## Requirements

- Node.js
- Rust 1.94 or newer, with Cargo
- A supported Emotiv headset with Bluetooth enabled

Install the JavaScript dependencies and launch the app from this directory:

```powershell
npm install
npm start
```

`npm start` builds the Rust bridge before opening Electron. In the app, choose
**Headsetek keresése**, select a detected headset, then choose **Csatlakozás**.
The dashboard displays live EEG values in microvolts, per-channel contact
quality, overall signal quality, battery level, and a selectable channel plot.

The displayed EEG data is for experimentation and is not medical advice.
