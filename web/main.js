// Web Serial + WebAssembly demo: read a live DGT board in the browser.
//
// The Rust core (compiled to wasm) does all the protocol work AND referees the
// game (legal-move checking, illegal-move alerts, check/checkmate — see
// wasm/src/lib.rs). This file only moves bytes between the serial port and the
// wasm `DgtSession`, and renders.

import init, { DgtSession, initSequence, version } from "./pkg/dgtboard_wasm.js";

// Solid glyphs for both colours (hollow "white" glyphs are invisible on light
// squares); colour is applied via a CSS class instead.
const GLYPH = { k: "♚", q: "♛", r: "♜", b: "♝", n: "♞", p: "♟" };
function pieceFor(ch) {
  if (!ch) return ["", ""];
  const lower = ch.toLowerCase();
  return [GLYPH[lower] || "", ch === lower ? "black" : "white"];
}

const START_FEN = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR";
const TAB = "\t";
const NL = "\n";
const $ = (id) => document.getElementById(id);
let port, reader, writer, session, keepReading = false, gameOver = false;

async function main() {
  await init();
  $("version").textContent = "core v" + version();
  buildEmptyBoard();
  renderBoard(START_FEN); // preview position until a board is connected
  if (!("serial" in navigator)) {
    setStatus("Web Serial isn't available in this browser — use Chrome, Edge, or Opera.", true);
    $("connect").disabled = true;
  }
  $("connect").addEventListener("click", connect);
  $("disconnect").addEventListener("click", disconnect);
  $("flip").addEventListener("change", onFlipToggle);
}

// Flipping live: rebuild the session in the new orientation and re-request a
// full board dump so the position re-seeds immediately. When disconnected, the
// new setting just applies on the next connect.
async function onFlipToggle() {
  if (!keepReading || !writer) return;
  session = new DgtSession($("flip").checked);
  resetGame();
  buildEmptyBoard();
  try {
    await writer.write(initSequence());
  } catch (e) {
    setStatus("Flip failed: " + e.message, true);
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

  session = new DgtSession($("flip").checked);
  resetGame();

  writer = port.writable.getWriter();
  reader = port.readable.getReader();

  // reset -> request full board (seeds position) -> stream field updates
  await writer.write(initSequence());

  setConnected(true);
  setStatus("Connected. Play a game on the board.");
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
  highlightCheck(session.checkedSquare());

  const events = session.takeEvents();
  if (events) {
    for (const line of events.split(NL)) {
      if (!line) continue;
      const parts = line.split(TAB);
      if (parts[0] === "move") {
        addMove(parts[1], parts[2], parts[3]); // ply, colour, SAN
        if (!gameOver) clearBanner();
      } else if (parts[0] === "illegal" && !gameOver) {
        showIllegal(parts[1]);
      } else if (parts[0] === "sync") {
        clearBanner();
      }
    }
  }
  updateStatus(session.status());
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
        setPiece(cells[i++], ch);
      }
    }
  }
}

function setPiece(cell, ch) {
  const span = cell.querySelector(".piece");
  if (span.dataset.ch === ch) return;
  const [glyph, color] = pieceFor(ch);
  span.textContent = glyph;
  span.className = "piece " + color;
  span.dataset.ch = ch;
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

function addMove(ply, color, san) {
  const num = Math.ceil(ply / 2);
  const marker = color === "White" ? `${num}.` : `${num}…`;
  const li = document.createElement("li");
  li.innerHTML = `<span class="n">${marker}</span><span class="san">${san}</span>`;
  const list = $("moves");
  list.appendChild(li);
  list.scrollTop = list.scrollHeight;
}

function highlightCheck(idx) {
  const cells = $("board").children;
  for (const c of cells) c.classList.remove("check");
  if (idx >= 0 && cells[idx]) cells[idx].classList.add("check");
}

function banner(text, type) {
  const el = $("banner");
  el.textContent = text;
  el.className = "banner show " + type;
}

function clearBanner() {
  const el = $("banner");
  // Keep a game-over banner sticky; only clear transient (illegal) ones.
  if (el.classList.contains("win") || el.classList.contains("draw")) return;
  el.className = "banner";
  el.textContent = "";
}

function showIllegal(uci) {
  banner(`Illegal move: ${uci} — put the piece back`, "illegal");
}

function updateStatus(word) {
  const turn = $("turn");
  if (word.startsWith("checkmate:")) {
    gameOver = true;
    banner(`♚ Checkmate — ${word.split(":")[1]} wins`, "win");
    turn.textContent = "Checkmate";
  } else if (word === "stalemate") {
    gameOver = true;
    banner("½–½ Stalemate — draw", "draw");
    turn.textContent = "Stalemate";
  } else if (word === "draw") {
    gameOver = true;
    banner("½–½ Draw — insufficient material", "draw");
    turn.textContent = "Draw";
  } else if (word === "check") {
    turn.textContent = "Check!";
  } else {
    turn.textContent = "";
  }
}

function resetGame() {
  gameOver = false;
  $("moves").innerHTML = "";
  $("banner").className = "banner";
  $("banner").textContent = "";
  highlightCheck(-1);
}

function setConnected(on) {
  $("connect").disabled = on;
  $("disconnect").disabled = !on;
  // flip stays enabled so you can re-orient the board live
}

function setStatus(text, isError = false) {
  const el = $("status");
  el.textContent = text;
  el.className = isError ? "error" : "";
}

main();
