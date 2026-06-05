// Web Serial + WebAssembly demo: read a live DGT board in the browser.
//
// The Rust core (compiled to wasm) does all the protocol work; this file only
// moves bytes between the serial port and the wasm `DgtSession`, and renders.

import init, { DgtSession, initSequence, version } from "./pkg/dgtboard_wasm.js";

const PIECES = {
  K: "♔", Q: "♕", R: "♖", B: "♗", N: "♘", P: "♙",
  k: "♚", q: "♛", r: "♜", b: "♝", n: "♞", p: "♟",
};

const $ = (id) => document.getElementById(id);
let port, reader, writer, session, keepReading = false, ply = 0;

async function main() {
  await init();
  $("version").textContent = "core v" + version();
  buildEmptyBoard();
  if (!("serial" in navigator)) {
    setStatus("Web Serial isn't available in this browser — use Chrome, Edge, or Opera.", true);
    $("connect").disabled = true;
  }
  $("connect").addEventListener("click", connect);
  $("disconnect").addEventListener("click", disconnect);
}

async function connect() {
  try {
    port = await navigator.serial.requestPort();
    await port.open({ baudRate: 9600, dataBits: 8, parity: "none", stopBits: 1, flowControl: "none" });
  } catch (e) {
    setStatus("Connection cancelled or failed: " + e.message, true);
    return;
  }

  session = new DgtSession($("flip").checked);
  ply = 0;
  $("moves").innerHTML = "";

  writer = port.writable.getWriter();
  reader = port.readable.getReader();

  // reset -> request full board (seeds position) -> stream field updates
  await writer.write(initSequence());

  setConnected(true);
  setStatus("Connected. Move pieces on the board.");
  keepReading = true;
  readLoop();
}

async function readLoop() {
  try {
    while (keepReading) {
      const { value, done } = await reader.read();
      if (done) break;
      if (value && value.length) {
        session.push(value); // value is a Uint8Array
        render();
      }
    }
  } catch (e) {
    if (keepReading) setStatus("Read error: " + e.message, true);
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
  setStatus("Disconnected.");
}

function render() {
  const fen = session.fen();
  renderBoard(fen);
  $("fen").textContent = fen;
  const side = session.sideToMove();
  $("turn").textContent = side ? side + " to move" : " ";

  const moves = session.takeMoves();
  if (moves) {
    for (const line of moves.split("\n")) {
      if (!line) continue;
      const [color, uci, desc] = line.split("\t");
      addMove(color, uci, desc);
    }
  }
}

function renderBoard(fen) {
  const ranks = fen.split("/");
  const cells = $("board").children;
  let i = 0;
  for (let r = 0; r < 8; r++) {
    for (const ch of ranks[r]) {
      if (ch >= "1" && ch <= "8") {
        for (let k = 0; k < +ch; k++) setPiece(cells[i++], "");
      } else {
        setPiece(cells[i++], PIECES[ch] || "");
      }
    }
  }
}

function setPiece(cell, glyph) {
  const span = cell.querySelector(".piece");
  if (span.textContent !== glyph) span.textContent = glyph;
}

function buildEmptyBoard() {
  const board = $("board");
  board.innerHTML = "";
  const files = "abcdefgh";
  for (let idx = 0; idx < 64; idx++) {
    const file = idx % 8;
    const rank = 8 - Math.floor(idx / 8); // index 0 = a8
    const cell = document.createElement("div");
    cell.className = "sq " + ((file + rank) % 2 === 0 ? "light" : "dark");
    const piece = document.createElement("span");
    piece.className = "piece";
    cell.appendChild(piece);
    if (file === 0 || rank === 1) {
      const coord = document.createElement("span");
      coord.className = "coord";
      coord.textContent = file === 0 ? rank : (rank === 1 ? files[file] : "");
      cell.appendChild(coord);
    }
    board.appendChild(cell);
  }
}

function addMove(color, uci, desc) {
  ply++;
  const li = document.createElement("li");
  li.innerHTML =
    `<span class="n">${ply}.</span><span class="uci">${uci}</span><span class="desc">${color} ${desc}</span>`;
  const list = $("moves");
  list.appendChild(li);
  list.scrollTop = list.scrollHeight;
}

function setConnected(on) {
  $("connect").disabled = on;
  $("disconnect").disabled = !on;
  $("flip").disabled = on;
}

function setStatus(text, isError = false) {
  const el = $("status");
  el.textContent = text;
  el.className = isError ? "error" : "";
}

main();
