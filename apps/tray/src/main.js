// 在 Tauri 之外（如浏览器直接预览）优雅降级，仅用于查看布局。
const TAURI = window.__TAURI__;
const invoke = TAURI ? TAURI.core.invoke : async () => { throw new Error("非 Tauri 环境"); };
const listen = TAURI ? TAURI.event.listen : () => {};

const $ = (sel) => document.querySelector(sel);

// ---- 标签页切换 ----
document.querySelectorAll(".tab").forEach((tab) => {
  tab.addEventListener("click", () => {
    document.querySelectorAll(".tab").forEach((t) => t.classList.remove("active"));
    document.querySelectorAll(".page").forEach((p) => p.classList.remove("active"));
    tab.classList.add("active");
    $("#page-" + tab.dataset.tab).classList.add("active");
    if (tab.dataset.tab === "permissions") refreshPermissions();
    if (tab.dataset.tab === "keys") refreshButtons();
  });
});

// ---- 状态条 ----
function applyStatus(dto) {
  const bar = $("#statusbar");
  bar.dataset.kind = dto.kind;
  $("#status-text").textContent = dto.detail;
}
listen("bridge-status", (e) => applyStatus(e.payload));

// ---- 连接页 ----
let running = false;
let platform = "macos";

const LOOPBACK_HINTS = {
  macos: "未发现回环设备。请先安装 BlackHole 2ch（existential.audio/blackhole），然后重开本页。",
  windows: "未发现回环设备。请先安装 VB-Cable（vb-audio.com/Cable），然后重开本页。",
  linux: "未发现回环设备。执行 pactl load-module module-null-sink sink_name=rctool 创建，然后重开本页。",
};

async function refreshOutputs() {
  const outputs = await invoke("list_outputs");
  const cfg = await invoke("get_config");
  const sel = $("#output-select");
  sel.innerHTML = "";
  const none = document.createElement("option");
  none.value = "";
  none.textContent = "— 选择输出设备 —";
  sel.appendChild(none);
  let hasLoopback = false;
  for (const o of outputs) {
    const opt = document.createElement("option");
    opt.value = o.name;
    let tag = "";
    if (o.is_loopback) { tag = "  ✓回环"; hasLoopback = true; }
    else if (o.is_default) tag = "  (默认)";
    opt.textContent = o.name + tag;
    if (o.name === cfg.output_device) opt.selected = true;
    sel.appendChild(opt);
  }
  const warn = $("#no-loopback");
  warn.textContent = LOOPBACK_HINTS[platform] || LOOPBACK_HINTS.macos;
  warn.hidden = hasLoopback;
}

/// 按平台裁剪界面：F5→Fn 与按键映射仅 macOS；Win+H 仅 Windows。
function applyPlatform() {
  const isMac = platform === "macos";
  $("#card-fn-remap").hidden = !isMac;
  $("#card-win-hotkey").hidden = platform !== "windows";
  const keysTab = document.querySelector('.tab[data-tab="keys"]');
  const permsTab = document.querySelector('.tab[data-tab="permissions"]');
  keysTab.style.display = isMac ? "" : "none";
  permsTab.style.display = isMac ? "" : "none";
}

$("#output-select").addEventListener("change", (e) => {
  invoke("set_output", { name: e.target.value || null });
});

$("#gain").addEventListener("input", (e) => {
  $("#gain-value").textContent = Number(e.target.value).toFixed(1);
});
$("#gain").addEventListener("change", (e) => {
  invoke("set_gain", { gainDb: Number(e.target.value) });
});

$("#fn-remap").addEventListener("change", (e) => {
  invoke("set_fn_remap", { enabled: e.target.checked });
});

$("#win-hotkey").addEventListener("change", (e) => {
  invoke("set_win_hotkey", { enabled: e.target.checked });
});

$("#toggle-bridge").addEventListener("click", async () => {
  const btn = $("#toggle-bridge");
  btn.disabled = true;
  try {
    if (running) {
      await invoke("stop_bridge");
    } else {
      await invoke("start_bridge");
    }
  } catch (err) {
    applyStatus({ kind: "error", detail: String(err), streaming: false });
  }
  btn.disabled = false;
  await syncRunning();
});

async function syncRunning() {
  const cfg = await invoke("get_config");
  running = cfg.running;
  const btn = $("#toggle-bridge");
  btn.textContent = running ? "停用桥接" : "启用桥接";
  btn.classList.toggle("running", running);
}

// ---- 按键页（遥控器实物图 + 可点热点）----
let actionsCache = null;
let buttonsById = {};
let selectedButton = null;

// 麦克风键固定用途，不进入通用映射。
const MIC_NOTE = "固定用于 RC003 语音 / 系统听写，不可改";
// Apple TV 款布局独有、RC003 无对应实体键的键。
const EXTRA_KEYS = { play_pause: "播放 / 暂停", mute: "静音" };

async function refreshButtons() {
  if (!actionsCache) actionsCache = await invoke("get_actions");
  const buttons = await invoke("get_buttons");
  const cfg = await invoke("get_config");
  $("#key-mapping").checked = cfg.key_mapping;
  $("#page-keys").dataset.enabled = cfg.key_mapping;

  buttonsById = {};
  for (const b of buttons) buttonsById[b.id] = b;

  // 在实物图上标出"已接管行为"的键。
  document.querySelectorAll(".remote .key").forEach((el) => {
    const id = el.dataset.button;
    const b = buttonsById[id];
    el.classList.toggle("managed", !!(b && b.managed));
    el.classList.toggle("selected", id === selectedButton);
  });

  buildTable(buttons);
  updateNote();
}

/// 右侧映射表：语音固定行 + 12 个可映射键（每行内嵌动作下拉）。
function buildTable(buttons) {
  const tbody = $("#map-rows");
  tbody.innerHTML = "";

  const micRow = document.createElement("tr");
  micRow.className = "maprow";
  micRow.dataset.button = "mic";
  micRow.innerHTML = '<td class="k">语音</td><td class="fixed">语音输入 / 听写（固定）</td>';
  micRow.addEventListener("click", () => selectButton("mic"));
  tbody.appendChild(micRow);

  for (const b of buttons) {
    const tr = document.createElement("tr");
    tr.className = "maprow" + (b.managed ? " managed" : "");
    tr.dataset.button = b.id;

    const tdKey = document.createElement("td");
    tdKey.className = "k";
    tdKey.innerHTML = `${b.label}<span class="dot"></span>`;

    const tdAction = document.createElement("td");
    const sel = document.createElement("select");
    for (const a of actionsCache) {
      const opt = document.createElement("option");
      opt.value = a.id;
      opt.textContent = a.label;
      if (a.id === b.action_id) opt.selected = true;
      sel.appendChild(opt);
    }
    sel.addEventListener("click", (e) => e.stopPropagation());
    sel.addEventListener("change", async (e) => {
      await invoke("set_binding", { buttonId: b.id, actionId: e.target.value });
      selectedButton = b.id;
      await refreshButtons();
    });
    tdAction.appendChild(sel);

    tr.appendChild(tdKey);
    tr.appendChild(tdAction);
    tr.addEventListener("click", () => selectButton(b.id));
    tbody.appendChild(tr);
  }
  syncRowSelection();
}

function syncRowSelection() {
  document.querySelectorAll("#map-rows .maprow").forEach((tr) => {
    tr.classList.toggle("selected", tr.dataset.button === selectedButton);
  });
}

function selectButton(id) {
  selectedButton = id;
  document.querySelectorAll(".remote .key").forEach((el) => {
    el.classList.toggle("selected", el.dataset.button === id);
  });
  syncRowSelection();
  const row = document.querySelector(`#map-rows .maprow[data-button="${id}"]`);
  if (row) row.scrollIntoView({ block: "nearest", behavior: "smooth" });
  updateNote();
}

function updateNote() {
  const note = $("#key-note");
  if (!selectedButton) {
    note.textContent = "点击遥控器按键或表格行查看说明";
    return;
  }
  if (selectedButton === "mic") {
    note.textContent = "语音键：" + MIC_NOTE;
    return;
  }
  const b = buttonsById[selectedButton];
  if (!b) {
    note.textContent =
      (EXTRA_KEYS[selectedButton] || selectedButton) +
      "键：此布局键在小米 RC003 上无对应实体键，暂未接入映射";
    return;
  }
  note.textContent =
    b.label + "键：" + (b.managed ? "已改变默认行为（拦截原生并注入所选动作）" : "保持系统原生行为");
}

// 遥控器样式切换（两套 SVG 布局，热点共用同一按钮模型）。
function applyRemoteStyle(style) {
  $("#wrap-rc003").hidden = style === "atv";
  $("#wrap-atv").hidden = style !== "atv";
  document.querySelectorAll("#remote-style .seg-btn").forEach((b) => {
    b.classList.toggle("active", b.dataset.style === style);
  });
  try { localStorage.setItem("remoteStyle", style); } catch {}
}
$("#remote-style").addEventListener("click", (e) => {
  const btn = e.target.closest(".seg-btn");
  if (btn) applyRemoteStyle(btn.dataset.style);
});
try {
  applyRemoteStyle(localStorage.getItem("remoteStyle") || "rc003");
} catch {
  applyRemoteStyle("rc003");
}

// 绑定 SVG 热点点击 / 键盘可达（两套布局的热点都在 DOM 中，一次绑定全覆盖）。
document.querySelectorAll(".remote .key").forEach((el) => {
  const id = el.dataset.button;
  el.addEventListener("click", () => selectButton(id));
  el.addEventListener("keydown", (e) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      selectButton(id);
    }
  });
});

$("#key-mapping").addEventListener("change", async (e) => {
  await invoke("set_key_mapping", { enabled: e.target.checked });
  $("#page-keys").dataset.enabled = e.target.checked;
  if (e.target.checked) refreshPermissions();
});

$("#reset-bindings").addEventListener("click", async () => {
  await invoke("reset_bindings");
  await refreshButtons();
});

// ---- 权限页 ----
async function refreshPermissions() {
  const p = await invoke("get_permissions");
  const setBadge = (id, granted) => {
    const el = $(id);
    el.textContent = granted ? "已授权" : "未授权";
    el.className = "badge " + (granted ? "ok" : "no");
  };
  setBadge("#badge-input", p.input_monitoring);
  setBadge("#badge-ax", p.accessibility);
  if (!p.applicable) {
    $("#perm-note").querySelector(".hint").textContent =
      "当前平台无需额外权限。";
  }
}

$("#req-input").addEventListener("click", async () => {
  await invoke("request_permissions");
  setTimeout(refreshPermissions, 500);
});
$("#req-ax").addEventListener("click", async () => {
  await invoke("request_permissions");
  setTimeout(refreshPermissions, 500);
});

// ---- 初始化 ----
async function init() {
  const cfg = await invoke("get_config");
  platform = cfg.platform || "macos";
  applyPlatform();
  $("#gain").value = cfg.gain_db;
  $("#gain-value").textContent = Number(cfg.gain_db).toFixed(1);
  $("#fn-remap").checked = cfg.fn_remap;
  $("#win-hotkey").checked = cfg.win_hotkey;
  await refreshOutputs();
  await syncRunning();
  applyStatus(
    cfg.running
      ? { kind: "searching", detail: "正在查找遥控器…", streaming: false }
      : { kind: "stopped", detail: "已停止", streaming: false }
  );
}

init();
