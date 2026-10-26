// Web Serial → WebAssembly → Bevy: drive a live 3D DGT board in the browser.
//
// Rust/wasm can't open a serial port, so this file does the I/O: it opens the
// Web Serial port, writes the init sequence to start the board, and pushes every
// received chunk into the wasm via `push_bytes`. The Bevy app (started by
// `start`) drains those bytes through `dgtboard-core` and reconciles the 3D
// scene. We only move bytes — all protocol + rendering work lives in Rust.

import init, { start, push_bytes, init_sequence, set_flip } from "./pkg/dgtboard_bevy.js";

const $ = (id) => document.getElementById(id);
let port, reader, writer, keepReading = false, started = false;

async function main() {
  if (!("serial" in navigator)) {
    setStatus("Web Serial isn't available in this browser — use Chrome, Edge, or Opera.", true);
    $("connect").disabled = true;
  }
  $("connect").addEventListener("click", connect);
  $("disconnect").addEventListener("click", disconnect);
  $("flip").addEventListener("change", onFlipToggle);
  // Boot the scene right away so the start position (and the scanned set, as
  // it streams in) is on screen before any board is connected.
  await bootScene();
}

// Boot the wasm + Bevy app exactly once; it renders into <canvas id="scene">
// and runs its own loop forever. The byte queue is shared with `push_bytes`.
async function bootScene() {
  if (started) return;
  await init();
  started = true;
  try {
    start($("flip").checked);
  } catch (e) {
    // Bevy/winit on wasm hands control to the browser's event loop by
    // throwing a sentinel ("Using exceptions for control flow"); anything
    // else is a real error.
    if (!String(e).includes("control flow")) {
      setStatus("3D scene failed to start: " + e, true);
      throw e;
    }
  }
}

async function connect() {
  try {
    port = await navigator.serial.requestPort();
    await port.open({ baudRate: 9600, dataBits: 8, parity: "none", stopBits: 1, flowControl: "none" });
  } catch (e) {
    setStatus("Connection cancelled or failed: " + e.message, true);
    return;
  }

  await bootScene();

  writer = port.writable.getWriter();
  reader = port.readable.getReader();

  // reset → request full board dump (seeds position) → stream field updates
  await writer.write(init_sequence());

  setConnected(true);
  setStatus("Connected. Play a game — pieces move on the 3D board in real time.");
  keepReading = true;
  readLoop();
}

async function readLoop() {
  try {
    while (keepReading) {
      const { value, done } = await reader.read();
      if (done) break;
      if (value && value.length) push_bytes(value); // value is a Uint8Array
    }
  } catch (e) {
    if (keepReading) setStatus("Read error: " + e.message, true);
  }
}

// Flip live: tell the wasm to reseed in the new orientation, then re-request a
// full dump so the position re-streams immediately.
async function onFlipToggle() {
  if (!started) return; // scene not booted yet — the setting applies on boot
  set_flip($("flip").checked);
  if (keepReading && writer) {
    try {
      await writer.write(init_sequence());
    } catch (e) {
      setStatus("Flip failed: " + e.message, true);
    }
  }
}

async function disconnect() {
  keepReading = false;
  try { await reader?.cancel(); } catch {}
  try { reader?.releaseLock(); } catch {}
  try { writer?.releaseLock(); } catch {}
  try { await port?.close(); } catch {}
  reader = writer = port = undefined;
  setConnected(false);
  setStatus("Disconnected. The 3D scene keeps the last position.");
}

function setConnected(on) {
  $("connect").disabled = on;
  $("disconnect").disabled = !on;
}

function setStatus(text, isError = false) {
  const el = $("status");
  el.textContent = text;
  el.className = isError ? "error" : "";
}

main();
