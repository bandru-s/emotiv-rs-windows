const cortexCheckButton = document.getElementById('cortex-check');
const cortexStatus = document.getElementById('cortex-status');
const headsetQueryButton = document.getElementById('headset-query');
const headsetInfo = document.getElementById('headset-info');

headsetQueryButton.addEventListener('click', async () => {
    headsetQueryButton.disabled = true;
    headsetInfo.innerText = 'Headset keresése (kb. 3 mp)...';

    try {
        const headsets = await window.cortex.queryHeadsets();
        headsetInfo.innerText = headsets.length === 0
            ? 'Nem található headset. Kapcsold be az EPOC X-et.'
            : headsets.map((h) => [
                `Headset: ${h.id}`,
                `Állapot: ${h.status} (${h.connectedBy})`,
                `Firmware: ${h.firmware}`,
                `Mód: ${h.mode}, EEG mintavétel: ${h.eegRate} Hz`,
                `Szenzorok (${h.sensors.length}): ${h.sensors.join(', ')}`
            ].join('\n')).join('\n\n');
    } catch (error) {
        headsetInfo.innerText = error.message;
    } finally {
        headsetQueryButton.disabled = false;
    }
});

cortexCheckButton.addEventListener('click', async () => {
    cortexCheckButton.disabled = true;
    cortexStatus.innerText = 'Kapcsolódás a Cortexhez...';

    try {
        const result = await window.cortex.checkAccess();
        cortexStatus.innerText = result.cortexVersion
            ? `${result.message} (Cortex ${result.cortexVersion})`
            : result.message;
    } catch (error) {
        cortexStatus.innerText = error.message;
    } finally {
        cortexCheckButton.disabled = false;
    }
});