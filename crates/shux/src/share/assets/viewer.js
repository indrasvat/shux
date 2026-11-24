// The shux share viewer: shux.view.v1 into ghostty-web. It sends the server
// its size, resize requests and acknowledgements, and nothing else.
import { Ghostty, Terminal } from "/_/ghostty-web.js";

const FINAL = {
  revoked: "The owner ended this link.",
  expired: "This link has expired.",
  stopped: "The owner stopped sharing.",
  session_ended: "The session ended.",
  kicked: "The owner removed you from this session.",
  replaced: "This session is open in another tab.",
  protocol: "The viewer lost its place in the stream.",
};
const MAX_RETRIES = 5;
const FONT = { family: 'Menlo, "DejaVu Sans Mono", Consolas, monospace', size: 14 };

const id = location.pathname.split("/")[2] || "";
// The secret lives in memory only, and leaves the address bar at once.
let secret = location.hash.length > 1 ? location.hash.slice(1) : null;
if (location.hash) history.replaceState(null, "", location.pathname);

const $ = (sel) => document.querySelector(sel);
const view = {
  phase: "idle",
  received: 0,
  connects: 0,
  size: null,
  ended: null,
  // A renderer that stops consuming, for tests of the server's cutoff.
  stall: false,
  text: () => screenText(),
};
window.__shux = view;

let ghostty = null;
let term = null;
let ws = null;
let max = { cols: 240, rows: 100 };
let expiresAt = null;
let retries = 0;

function setState(text, bad = false) {
  const el = $("#state");
  el.textContent = text;
  el.classList.toggle("bad", bad);
}

function showExpiry() {
  if (!expiresAt) return;
  const at = expiresAt.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  $("#expiry").textContent = `· expires at ${at}`;
}

function end(message) {
  view.phase = "ended";
  setState("ended", true);
  $("#gate-text").textContent = message;
  $("#watch").hidden = true;
  $("#gate").hidden = false;
  $("#frame").hidden = true;
}

function screenText() {
  if (!term) return "";
  const buf = term.buffer.active;
  const lines = [];
  for (let y = 0; y < term.rows; y++) lines.push(buf.getLine(y)?.translateToString(true) ?? "");
  return lines.join("\n");
}

function cellSize() {
  const ctx = document.createElement("canvas").getContext("2d");
  ctx.font = `${FONT.size}px ${FONT.family}`;
  return { w: ctx.measureText("M").width || 8.4, h: Math.ceil(FONT.size * 1.2) };
}

function fitted() {
  const stage = $("#stage");
  const cell = cellSize();
  const cols = Math.floor(stage.clientWidth / cell.w);
  const rows = Math.floor(stage.clientHeight / cell.h);
  return [Math.max(2, Math.min(max.cols, cols)), Math.max(2, Math.min(max.rows, rows))];
}

// A grid wider than the screen is shown whole, scaled: an overview the
// browser's own zoom and scroll can close in on.
function layout() {
  const frame = $("#frame");
  frame.style.transform = "";
  const scale = Math.min(1, $("#stage").clientWidth / Math.max(1, frame.scrollWidth));
  frame.style.transform = scale < 1 ? `scale(${scale})` : "";
}

function applyPalette(p) {
  const root = document.documentElement.style;
  root.setProperty("--bg", p.background);
  root.setProperty("--fg", p.foreground);
  root.setProperty("--bar", p.bar);
  root.setProperty("--bar-text", p.bar_text);
  root.setProperty("--muted", p.muted);
  root.setProperty("--shared", p.shared);
}

function freshTerminal(size, palette) {
  term?.dispose();
  $("#term").replaceChildren();
  term = new Terminal({
    ghostty,
    cols: size.cols,
    rows: size.rows,
    disableStdin: true,
    cursorBlink: false,
    scrollback: 0,
    fontFamily: FONT.family,
    fontSize: FONT.size,
    theme: { background: palette.background, foreground: palette.foreground },
  });
  term.open($("#term"));
  view.size = [size.cols, size.rows];
  layout();
}

function send(message) {
  if (ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify(message));
}

function requestSize(host) {
  send({ t: "resize", cols: Math.min(host.cols, max.cols), rows: Math.min(host.rows, max.rows) });
}

function onText(m) {
  switch (m.t) {
    case "ready":
      max = m.max;
      // Counted down from the server's remaining time: this clock may be off.
      expiresAt = new Date(Date.now() + m.expires_in_ms);
      applyPalette(m.palette);
      $("#session").textContent = m.session;
      showExpiry();
      freshTerminal(m.size, m.palette);
      view.received = 0;
      view.phase = "live";
      retries = 0;
      setState("live");
      // Draw at the host's own layout when it fits, so no pane is clipped.
      if (m.host && (m.host.cols !== m.size.cols || m.host.rows !== m.size.rows)) requestSize(m.host);
      break;
    case "resize":
      term.resize(m.cols, m.rows);
      view.size = [m.cols, m.rows];
      layout();
      break;
    case "host":
      requestSize(m);
      break;
    case "ended":
      view.ended = m.reason;
      break;
  }
}

function onBytes(data) {
  if (view.stall) return;
  const offset = Number(new DataView(data).getBigUint64(0));
  if (offset !== view.received) {
    // A gap or a repeat: only a fresh stream draws correctly from here.
    ws.close();
    return;
  }
  const bytes = new Uint8Array(data, 8);
  term.write(bytes);
  view.received += bytes.length;
  send({ t: "ack", offset: view.received });
}

async function status() {
  try {
    const r = await fetch(`/v/${id}/status`, { cache: "no-store", credentials: "same-origin" });
    if (!r.ok) return false;
    expiresAt = new Date((await r.json()).expires_at);
    return true;
  } catch {
    return null;
  }
}

async function connect() {
  ghostty ??= await Ghostty.load("/_/ghostty-vt.wasm");
  view.connects += 1;
  view.ended = null;
  view.phase = "connecting";
  setState(view.connects > 1 ? "reconnecting…" : "connecting…");
  $("#gate").hidden = true;
  $("#frame").hidden = false;
  const scheme = location.protocol === "https:" ? "wss" : "ws";
  const socket = new WebSocket(`${scheme}://${location.host}/v/${id}/ws`);
  ws = socket;
  socket.binaryType = "arraybuffer";
  socket.onopen = () => {
    const [cols, rows] = fitted();
    socket.send(JSON.stringify({ t: "hello", cols, rows }));
  };
  socket.onmessage = (e) => (typeof e.data === "string" ? onText(JSON.parse(e.data)) : onBytes(e.data));
  socket.onclose = (e) => {
    if (ws === socket) ws = null;
    reconnectOrEnd(view.ended || e.reason);
  };
}

async function reconnectOrEnd(reason) {
  if (FINAL[reason]) return end(FINAL[reason]);
  if (expiresAt && expiresAt <= new Date()) return end(FINAL.expired);
  if (retries >= MAX_RETRIES) return end("The connection to the session was lost.");
  retries += 1;
  view.phase = "waiting";
  setState("reconnecting…", true);
  await new Promise((ok) => setTimeout(ok, 250 * 2 ** (retries - 1)));
  // The socket cannot say why it failed; the status can.
  const ok = await status();
  if (ok === false) return end("This link has ended.");
  connect();
}

async function redeem() {
  $("#watch").disabled = true;
  const r = await fetch(`/v/${id}/redeem`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ secret }),
    credentials: "same-origin",
    cache: "no-store",
  });
  secret = null;
  if (r.ok) {
    expiresAt = new Date((await r.json()).expires_at);
    connect();
  } else {
    end(r.status === 410 ? "This link has already been used, or has ended." : "This link is not valid.");
  }
}

async function boot() {
  window.addEventListener("resize", layout);
  if (secret) {
    setState("ready");
    $("#watch").addEventListener("click", redeem, { once: true });
    $("#frame").hidden = true;
    $("#gate").hidden = false;
    return;
  }
  // A reload: this browser's credential, if it still has one.
  if (await status()) connect();
  else end("This link has ended, or was opened in another browser.");
}

boot();
