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
let port, reader, writer, session, keepReading = false, gameOver = false, startChecked = false;

// --- Dutch voice coach (Web Speech API) -----------------------------------
// Speaks a friendly, child-friendly Dutch explanation when an illegal move is
// played, and a little cheer when the board is fixed.
const PIECE_NL = {
  p: ["de", "pion"], n: ["het", "paard"], b: ["de", "loper"],
  r: ["de", "toren"], q: ["de", "dame"], k: ["de", "koning"],
};
let dutchVoice = null;
function pickDutchVoice() {
  if (!("speechSynthesis" in window)) return;
  const voices = speechSynthesis.getVoices();
  dutchVoice = voices.find((v) => v.lang && v.lang.toLowerCase().startsWith("nl")) || null;
}
function speakDutch(text) {
  if (!$("voice").checked || !("speechSynthesis" in window)) return;
  speechSynthesis.cancel(); // don't let messages pile up
  const u = new SpeechSynthesisUtterance(text);
  u.lang = "nl-NL";
  if (dutchVoice) u.voice = dutchVoice;
  u.rate = 0.95;
  u.pitch = 1.05;
  speechSynthesis.speak(u);
}
function illegalDutch(reason, pieceLetter) {
  const [art, name] = PIECE_NL[pieceLetter] || ["het", "stuk"];
  switch (reason) {
    case "turn":
      return "Wacht even! Het is nog niet jouw beurt. De andere speler is aan zet.";
    case "nopiece":
      return "Hmm, op dat vakje stond geen stuk. Zet de stukken even terug zoals ze net stonden.";
    case "check":
      return "Pas op! Jouw koning staat schaak. Je moet eerst je koning in veiligheid brengen.";
    case "own":
      return "Daar staat al een eigen stuk. Op je eigen stuk mag je niet gaan staan. Kies een ander vakje.";
    default:
      return `Oeps! Zo mag ${art} ${name} niet lopen. Zet hem maar terug en probeer een andere zet.`;
  }
}

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

  pickDutchVoice();
  if ("speechSynthesis" in window) speechSynthesis.onvoiceschanged = pickDutchVoice;
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

  // Once the first real position arrives, check it's the start position — if
  // not (usually a wrong Flip), the referee can't begin, so say so loudly.
  if (!startChecked && fen !== "8/8/8/8/8/8/8/8") {
    startChecked = true;
    if (session.isStartPosition()) {
      setStatus("Referee ready — play a game.");
    } else {
      setStatus("⚠ Referee needs the standard start position — toggle “Flip 180°”, or set the pieces up and reconnect.", true);
    }
  }

  const events = session.takeEvents();
  if (events) {
    for (const line of events.split(NL)) {
      if (!line) continue;
      const parts = line.split(TAB);
      if (parts[0] === "move") {
        // move = ply, colour, SAN, status, uci
        addMove(parts[1], parts[2], parts[3]);
        const mate = parts[4].startsWith("checkmate");
        markSquares(parts[5], mate ? "matemove" : "lastmove");
        clearSquares(mate ? "lastmove" : "matemove");
        clearSquares("illegalsq");
        if (!gameOver) clearBanner();
      } else if (parts[0] === "illegal" && !gameOver) {
        // illegal = uci, reason, pieceLetter
        showIllegal(parts[1]);
        markSquares(parts[1], "illegalsq");
        speakDutch(illegalDutch(parts[2], parts[3]));
      } else if (parts[0] === "sync") {
        clearBanner();
        clearSquares("illegalsq");
        speakDutch("Goed gedaan! Nu klopt het bord weer. Speel maar verder.");
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

// Algebraic square ("e4") -> board cell index (0 = a8, matching renderBoard).
function squareIndex(sq) {
  const file = sq.charCodeAt(0) - 97; // 'a'
  const rank = sq.charCodeAt(1) - 48; // '1'
  if (file < 0 || file > 7 || rank < 1 || rank > 8) return -1;
  return file + 8 * (8 - rank);
}

function clearSquares(cls) {
  for (const c of $("board").children) c.classList.remove(cls);
}

// Highlight the from/to squares of a UCI move (e.g. "e2e4", "e7e8q").
function markSquares(uci, cls) {
  clearSquares(cls);
  const cells = $("board").children;
  for (const sq of [uci.slice(0, 2), uci.slice(2, 4)]) {
    const idx = squareIndex(sq);
    if (idx >= 0 && cells[idx]) cells[idx].classList.add(cls);
  }
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
    turn.className = "";
    turn.textContent = "Checkmate";
    return;
  }
  if (word === "stalemate") {
    gameOver = true;
    banner("½–½ Stalemate — draw", "draw");
    turn.className = "";
    turn.textContent = "Stalemate";
    return;
  }
  if (word === "draw") {
    gameOver = true;
    banner("½–½ Draw — insufficient material", "draw");
    turn.className = "";
    turn.textContent = "Draw";
    return;
  }
  // Persistent out-of-sync indicator so an illegal move stays visible until fixed.
  if (!session.inSync()) {
    turn.className = "outofsync";
    turn.textContent = "⚠ Illegal move — board out of sync. Restore the last legal position.";
    return;
  }
  turn.className = "";
  turn.textContent = (word === "check" ? "Check! " : "") + session.sideToMove() + " to move";
}

function resetGame() {
  gameOver = false;
  startChecked = false;
  $("moves").innerHTML = "";
  $("banner").className = "banner";
  $("banner").textContent = "";
  $("turn").className = "";
  $("turn").textContent = "";
  highlightCheck(-1);
  clearSquares("lastmove");
  clearSquares("matemove");
  clearSquares("illegalsq");
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
