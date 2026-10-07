let title = document.getElementById("title");
let count = 0;
setInterval(() => {
    count++;
    title.innerText = `Hello from Electron renderer! (edited) ${count}`;
}, 1000); 

const information = document.getElementById('info');
information.innerText = `This app is using Chrome (v${window.versions.chrome()}), Node.js (v${window.versions.node()}), and Electron (v${window.versions.electron()})`;