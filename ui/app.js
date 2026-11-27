// Sylva Chat: Twitch chat for the signed-in streamer's channel.
//
// Chat comes straight from Twitch IRC over a websocket (wss://irc-ws.chat.twitch.tv). Sending,
// deleting, timeouts and bans go through Helix via Rust, which holds the token. Emotes come from
// Twitch (message tags), 7TV, BTTV and FFZ; badges from Helix; the last messages from
// recent-messages.robotty.de so chat is not empty on open.
//
// Busy chats: messages are built off screen and added once per frame, the list keeps at most
// MAX_LINES rows, rows skip layout when off screen (content-visibility), and the mod buttons are
// one shared strip that moves to the row under the pointer instead of living on every row.
"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const appWindow = window.__TAURI__.window.getCurrentWindow();
const $ = (id) => document.getElementById(id);

const MAX_LINES = 600;
const HISTORY = 150;
const IRC_URL = "wss://irc-ws.chat.twitch.tv:443";
const BTTV_ZERO_WIDTH = new Set(["SoSnowy", "IceCold", "SantaHat", "TopHat", "ReinDeer", "CandyCane", "cvMask", "cvHazmat"]);
const DEFAULT_COLORS = ["#ff4a4a", "#5b8cff", "#3fcf5e", "#e05a3a", "#ff7f50", "#9acd32", "#ff6a3d", "#2e8b57", "#daa520", "#d2691e", "#5f9ea0", "#1e90ff", "#ff69b4", "#a879ff", "#00ff7f"];

let me = null; // { login, userId }
let channel = null; // { login, id }
const emotes = new Map(); // code -> { src, srcset, w, h, zw, by }
const badges = new Map(); // "set/version" -> { src, srcset, title }
const users = new Map(); // login -> { id, name, color, rows: [] }
const rowsById = new Map(); // message id -> row element
const recentNames = []; // display names, most recent last, for Tab completion

// ---------- Small helpers ----------
function el(tag, cls, text) {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text != null) n.textContent = text;
  return n;
}
function icon(id) {
  const ns = "http://www.w3.org/2000/svg";
  const s = document.createElementNS(ns, "svg");
  const u = document.createElementNS(ns, "use");
  u.setAttribute("href", `#i-${id}`);
  s.appendChild(u);
  return s;
}
function toast(text, bad = false) {
  const t = el("div", `toast${bad ? " bad" : ""}`, text);
  $("toasts").appendChild(t);
  setTimeout(() => { t.classList.add("out"); setTimeout(() => t.remove(), 200); }, bad ? 5000 : 2600);
}
const fetchJson = (url) => invoke("fetch_json", { url });
const helix = (method, path, query = [], body = null) => invoke("helix", { method, path, query, body });
function errText(e) { return String(e?.message || e).replace(/^Error:\s*/, ""); }
function duration(s) {
  s = Number(s);
  if (s < 60) return `${s} second${s === 1 ? "" : "s"}`;
  if (s < 3600) { const m = Math.round(s / 60); return `${m} minute${m === 1 ? "" : "s"}`; }
  if (s < 86400) { const h = Math.round(s / 3600); return `${h} hour${h === 1 ? "" : "s"}`; }
  const d = Math.round(s / 86400); return `${d} day${d === 1 ? "" : "s"}`;
}
function clock(ms) {
  const d = ms ? new Date(Number(ms)) : new Date();
  return `${d.getHours()}:${String(d.getMinutes()).padStart(2, "0")}`;
}

// Twitch lets people pick very dark name colors; lift those so they read on near black.
const colorCache = new Map();
function nameColor(hex, login) {
  if (!hex) hex = DEFAULT_COLORS[[...login].reduce((a, c) => (a * 31 + c.charCodeAt(0)) >>> 0, 7) % DEFAULT_COLORS.length];
  let c = colorCache.get(hex);
  if (c) return c;
  const n = parseInt(hex.slice(1), 16);
  let r = (n >> 16) / 255, g = ((n >> 8) & 255) / 255, b = (n & 255) / 255;
  const max = Math.max(r, g, b), min = Math.min(r, g, b);
  let h = 0, s = 0, l = (max + min) / 2;
  if (max !== min) {
    const d = max - min;
    s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
    h = max === r ? (g - b) / d + (g < b ? 6 : 0) : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
    h /= 6;
  }
  // Blue reads darker than its lightness says, so it needs more lift.
  const floor = h > 0.55 && h < 0.75 ? 0.68 : 0.6;
  c = l >= floor ? hex : `hsl(${Math.round(h * 360)} ${Math.round(s * 100)}% ${Math.round(floor * 100)}%)`;
  colorCache.set(hex, c);
  return c;
}

// ---------- IRC ----------
function unescapeTag(v) {
  return v.replace(/\\(.)?/g, (_, c) => (c === ":" ? ";" : c === "s" ? " " : c === "r" ? "\r" : c === "n" ? "\n" : c || ""));
}
function parseIrc(line) {
  const msg = { tags: {}, prefix: "", command: "", params: [] };
  let i = 0;
  if (line[0] === "@") {
    const sp = line.indexOf(" ");
    for (const kv of line.slice(1, sp).split(";")) {
      const eq = kv.indexOf("=");
      if (eq < 0) msg.tags[kv] = "";
      else msg.tags[kv.slice(0, eq)] = unescapeTag(kv.slice(eq + 1));
    }
    i = sp + 1;
  }
  if (line[i] === ":") {
    const sp = line.indexOf(" ", i);
    msg.prefix = line.slice(i + 1, sp);
    i = sp + 1;
  }
  const rest = line.slice(i);
  const t = rest.indexOf(" :");
  const head = t < 0 ? rest : rest.slice(0, t);
  const parts = head.split(" ").filter(Boolean);
  msg.command = parts.shift() || "";
  msg.params = parts;
  if (t >= 0) msg.params.push(rest.slice(t + 2));
  return msg;
}

let ws = null;
let retryTimer = 0;
let backoff = 1000;
let authFails = 0;
let lastRx = 0;
let joined = false;
let wasJoined = false;

async function connect(fresh = false) {
  clearTimeout(retryTimer);
  if (ws) { ws.onclose = null; ws.onmessage = null; try { ws.close(); } catch {} ws = null; }
  if (!me) return;
  let tok;
  try {
    tok = await invoke("chat_token", { fresh });
  } catch (e) {
    systemLine(`Could not sign in to chat: ${errText(e)}`);
    return retry();
  }
  const sock = new WebSocket(IRC_URL);
  ws = sock;
  sock.onopen = () => {
    sock.send("CAP REQ :twitch.tv/tags twitch.tv/commands");
    sock.send(`PASS oauth:${tok.accessToken}`);
    sock.send(`NICK ${tok.login}`);
    sock.send(`JOIN #${channel.login}`);
  };
  sock.onmessage = (e) => {
    lastRx = Date.now();
    for (const line of e.data.split("\r\n")) if (line) handle(parseIrc(line), false);
  };
  sock.onclose = () => {
    if (ws !== sock) return;
    if (joined) systemLine("Disconnected from chat. Reconnecting.");
    joined = false;
    retry();
  };
}
function retry() {
  clearTimeout(retryTimer);
  retryTimer = setTimeout(() => connect(), backoff);
  backoff = Math.min(backoff * 2, 30000);
}
// Twitch pings every few minutes. Silence for longer than that means the socket died quietly.
setInterval(() => {
  if (ws && joined && Date.now() - lastRx > 6 * 60 * 1000) connect();
}, 60 * 1000);

function handle(m, historical) {
  switch (m.command) {
    case "PING":
      ws?.send(`PONG :${m.params[0] || "tmi.twitch.tv"}`);
      break;
    case "PRIVMSG":
      push(renderMessage(m, historical));
      break;
    case "USERNOTICE":
      push(renderEvent(m, historical));
      break;
    case "CLEARCHAT":
      clearChat(m);
      break;
    case "CLEARMSG": {
      const row = rowsById.get(m.tags["target-msg-id"]);
      if (row) row.classList.add("gone");
      break;
    }
    case "NOTICE": {
      const text = m.params[m.params.length - 1] || "";
      if (/authentication failed|improperly formatted auth/i.test(text)) {
        authFails += 1;
        if (authFails <= 2) connect(true);
        else systemLine("Twitch did not accept the sign-in for chat. Sign out and sign in again.");
        return;
      }
      if (!historical) systemLine(text);
      break;
    }
    case "ROOMSTATE":
      if (!joined) {
        joined = true;
        authFails = 0;
        backoff = 1000;
        if (wasJoined) systemLine("Reconnected.");
        wasJoined = true;
      }
      break;
    case "RECONNECT":
      connect();
      break;
  }
}

// ---------- Rendering ----------
function badgeImgs(tag) {
  const out = [];
  if (!tag) return out;
  for (const b of tag.split(",")) {
    const info = badges.get(b);
    if (!info) continue;
    const img = el("img", "bd");
    img.src = info.src;
    img.srcset = info.srcset;
    img.alt = info.title;
    img.title = info.title;
    img.decoding = "async";
    out.push(img);
  }
  return out;
}

function emoteNode(e, code) {
  const wrap = el("span", "e");
  wrap.appendChild(emoteImg(e, code, false));
  return wrap;
}
function emoteImg(e, code, zw) {
  const img = document.createElement("img");
  img.src = e.src;
  if (e.srcset) img.srcset = e.srcset;
  img.alt = code;
  img.title = e.by ? `${code}\n${e.by}` : code;
  if (e.w && e.h) { img.width = Math.round((e.w * 28) / e.h); img.height = 28; }
  img.decoding = "async";
  if (zw) img.className = "zw";
  return img;
}

const URL_RE = /^(https?:\/\/)?([a-z0-9-]+\.)+[a-z]{2,}(\/\S*)?$/i;

// Message text into nodes: Twitch emotes by position (code points), then 7TV/BTTV/FFZ by word,
// links and @mentions. A zero width emote stacks onto the emote before it.
function renderBody(text, emoteTag, skipWords = 0) {
  const frag = document.createDocumentFragment();
  const cps = Array.from(text);
  const ranges = [];
  if (emoteTag) {
    for (const part of emoteTag.split("/")) {
      const [id, pos] = part.split(":");
      if (!pos) continue;
      for (const r of pos.split(",")) {
        const [a, b] = r.split("-").map(Number);
        ranges.push({ a, b, id });
      }
    }
    ranges.sort((x, y) => x.a - y.a);
  }
  // Words and emotes in order. Runs of spaces collapse to one, as they do on Twitch.
  const tokens = [];
  let i = 0;
  const words = (str) => { for (const w of str.split(" ")) if (w) tokens.push(w); };
  for (const r of ranges) {
    if (r.a < i) continue;
    words(cps.slice(i, r.a).join(""));
    const code = cps.slice(r.a, r.b + 1).join("");
    tokens.push({ code, e: { src: `https://static-cdn.jtvnw.net/emoticons/v2/${r.id}/default/dark/1.0`, srcset: `https://static-cdn.jtvnw.net/emoticons/v2/${r.id}/default/dark/2.0 2x`, w: 28, h: 28, by: "Twitch" } });
    i = r.b + 1;
  }
  words(cps.slice(i).join(""));

  let lastEmote = null;
  let text_ = "";
  const flushText = () => { if (text_) { frag.appendChild(document.createTextNode(text_)); text_ = ""; } };
  let first = true;
  for (const tok of tokens.slice(skipWords)) {
    if (typeof tok === "object") {
      if (!first) text_ += " ";
      flushText();
      lastEmote = emoteNode(tok.e, tok.code);
      frag.appendChild(lastEmote);
      first = false;
      continue;
    }
    const e = emotes.get(tok);
    if (e && e.zw && lastEmote) {
      lastEmote.appendChild(emoteImg(e, tok, true));
      continue;
    }
    if (!first) text_ += " ";
    first = false;
    if (e) {
      flushText();
      lastEmote = emoteNode(e, tok);
      frag.appendChild(lastEmote);
      continue;
    }
    lastEmote = null;
    if (tok.length > 3 && URL_RE.test(tok)) {
      flushText();
      const a = el("span", "link", tok);
      a.dataset.url = /^https?:\/\//i.test(tok) ? tok : `https://${tok}`;
      frag.appendChild(a);
    } else if (tok[0] === "@" && tok.length > 1) {
      flushText();
      frag.appendChild(el("span", "mention", tok));
    } else {
      text_ += tok;
    }
  }
  flushText();
  return frag;
}

function rememberUser(login, tags, row) {
  let u = users.get(login);
  if (!u) { u = { id: tags["user-id"], name: tags["display-name"] || login, color: "", rows: [] }; users.set(login, u); }
  if (tags["user-id"]) u.id = tags["user-id"];
  u.name = tags["display-name"] || login;
  u.color = nameColor(tags.color, login);
  u.rows.push(row);
  if (u.rows.length > 30) u.rows.shift();
  const at = recentNames.indexOf(u.name);
  if (at >= 0) recentNames.splice(at, 1);
  recentNames.push(u.name);
  if (recentNames.length > 300) recentNames.shift();
}

function renderMessage(m, historical) {
  const t = m.tags;
  const login = m.prefix.split("!")[0];
  let text = m.params[1] || "";
  let action = false;
  if (text.startsWith("\u0001ACTION ") && text.endsWith("\u0001")) { text = text.slice(8, -1); action = true; }
  const name = t["display-name"] || login;
  const color = nameColor(t.color, login);

  const row = el("div", "m");
  row.dataset.id = t.id || "";
  row.dataset.login = login;
  if (historical) row.classList.add("old");
  if (action) row.classList.add("me");
  if (me && login !== me.login && new RegExp(`(^|[^\\w])@?${me.login}([^\\w]|$)`, "i").test(text)) row.classList.add("hl");

  let skip = 0;
  if (t["reply-parent-display-name"]) {
    const r = el("div", "reply-to");
    r.append(icon("reply"), `Replying to @${t["reply-parent-display-name"]}: ${t["reply-parent-msg-body"] || ""}`);
    row.appendChild(r);
    // Twitch starts a reply with "@name"; the line above already says who it answers.
    if (text.toLowerCase().startsWith(`@${(t["reply-parent-user-login"] || "").toLowerCase()} `)) skip = 1;
  }
  row.appendChild(el("span", "ts", clock(t["tmi-sent-ts"])));
  if (t["first-msg"] === "1") row.appendChild(el("span", "first", "First message"));
  for (const b of badgeImgs(t.badges)) row.appendChild(b);
  const nm = el("span", "nm", name);
  nm.style.color = color;
  if (name.toLowerCase() !== login) nm.title = login;
  row.appendChild(nm);
  row.appendChild(el("span", "sep", action ? "" : ":"));
  const body = el("span", "body");
  if (action) body.style.color = color;
  body.appendChild(renderBody(text, t.emotes, skip));
  row.appendChild(body);

  if (t.id) rowsById.set(t.id, row);
  rememberUser(login, t, row);
  return row;
}

function renderEvent(m, historical) {
  const t = m.tags;
  const box = el("div", "ev");
  if (historical) box.classList.add("old");
  box.appendChild(el("div", "ev-head", t["system-msg"] || "Something happened in chat"));
  const text = m.params[1];
  if (text) {
    const login = t.login || "";
    const line = el("div", "ev-msg");
    for (const b of badgeImgs(t.badges)) line.appendChild(b);
    const nm = el("span", "nm", t["display-name"] || login);
    nm.style.color = nameColor(t.color, login);
    line.append(nm, el("span", "sep", ":"));
    const body = el("span", "body");
    body.appendChild(renderBody(text, t.emotes));
    line.appendChild(body);
    box.appendChild(line);
    box.dataset.login = login;
    box.dataset.id = t.id || "";
    if (t.id) rowsById.set(t.id, box);
  }
  return box;
}

function systemLine(text) {
  push(el("div", "sys", text));
}

function clearChat(m) {
  const target = m.params[1];
  if (!target) {
    for (const row of list.querySelectorAll(".m")) row.classList.add("gone");
    systemLine("A moderator cleared chat.");
    return;
  }
  const u = users.get(target);
  for (const row of u?.rows || []) row.classList.add("gone");
  const who = u?.name || target;
  const secs = m.tags["ban-duration"];
  systemLine(secs ? `${who} was timed out for ${duration(secs)}.` : `${who} was banned.`);
}

// ---------- The list: batched appends, sticking to the bottom ----------
const list = $("list");
const pending = [];
let flushQueued = false;
let stick = true;
let unseen = 0;
let pointerUntil = 0; // chat holds still while the pointer moves over it, so buttons stay put

function push(node) {
  pending.push(node);
  if (pending.length > MAX_LINES) pending.splice(0, pending.length - MAX_LINES);
  if (!flushQueued) { flushQueued = true; requestAnimationFrame(flush); }
}
function following() { return stick && Date.now() > pointerUntil; }
function flush() {
  flushQueued = false;
  if (!pending.length) return;
  const n = pending.length;
  const frag = document.createDocumentFragment();
  for (const node of pending) frag.appendChild(node);
  pending.length = 0;
  list.appendChild(frag);
  const follow = following();
  trim(follow ? MAX_LINES : MAX_LINES * 2);
  if (follow) list.scrollTop = list.scrollHeight;
  else { unseen += n; showJump(); }
}
function trim(limit) {
  let extra = list.childElementCount - limit;
  if (extra <= 0) return;
  const before = list.scrollHeight;
  while (extra-- > 0) {
    const row = list.firstElementChild;
    if (row.dataset.id) rowsById.delete(row.dataset.id);
    if (row === railRow) hideRail();
    row.remove();
  }
  if (!following()) list.scrollTop -= before - list.scrollHeight;
}
function toBottom() {
  stick = true;
  pointerUntil = 0;
  unseen = 0;
  list.scrollTop = list.scrollHeight;
  $("jump").hidden = true;
}
function showJump() {
  if (!stick || unseen > 0) {
    $("jump-text").textContent = unseen > 0 ? `${unseen > 99 ? "99+" : unseen} new message${unseen === 1 ? "" : "s"}` : "Back to live chat";
    $("jump").hidden = false;
  }
}
list.addEventListener("scroll", () => {
  const atBottom = list.scrollHeight - list.scrollTop - list.clientHeight < 32;
  if (atBottom && !stick) { stick = true; if (Date.now() > pointerUntil) { unseen = 0; $("jump").hidden = true; } }
  else if (!atBottom && stick && list.scrollTop > 0) { stick = false; showJump(); }
}, { passive: true });
// Images that arrive late change row heights; keep the newest message in view.
list.addEventListener("load", () => { if (following()) list.scrollTop = list.scrollHeight; }, true);
$("jump").addEventListener("click", toBottom);

let resumeTimer = 0;
list.addEventListener("mousemove", () => {
  pointerUntil = Date.now() + 1200;
  clearTimeout(resumeTimer);
  resumeTimer = setTimeout(resume, 1250);
}, { passive: true });
list.addEventListener("mouseleave", () => { pointerUntil = 0; resume(); });
function resume() {
  if (railHeld()) { resumeTimer = setTimeout(resume, 500); return; }
  if (stick && Date.now() >= pointerUntil) { unseen = 0; $("jump").hidden = true; list.scrollTop = list.scrollHeight; }
}

// ---------- The action rail ----------
const rail = $("rail");
let railRow = null;
let banArmed = 0;
function railHeld() { return rail.matches(":hover") || !$("card").hidden; }
function hideRail() {
  if (railRow) railRow.classList.remove("on");
  railRow = null;
  rail.hidden = true;
  disarmBan();
}
function disarmBan() {
  clearTimeout(banArmed);
  banArmed = 0;
  rail.querySelector('[data-act="ban"]').classList.remove("confirm");
}
list.addEventListener("mouseover", (e) => {
  const row = e.target.closest(".m, .ev[data-login]");
  if (!row || row === railRow || !me) return;
  if (railRow) railRow.classList.remove("on");
  disarmBan();
  railRow = row;
  row.classList.add("on");
  const own = row.dataset.login === me.login;
  const isBroadcaster = row.dataset.login === channel.login;
  rail.querySelector('[data-act="reply"]').hidden = !row.dataset.id;
  rail.querySelector('[data-act="delete"]').hidden = !row.dataset.id || isBroadcaster && !own;
  rail.querySelector('[data-act="timeout"]').hidden = own || isBroadcaster;
  rail.querySelector('[data-act="ban"]').hidden = own || isBroadcaster;
  rail.hidden = false;
  row.appendChild(rail);
});
list.addEventListener("mouseleave", () => { if (!rail.matches(":hover")) hideRail(); });

rail.addEventListener("click", async (e) => {
  const btn = e.target.closest("button");
  if (!btn || !railRow) return;
  const row = railRow;
  const login = row.dataset.login;
  const act = btn.dataset.act;
  if (act === "reply") return startReply(row);
  if (act === "delete") return run(() => deleteMessage(row.dataset.id), null);
  if (act === "timeout") return run(() => timeoutUser(login, 600), null);
  if (act === "ban") {
    if (!banArmed) {
      btn.classList.add("confirm");
      banArmed = setTimeout(disarmBan, 3000);
      return;
    }
    disarmBan();
    return run(() => timeoutUser(login, 0), null);
  }
});

async function run(fn, done) {
  try {
    await fn();
    if (done) toast(done);
  } catch (e) {
    toast(errText(e), true);
  }
}

// ---------- Moderation ----------
async function userId(login) {
  login = login.replace(/^@/, "").toLowerCase();
  const known = users.get(login)?.id;
  if (known) return known;
  const r = await helix("GET", "/users", [["login", login]]);
  const id = r?.data?.[0]?.id;
  if (!id) throw new Error(`No Twitch user called ${login}.`);
  return id;
}
const modQuery = () => [["broadcaster_id", channel.id], ["moderator_id", me.userId]];
async function deleteMessage(id) {
  await helix("DELETE", "/moderation/chat", [...modQuery(), ["message_id", id]]);
}
// seconds 0 bans.
async function timeoutUser(login, seconds, reason = "") {
  const data = { user_id: await userId(login) };
  if (seconds) data.duration = seconds;
  if (reason) data.reason = reason;
  await helix("POST", "/moderation/bans", modQuery(), { data });
}
async function unbanUser(login) {
  await helix("DELETE", "/moderation/bans", [...modQuery(), ["user_id", await userId(login)]]);
}

// ---------- Chatter card ----------
const card = $("card");
let cardLogin = "";
list.addEventListener("click", (e) => {
  const link = e.target.closest(".link");
  if (link) { invoke("open_external", { url: link.dataset.url }).catch((err) => toast(errText(err), true)); return; }
  const nm = e.target.closest(".nm, .mention");
  if (nm) {
    const login = nm.classList.contains("mention")
      ? nm.textContent.replace(/^@/, "").replace(/[^\w]+$/, "").toLowerCase()
      : nm.closest("[data-login]")?.dataset.login;
    if (login) openCard(login, nm.getBoundingClientRect());
  }
});
function openCard(login, at) {
  cardLogin = login;
  const u = users.get(login);
  $("card-name").textContent = u?.name || login;
  $("card-name").style.color = u?.color || "";
  $("card-login").textContent = login;
  const self = me && (login === me.login || login === channel.login);
  $("card-acts").hidden = !!self;
  const msgs = $("card-msgs");
  msgs.textContent = "";
  const rows = (u?.rows || []).filter((r) => r.isConnected).slice(-12);
  if (!rows.length) msgs.appendChild(el("p", "empty", "Nothing in chat yet."));
  for (const r of rows) {
    const c = el("div", `m${r.classList.contains("gone") ? " gone" : ""}`);
    c.append(el("span", "ts", r.querySelector(".ts")?.textContent || ""));
    c.append(r.querySelector(".body").cloneNode(true));
    msgs.appendChild(c);
  }
  card.hidden = false;
  const w = card.offsetWidth, h = card.offsetHeight;
  card.style.left = `${Math.max(12, Math.min(at.left, innerWidth - w - 12))}px`;
  card.style.top = `${Math.max(52, Math.min(at.bottom + 6, innerHeight - h - 12))}px`;
}
function closeCard() { card.hidden = true; cardLogin = ""; }
$("card-close").addEventListener("click", closeCard);
$("card-acts").addEventListener("click", (e) => {
  const btn = e.target.closest("button");
  if (!btn || !cardLogin) return;
  const login = cardLogin;
  const name = users.get(login)?.name || login;
  if (btn.dataset.timeout) return run(() => timeoutUser(login, Number(btn.dataset.timeout)), `${name} timed out for ${duration(btn.dataset.timeout)}.`);
  if (btn.dataset.card === "ban") {
    if (btn.dataset.armed) { delete btn.dataset.armed; btn.lastChild.textContent = "Ban"; return run(() => timeoutUser(login, 0), `${name} is banned.`); }
    btn.dataset.armed = "1";
    btn.lastChild.textContent = "Ban, sure?";
    setTimeout(() => { delete btn.dataset.armed; btn.lastChild.textContent = "Ban"; }, 3000);
    return;
  }
  if (btn.dataset.card === "unban") return run(() => unbanUser(login), `${name} can chat again.`);
});
$("card-twitch").addEventListener("click", () => {
  invoke("open_external", { url: `https://www.twitch.tv/popout/${channel.login}/viewercard/${cardLogin}` }).catch((e) => toast(errText(e), true));
});

// ---------- Composer ----------
const input = $("input");
let replyTo = null;
const sent = [];
let sentAt = -1;

function startReply(row) {
  const u = users.get(row.dataset.login);
  replyTo = { id: row.dataset.id, name: u?.name || row.dataset.login };
  const span = $("reply-text");
  span.textContent = "";
  span.append("Replying to ", el("b", null, `@${replyTo.name}`));
  $("reply-bar").hidden = false;
  input.focus();
}
function cancelReply() { replyTo = null; $("reply-bar").hidden = true; }
$("reply-cancel").addEventListener("click", cancelReply);

$("composer").addEventListener("submit", async (e) => {
  e.preventDefault();
  const text = input.value.trim();
  if (!text || !me) return;
  sent.push(text);
  if (sent.length > 50) sent.shift();
  sentAt = -1;
  input.value = "";
  const reply = replyTo;
  cancelReply();
  try {
    if (text.startsWith("/") && !text.startsWith("//")) await command(text);
    else await sendMessage(text.replace(/^\/\//, "/"), reply);
  } catch (err) {
    if (!input.value) input.value = text;
    toast(errText(err), true);
  }
});

async function sendMessage(text, reply) {
  const body = { broadcaster_id: channel.id, sender_id: me.userId, message: text };
  if (reply) body.reply_parent_message_id = reply.id;
  const r = await helix("POST", "/chat/messages", [], body);
  const d = r?.data?.[0];
  if (d && !d.is_sent) throw new Error(d.drop_reason?.message || "Twitch did not send that message.");
}

const HELP = "Commands: /ban name [reason], /unban name, /timeout name [seconds] [reason], /untimeout name, /clear. Start with // to send a message that begins with /.";
async function command(text) {
  const [cmd, who, ...rest] = text.slice(1).split(/\s+/);
  const need = () => { if (!who) throw new Error(`Add a name: /${cmd} name`); return who.replace(/^@/, "").toLowerCase(); };
  switch (cmd.toLowerCase()) {
    case "ban": await timeoutUser(need(), 0, rest.join(" ")); return toast(`${who} is banned.`);
    case "unban": case "untimeout": await unbanUser(need()); return toast(`${who} can chat again.`);
    case "timeout": {
      const login = need();
      let secs = 600;
      if (rest[0] && /^\d+[smhd]?$/i.test(rest[0])) {
        const n = parseInt(rest[0], 10);
        secs = n * ({ s: 1, m: 60, h: 3600, d: 86400 }[rest.shift().slice(-1).toLowerCase()] || 1);
      }
      await timeoutUser(login, secs, rest.join(" "));
      return toast(`${who} timed out for ${duration(secs)}.`);
    }
    case "clear": await helix("DELETE", "/moderation/chat", modQuery()); return;
    case "help": return toast(HELP);
    default: throw new Error(`Sylva Chat does not know /${cmd}. ${HELP}`);
  }
}

// Tab completes emote codes and chatter names; Up and Down walk through what you sent.
let completion = null;
input.addEventListener("keydown", (e) => {
  if (e.key === "Tab") {
    e.preventDefault();
    if (!completion) {
      const caret = input.selectionStart;
      const before = input.value.slice(0, caret);
      const start = before.lastIndexOf(" ") + 1;
      const word = before.slice(start);
      if (!word) return;
      const at = word[0] === "@";
      const q = (at ? word.slice(1) : word).toLowerCase();
      const names = [...recentNames].reverse().filter((n) => n.toLowerCase().startsWith(q)).map((n) => (at ? `@${n}` : n));
      const codes = at ? [] : [...emotes.keys()].filter((c) => c.toLowerCase().startsWith(q)).sort((a, b) => a.length - b.length || a.localeCompare(b));
      const options = [...codes, ...names].slice(0, 50);
      if (!options.length) return;
      completion = { start, end: caret, options, i: -1, after: input.value.slice(caret) };
    }
    const c = completion;
    c.i = (c.i + (e.shiftKey ? -1 : 1) + c.options.length) % c.options.length;
    const pick = `${c.options[c.i]} `;
    input.value = input.value.slice(0, c.start) + pick + c.after;
    const pos = c.start + pick.length;
    input.setSelectionRange(pos, pos);
    return;
  }
  if (e.key !== "Shift") completion = null;
  if (e.key === "Escape") { if (replyTo) cancelReply(); else input.blur(); }
  if (e.key === "ArrowUp" && sent.length && (input.value === "" || sentAt >= 0)) {
    e.preventDefault();
    sentAt = sentAt < 0 ? sent.length - 1 : Math.max(0, sentAt - 1);
    input.value = sent[sentAt];
  }
  if (e.key === "ArrowDown" && sentAt >= 0) {
    e.preventDefault();
    sentAt += 1;
    if (sentAt >= sent.length) { sentAt = -1; input.value = ""; } else input.value = sent[sentAt];
  }
});
input.addEventListener("input", () => { completion = null; });

// ---------- Emotes, badges, history ----------
function addFfz(sets, only) {
  for (const [id, set] of Object.entries(sets || {})) {
    if (only && !only.includes(Number(id))) continue;
    for (const e of set.emoticons || []) {
      const urls = e.animated || e.urls || {};
      const one = urls["1"];
      if (!one) continue;
      emotes.set(e.name, { src: one, srcset: urls["2"] ? `${urls["2"]} 2x` : "", w: e.width, h: e.height, by: "FrankerFaceZ" });
    }
  }
}
function addBttv(list_) {
  for (const e of list_ || []) {
    const base = `https://cdn.betterttv.net/emote/${e.id}`;
    emotes.set(e.code, { src: `${base}/1x.webp`, srcset: `${base}/2x.webp 2x`, w: e.width, h: e.height, zw: BTTV_ZERO_WIDTH.has(e.code), by: "BetterTTV" });
  }
}
function add7tv(set) {
  for (const e of set?.emotes || []) {
    const host = e.data?.host;
    if (!host?.url) continue;
    const files = (host.files || []).filter((f) => f.format === "WEBP");
    const one = files.find((f) => f.name.startsWith("1x")) || files[0];
    const two = files.find((f) => f.name.startsWith("2x"));
    if (!one) continue;
    const base = `https:${host.url}`;
    emotes.set(e.name, {
      src: `${base}/${one.name}`,
      srcset: two ? `${base}/${two.name} 2x` : "",
      w: one.width, h: one.height,
      zw: ((e.flags || 0) & 1) === 1 || ((e.data?.flags || 0) & 256) === 256,
      by: "7TV",
    });
  }
}
async function loadEmotes() {
  const id = channel.id;
  const get = (url) => fetchJson(url).catch(() => null);
  const [ffzG, bttvG, stvG, ffzC, bttvC, stvC] = await Promise.all([
    get("https://api.frankerfacez.com/v1/set/global"),
    get("https://api.betterttv.net/3/cached/emotes/global"),
    get("https://7tv.io/v3/emote-sets/global"),
    get(`https://api.frankerfacez.com/v1/room/id/${id}`),
    get(`https://api.betterttv.net/3/cached/users/twitch/${id}`),
    get(`https://7tv.io/v3/users/twitch/${id}`),
  ]);
  emotes.clear();
  // Later wins: channel emotes over global ones, and 7TV over BTTV over FFZ.
  addFfz(ffzG?.sets, ffzG?.default_sets);
  addBttv(bttvG);
  add7tv(stvG);
  addFfz(ffzC?.sets);
  addBttv([...(bttvC?.sharedEmotes || []), ...(bttvC?.channelEmotes || [])]);
  add7tv(stvC?.emote_set);
}
async function loadBadges() {
  const [g, c] = await Promise.all([
    helix("GET", "/chat/badges/global").catch(() => null),
    helix("GET", "/chat/badges", [["broadcaster_id", channel.id]]).catch(() => null),
  ]);
  for (const r of [g, c]) {
    for (const set of r?.data || []) {
      for (const v of set.versions || []) {
        badges.set(`${set.set_id}/${v.id}`, { src: v.image_url_1x, srcset: `${v.image_url_2x} 2x`, title: v.title || set.set_id });
      }
    }
  }
}
async function loadHistory() {
  const r = await fetchJson(`https://recent-messages.robotty.de/api/v2/recent-messages/${channel.login}?limit=${HISTORY}`).catch(() => null);
  const lines = r?.messages || [];
  for (const line of lines) {
    const m = parseIrc(line);
    if (m.command === "PRIVMSG" || m.command === "USERNOTICE" || m.command === "CLEARCHAT" || m.command === "CLEARMSG") handle(m, true);
  }
  if (lines.length) push(el("div", "divider", "Live"));
}

// ---------- Session ----------
async function startChat(user) {
  me = user;
  channel = { login: user.login, id: user.userId };
  $("signin-view").hidden = true;
  $("chat-view").hidden = false;
  $("account-btn").hidden = false;
  $("channel-tag").textContent = `#${channel.login}`;
  $("channel-tag").hidden = false;
  $("menu-who").textContent = "";
  $("menu-who").append("Signed in as ", el("b", null, user.login));
  list.textContent = "";
  rowsById.clear();
  users.clear();
  $("loading").hidden = false;
  await Promise.all([loadEmotes(), loadBadges()]);
  await loadHistory();
  $("loading").hidden = true;
  connect();
  input.focus();
}

let suggested = "";
function showSignIn(suggestedId) {
  me = null;
  channel = null;
  if (ws) { ws.onclose = null; ws.close(); ws = null; }
  clearTimeout(retryTimer);
  joined = wasJoined = false;
  $("chat-view").hidden = true;
  $("account-btn").hidden = true;
  $("channel-tag").hidden = true;
  $("signin-view").hidden = false;
  $("code-box").hidden = true;
  $("connect").disabled = false;
  suggested = suggestedId || suggested;
  let saved = "";
  try { saved = localStorage.getItem("client-id") || ""; } catch {}
  $("client-id").value = saved || suggested;
  const filled = !saved && !!suggested;
  $("cid-hint").hidden = !filled;
  $("cid-steps").hidden = !!(saved || suggested);
}

$("connect").addEventListener("click", async () => {
  const clientId = $("client-id").value.trim();
  $("connect").disabled = true;
  try {
    const code = await invoke("sign_in", { clientId });
    try { localStorage.setItem("client-id", clientId); } catch {}
    $("user-code").textContent = code.user_code;
    $("open-again").dataset.url = code.verification_uri;
    $("code-box").hidden = false;
  } catch (e) {
    $("connect").disabled = false;
    toast(errText(e), true);
  }
});
$("open-again").addEventListener("click", (e) => {
  e.preventDefault();
  invoke("open_external", { url: e.currentTarget.dataset.url }).catch(() => {});
});
$("cancel-signin").addEventListener("click", async (e) => {
  e.preventDefault();
  await invoke("cancel_sign_in").catch(() => {});
  $("code-box").hidden = true;
  $("connect").disabled = false;
});
document.addEventListener("click", (e) => {
  const a = e.target.closest("[data-ext]");
  if (a) { e.preventDefault(); invoke("open_external", { url: a.dataset.ext }).catch(() => {}); }
});

// ---------- Title bar ----------
$("win-min").addEventListener("click", () => appWindow.minimize().catch(console.warn));
$("win-max").addEventListener("click", () => appWindow.toggleMaximize().catch(console.warn));
$("win-close").addEventListener("click", () => appWindow.close().catch(console.warn));
document.querySelector(".titlebar").addEventListener("mousedown", (e) => {
  if (e.button !== 0 || !e.target.closest("[data-drag-region]")) return;
  if (e.detail === 2) appWindow.toggleMaximize().catch(console.warn);
  else if (e.detail === 1) invoke("start_window_drag").catch(console.warn);
});

function setPinned(on) {
  $("pin-btn").setAttribute("aria-pressed", String(on));
  $("pin-btn").title = on ? "Stop keeping on top" : "Keep on top";
  appWindow.setAlwaysOnTop(on).catch(console.warn);
  try { localStorage.setItem("pinned", on ? "1" : ""); } catch {}
}
$("pin-btn").addEventListener("click", () => setPinned($("pin-btn").getAttribute("aria-pressed") !== "true"));

const menu = $("menu");
$("account-btn").addEventListener("click", (e) => { e.stopPropagation(); menu.hidden = !menu.hidden; });
$("sign-out").addEventListener("click", async () => {
  menu.hidden = true;
  try {
    const s = await invoke("sign_out");
    showSignIn(s.suggestedClientId);
  } catch (e) {
    toast(errText(e), true);
  }
});
document.addEventListener("mousedown", (e) => {
  if (!menu.hidden && !menu.contains(e.target) && !e.target.closest("#account-btn")) menu.hidden = true;
  if (!card.hidden && !card.contains(e.target) && !e.target.closest(".nm, .mention")) closeCard();
});
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") { menu.hidden = true; closeCard(); }
});
document.addEventListener("visibilitychange", () => {
  document.documentElement.toggleAttribute("data-hidden", document.hidden);
});

// ---------- Updates ----------
async function checkUpdate() {
  try {
    const u = await invoke("check_for_update");
    if (u.version) {
      $("update-btn").hidden = false;
      $("update-btn").title = `Sylva Chat ${u.version} is ready`;
    }
  } catch (e) {
    console.warn(e);
  }
}
$("update-btn").addEventListener("click", async () => {
  $("update-btn").disabled = true;
  toast("Updating Sylva Chat. It restarts by itself.");
  try {
    await invoke("apply_update");
  } catch (e) {
    $("update-btn").disabled = false;
    toast(errText(e), true);
  }
});

// ---------- Start ----------
(async () => {
  let pinned = false;
  try { pinned = localStorage.getItem("pinned") === "1"; } catch {}
  if (pinned) setPinned(true);
  await listen("signed-in", (e) => startChat(e.payload));
  await listen("sign-in-failed", (e) => {
    $("code-box").hidden = true;
    $("connect").disabled = false;
    toast(String(e.payload), true);
  });
  try {
    const s = await invoke("restore_session");
    if (s.user) startChat(s.user);
    else showSignIn(s.suggestedClientId);
  } catch (e) {
    showSignIn("");
    toast(errText(e), true);
  }
  checkUpdate();
  setInterval(checkUpdate, 6 * 60 * 60 * 1000);
})();
