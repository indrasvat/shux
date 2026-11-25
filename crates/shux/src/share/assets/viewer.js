// The shux share viewer: shux.view.v1 into ghostty-web. It sends the server
// its size, resize requests and acknowledgements, and nothing else.
import { Ghostty, Terminal } from "/_/ghostty-web.js";

const FINAL = {
  revoked: "The owner ended this link.",
  expired: "This link has expired.",
  stopped: "The owner stopped sharing.",
  session_ended: "The session ended.",
  tunnel_lost: "The owner's connection dropped. Ask them for a new link.",
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
// The same moment on this page's monotonic clock, which a changed system
// clock cannot move.
let deadline = null;
// Why the link ended, as the status probe said.
let endedWhy = null;
// The view the guest picked: "fit" or "readable"; null until they do.
let zoom = null;
const READABLE_PX = 9.5;
let retries = 0;

// Counted down from the server's remaining time: this clock may be off.
function setExpiry(remainingMs) {
  expiresAt = new Date(Date.now() + remainingMs);
  deadline = performance.now() + remainingMs;
}

function setState(text, bad = false) {
  const el = $("#state");
  el.textContent = text;
  el.classList.toggle("bad", bad);
}

// "23h 51m", "1d 2h": counted up to the minute as a countdown is, and said
// as the CLI says a span.
function timeLeft(ms) {
  const mins = Math.max(1, Math.ceil(ms / 60000));
  const d = Math.floor(mins / 1440);
  const h = Math.floor((mins % 1440) / 60);
  const m = mins % 60;
  if (d) return h ? `${d}d ${h}h` : `${d}d`;
  if (h) return m ? `${h}h ${m}m` : `${h}h`;
  return `${m}m`;
}

// In the guest's own time and zone, as they would say it: "3:55 PM PST",
// "tomorrow 3:55 PM PST", "Fri, Nov 27, 3:55 PM PST".
function when(d) {
  const t = d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit", timeZoneName: "short" });
  const now = new Date();
  if (d.toDateString() === now.toDateString()) return t;
  const tomorrow = new Date(now);
  tomorrow.setDate(now.getDate() + 1);
  if (d.toDateString() === tomorrow.toDateString()) return `tomorrow ${t}`;
  return `${d.toLocaleDateString([], { weekday: "short", month: "short", day: "numeric" })}, ${t}`;
}

// Uses left, as the first page last heard them.
let gateUses = null;

// The first page counts down to its end; the bar keeps the time left on a
// phone, where the end time gives way first.
function showExpiry() {
  if (deadline === null) return;
  const ms = deadline - performance.now();
  if (view.phase === "idle") {
    if (ms <= 0) return end(FINAL.expired);
    let text = `This link works for ${timeLeft(ms)} more, until ${when(expiresAt)}`;
    // Non-breaking: "1 use left" never wraps in the middle.
    if (gateUses != null) text += ` · ${gateUses} ${gateUses === 1 ? "use" : "uses"} left`;
    $("#gate-when").textContent = text;
    $("#gate-when").hidden = false;
    return;
  }
  if (view.phase !== "live") return;
  $("#left").textContent = ms > 0 ? `· ${timeLeft(ms)} left` : "";
  $("#expiry").textContent = ms > 0 ? `· until ${when(expiresAt)}` : "";
}
setInterval(showExpiry, 15000);
// A sleeping phone stops this page's clocks: ask the server again on waking.
document.addEventListener("visibilitychange", async () => {
  if (document.hidden || deadline === null || view.phase === "ended") return;
  if (view.phase === "idle") {
    if (secret) await peek();
  } else await status();
  showExpiry();
});

// Once a page: typing reaches nobody, and the guest should know.
let nudged = false;
function nudge(e) {
  if (nudged || view.phase !== "live") return;
  if (e.type === "keydown" && ["Shift", "Control", "Alt", "Meta"].includes(e.key)) return;
  nudged = true;
  const toast = $("#toast");
  toast.hidden = false;
  setTimeout(() => (toast.hidden = true), 4000);
}

// What the gate says before a use is spent: the secret proves the asker
// holds the link, and nothing is redeemed.
async function peek() {
  try {
    const r = await fetch(`/v/${id}/redeem`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ secret, peek: true }),
      credentials: "same-origin",
      cache: "no-store",
    });
    // Ended while this page slept: the server says so, whatever its clock says.
    if (r.status === 410) {
      const why = (await r.json().catch(() => ({}))).reason;
      return end(FINAL[why] ?? "This link has already been used.");
    }
    if (!r.ok) return;
    const { expires_in_ms: ms, uses_left: left } = await r.json();
    gateUses = left;
    setExpiry(ms);
    showExpiry();
  } catch {
    // The gate reads as well without it.
  }
}

function end(message) {
  view.phase = "ended";
  setState("ended", true);
  $("#gate-about").hidden = true;
  $("#gate-text").textContent = message;
  $("#gate-text").hidden = false;
  $("#watch").hidden = true;
  $("#gate").hidden = false;
  $("#sizer").hidden = true;
  // Nothing to fit and nobody to count once it has ended.
  $("#zoom").hidden = true;
  $("#watching").textContent = "";
  $("#left").textContent = "";
  $("#expiry").textContent = "";
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
// Two views of a grid wider than the screen: "fit", the whole layout
// scaled down, and "readable", glyphs no smaller than READABLE_PX, panned by
// scrolling the stage. Readable is the default once fit would go below it.
// Both are local: nothing here reaches the session, and no size is asked for.
function layout() {
  const frame = $("#frame");
  const sizer = $("#sizer");
  const stage = $("#stage");
  frame.style.transform = "";
  const w = frame.scrollWidth;
  const h = frame.scrollHeight;
  const fit = Math.min(1, stage.clientWidth / Math.max(1, w));
  const readable = Math.min(1, Math.max(fit, READABLE_PX / FONT.size));
  const mode = zoom ?? (fit < readable ? "readable" : "fit");
  const scale = mode === "readable" ? readable : fit;
  frame.style.transform = scale < 1 ? `scale(${scale})` : "";
  sizer.style.width = `${Math.ceil(w * scale)}px`;
  sizer.style.height = `${Math.ceil(h * scale)}px`;
  const button = $("#zoom");
  button.hidden = readable === fit || view.phase === "ended";
  button.textContent = mode === "readable" ? "Fit" : "Zoom";
  view.mode = mode;
  view.glyph = FONT.size * scale;
}

function toggleZoom() {
  zoom = view.mode === "readable" ? "fit" : "readable";
  layout();
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
  // Read-only: a tap must not raise a phone keyboard over the view.
  for (const el of [$("#term"), ...$("#term").querySelectorAll("textarea")]) el.inputMode = "none";
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
      setExpiry(m.expires_in_ms);
      applyPalette(m.palette);
      $("#session").textContent = m.session;
      $("#bar .sep").hidden = false;
      freshTerminal(m.size, m.palette);
      view.received = 0;
      view.phase = "live";
      showExpiry();
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
    case "watching":
      $("#watching").textContent = `· ${m.n} watching`;
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
    // Only the gateway's own answer says the link ended; anything else (a
    // tunnel's 530) is the server out of reach.
    if (!r.ok) return null;
    const j = await r.json();
    if (j.state !== "ok") {
      endedWhy = j.reason;
      return false;
    }
    setExpiry(j.expires_in_ms);
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
  $("#sizer").hidden = false;
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
  if (deadline !== null && deadline <= performance.now()) return end(FINAL.expired);
  if (retries >= MAX_RETRIES) return end("The connection to the session was lost.");
  retries += 1;
  view.phase = "waiting";
  setState("reconnecting…", true);
  await new Promise((ok) => setTimeout(ok, 250 * 2 ** (retries - 1)));
  // The socket cannot say why it failed; the status can. An unreachable
  // server is one more retry, not an ending.
  const ok = await status();
  if (ok === false) return end(FINAL[endedWhy] ?? "This link has ended.");
  if (ok === null) return reconnectOrEnd("");
  connect();
}

// The button comes back, the secret kept, after a failure that spent no use.
function retryWatch(message) {
  setState(message, true);
  $("#watch").disabled = false;
  $("#watch").addEventListener("click", redeem, { once: true });
}

async function redeem() {
  $("#watch").disabled = true;
  let r;
  try {
    r = await fetch(`/v/${id}/redeem`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ secret }),
      credentials: "same-origin",
      cache: "no-store",
    });
  } catch {
    return retryWatch("could not reach the session; try again");
  }
  if (r.status === 429 || r.status === 503) return retryWatch("busy; try again shortly");
  secret = null;
  if (r.ok) {
    setExpiry((await r.json()).expires_in_ms);
    connect();
  } else {
    const why = r.status === 410 ? (await r.json().catch(() => ({}))).reason : null;
    if (r.status !== 410) end("This link is not valid.");
    else end(FINAL[why] ?? "This link has already been used.");
  }
}

async function boot() {
  window.addEventListener("resize", layout);
  $("#zoom").addEventListener("click", toggleZoom);
  for (const type of ["keydown", "paste"]) window.addEventListener(type, nudge, { capture: true });
  $("#term").addEventListener("pointerup", nudge);
  // The terminal's own wheel handler would swallow the pan; a read-only view has nothing to scroll in it.
  $("#stage").addEventListener("wheel", (e) => e.stopPropagation(), { capture: true, passive: true });
  if (secret) {
    setState("ready");
    $("#watch").addEventListener("click", redeem, { once: true });
    $("#sizer").hidden = true;
    $("#gate").hidden = false;
    peek();
    return;
  }
  // A reload: this browser's credential, if it still has one. An
  // unreachable server is retried like any lost connection.
  const ok = await status();
  if (ok) connect();
  else if (ok === null) reconnectOrEnd("");
  else end(FINAL[endedWhy] ?? "This link has ended, or was opened in another browser.");
}

boot();
