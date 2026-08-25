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
    if (tab.dataset.tab === "apps") refreshApps();
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
  macos: { text: "未发现回环设备。语音需要写入 BlackHole 才能成为虚拟麦克风。", btn: "安装 BlackHole" },
  windows: { text: "未发现回环设备。语音需要写入 VB-Cable 才能成为虚拟麦克风。", btn: "安装 VB-Cable" },
  linux: { text: "未发现回环设备。可直接创建一个 PipeWire/Pulse 虚拟设备。", btn: "一键创建虚拟设备" },
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
  const hint = LOOPBACK_HINTS[platform] || LOOPBACK_HINTS.macos;
  $("#no-loopback-text").textContent = hint.text;
  $("#setup-loopback").textContent = hint.btn;
  $("#no-loopback").hidden = hasLoopback;
}

$("#setup-loopback").addEventListener("click", async () => {
  try {
    $("#no-loopback-text").textContent = await invoke("setup_loopback");
    if (platform === "linux") await refreshOutputs();
  } catch (err) {
    $("#no-loopback-text").textContent = String(err);
  }
});

$("#recheck-outputs").addEventListener("click", () => refreshOutputs());

/// 按平台裁剪界面：F5→Fn 与按键映射仅 macOS；Win+H 仅 Windows。
function applyPlatform() {
  const isMac = platform === "macos";
  document.body.classList.toggle("mac", isMac);
  $("#card-fn-remap").hidden = !isMac;
  $("#card-win-hotkey").hidden = platform !== "windows";
  // Dock 是 macOS 概念，其他平台不显示这个开关。
  $("#card-hide-dock").hidden = !isMac;
  // 按键映射与按应用覆盖都依赖 macOS 的 HID/NSWorkspace 通路。
  for (const name of ["keys", "apps", "permissions"]) {
    document.querySelector(`.tab[data-tab="${name}"]`).style.display = isMac ? "" : "none";
  }
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

$("#hide-dock").addEventListener("change", (e) => {
  invoke("set_hide_dock_on_close", { enabled: e.target.checked });
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

// ---- 应用页（按前台应用覆盖：只呈现与全局映射的差异）----
//
// 界面刻意不复制「按键」页那张整表：一个应用值得单独记住的信息，就是它和全局
// 差在哪几个键。其余键沿用全局，全局改了它们也跟着改。
let appProfiles = [];
let selectedApp = null;
let baseButtons = [];
let frontApp = null;

async function refreshApps() {
  if (!actionsCache) actionsCache = await invoke("get_actions");
  const cfg = await invoke("get_config");
  $("#apps-need-mapping").hidden = cfg.key_mapping;
  baseButtons = await invoke("get_buttons");
  appProfiles = await invoke("get_app_profiles");
  if (!appProfiles.some((p) => p.bundle_id === selectedApp)) {
    selectedApp = appProfiles.length ? appProfiles[0].bundle_id : null;
  }
  renderAppList();
  renderAppDetail();
  await refreshAppPicker();
  await refreshFrontAppHint();
}

function renderAppList() {
  const list = $("#app-list");
  list.innerHTML = "";
  if (!appProfiles.length) {
    const empty = document.createElement("div");
    empty.className = "app-empty";
    empty.textContent = "还没有按应用的覆盖。";
    list.appendChild(empty);
    return;
  }
  for (const p of appProfiles) {
    const item = document.createElement("button");
    item.className =
      "app-item" + (p.bundle_id === selectedApp ? " selected" : "") + (p.enabled ? "" : " off");

    const name = document.createElement("div");
    name.className = "an";
    if (p.active) {
      const live = document.createElement("span");
      live.className = "live";
      live.title = "此刻正是前台应用";
      name.appendChild(live);
    }
    name.appendChild(document.createTextNode(p.name || p.bundle_id));
    const count = document.createElement("span");
    count.className = "n";
    count.textContent = p.diffs.length ? p.diffs.length + " 项" : "无差异";
    name.appendChild(count);

    const bundle = document.createElement("div");
    bundle.className = "ab";
    bundle.textContent = p.bundle_id;

    item.append(name, bundle);
    item.addEventListener("click", () => {
      selectedApp = p.bundle_id;
      renderAppList();
      renderAppDetail();
    });
    list.appendChild(item);
  }
}

/// 动作下拉：与「按键」页共用同一份动作清单。
function actionSelect(currentId, onPick) {
  const sel = document.createElement("select");
  for (const a of actionsCache) {
    const opt = document.createElement("option");
    opt.value = a.id;
    opt.textContent = a.label;
    if (a.id === currentId) opt.selected = true;
    sel.appendChild(opt);
  }
  sel.addEventListener("change", (e) => onPick(e.target.value));
  return sel;
}

async function setAppBinding(profile, buttonId, actionId) {
  await invoke("set_app_binding", {
    bundleId: profile.bundle_id,
    name: profile.name,
    buttonId,
    actionId,
  });
  await refreshApps();
}

function renderAppDetail() {
  const host = $("#app-detail");
  host.innerHTML = "";
  const p = appProfiles.find((x) => x.bundle_id === selectedApp);
  if (!p) {
    const card = document.createElement("div");
    card.className = "card app-empty";
    card.textContent = "先在上面添加一个应用，然后在这里只写下它与全局映射的差异。";
    host.appendChild(card);
    return;
  }

  // 头部：应用名 / bundle id / 启停 / 移除
  const head = document.createElement("div");
  head.className = "card";
  const row = document.createElement("div");
  row.className = "app-head";
  const label = document.createElement("div");
  label.className = "label";
  const title = document.createElement("div");
  title.className = "title";
  title.textContent = p.name || p.bundle_id;
  const bundle = document.createElement("div");
  bundle.className = "bundle";
  bundle.textContent = p.bundle_id + (p.active ? " · 前台生效中" : "");
  label.append(title, bundle);

  const actions = document.createElement("div");
  actions.className = "app-head-actions";
  const toggle = document.createElement("input");
  toggle.type = "checkbox";
  toggle.checked = p.enabled;
  toggle.title = "启用这一层";
  toggle.addEventListener("change", async () => {
    await invoke("set_app_profile_enabled", { bundleId: p.bundle_id, enabled: toggle.checked });
    await refreshApps();
  });
  const remove = document.createElement("button");
  remove.className = "ghost sm";
  remove.textContent = "移除";
  remove.addEventListener("click", async () => {
    await invoke("remove_app_profile", { bundleId: p.bundle_id });
    selectedApp = null;
    await refreshApps();
  });
  actions.append(toggle, remove);
  row.append(label, actions);
  head.appendChild(row);
  host.appendChild(head);

  // 差异表：按键 | 全局动作 → 应用动作 | 清除
  const card = document.createElement("div");
  card.className = "card map-table diff-table";
  if (p.diffs.length) {
    const table = document.createElement("table");
    const tbody = document.createElement("tbody");
    for (const d of p.diffs) {
      const tr = document.createElement("tr");
      tr.className = "maprow";
      const key = document.createElement("td");
      key.className = "k";
      key.textContent = d.button_label;
      const from = document.createElement("td");
      from.className = "from";
      from.textContent = d.base_action_label;
      from.title = "全局映射";
      const arrow = document.createElement("td");
      arrow.className = "arrow";
      arrow.textContent = "→";
      const to = document.createElement("td");
      to.className = "to";
      to.appendChild(actionSelect(d.action_id, (id) => setAppBinding(p, d.button_id, id)));
      const del = document.createElement("td");
      del.className = "del";
      const clear = document.createElement("button");
      clear.className = "iconbtn";
      clear.textContent = "✕";
      clear.title = "清除覆盖，这个键回到全局";
      clear.addEventListener("click", () => setAppBinding(p, d.button_id, null));
      del.appendChild(clear);
      tr.append(key, from, arrow, to, del);
      tbody.appendChild(tr);
    }
    table.appendChild(tbody);
    card.appendChild(table);
  } else {
    const empty = document.createElement("div");
    empty.className = "app-empty";
    empty.textContent = "与全局映射完全一致。下面挑一个键改动作，它就会出现在这里。";
    card.appendChild(empty);
  }
  host.appendChild(card);

  const rest = baseButtons.length - p.diffs.length;
  const note = document.createElement("div");
  note.className = "hint";
  note.textContent = `其余 ${rest} 个键沿用全局映射——全局改了，这里跟着改。`;
  host.appendChild(note);

  // 加一条覆盖：只列还没被覆盖的键
  const available = baseButtons.filter((b) => !p.diffs.some((d) => d.button_id === b.id));
  if (available.length) {
    const adder = document.createElement("div");
    adder.className = "card add-override";
    const buttonSel = document.createElement("select");
    const ph = document.createElement("option");
    ph.value = "";
    ph.textContent = "— 选择按键 —";
    buttonSel.appendChild(ph);
    for (const b of available) {
      const opt = document.createElement("option");
      opt.value = b.id;
      opt.textContent = b.label;
      buttonSel.appendChild(opt);
    }
    // 动作下拉预置为该键的全局动作，改成别的才算差异。
    const actionSel = actionSelect(null, () => {});
    const sync = () => {
      const b = baseButtons.find((x) => x.id === buttonSel.value);
      if (b) actionSel.value = b.action_id;
    };
    buttonSel.addEventListener("change", sync);
    const add = document.createElement("button");
    add.className = "ghost sm";
    add.textContent = "加覆盖";
    add.addEventListener("click", () => {
      if (!buttonSel.value) return;
      setAppBinding(p, buttonSel.value, actionSel.value);
    });
    adder.append(buttonSel, actionSel, add);
    host.appendChild(adder);
  }
}

/// 候选应用：正在运行、还没有覆盖层的。
async function refreshAppPicker() {
  const apps = await invoke("list_running_apps");
  const sel = $("#app-picker");
  sel.innerHTML = "";
  const ph = document.createElement("option");
  ph.value = "";
  ph.textContent = "— 运行中的应用 —";
  sel.appendChild(ph);
  for (const a of apps) {
    if (a.has_profile) continue;
    const opt = document.createElement("option");
    opt.value = a.bundle_id;
    opt.textContent = a.name;
    sel.appendChild(opt);
  }
}

/// 设置窗被看着的时候前台就是 RCTool 自己，所以提示的是"刚才那个"应用。
async function refreshFrontAppHint() {
  frontApp = await invoke("get_front_app");
  const box = $("#front-app-hint");
  if (!frontApp || frontApp.has_profile) {
    box.hidden = true;
    return;
  }
  $("#front-app-text").textContent = `刚才在前台的是「${frontApp.name}」（${frontApp.bundle_id}）`;
  box.hidden = false;
}

async function addAppProfile(bundleId, name) {
  await invoke("add_app_profile", { bundleId, name });
  selectedApp = bundleId;
  await refreshApps();
}

$("#add-app").addEventListener("click", async () => {
  const manual = $("#app-bundle").value.trim();
  const picker = $("#app-picker");
  const bundleId = manual || picker.value;
  if (!bundleId) return;
  const name = manual ? manual : picker.selectedOptions[0]?.textContent || bundleId;
  $("#app-bundle").value = "";
  picker.value = "";
  await addAppProfile(bundleId, name);
});

$("#add-front-app").addEventListener("click", async () => {
  if (frontApp) await addAppProfile(frontApp.bundle_id, frontApp.name);
});

// 前台应用变了：应用页开着就顺手刷新（"生效中"标记与提示都会变）。
listen("front-app", () => {
  if ($("#page-apps").classList.contains("active")) refreshApps();
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
  $("#hide-dock").checked = cfg.hide_dock_on_close;
  await refreshOutputs();
  await syncRunning();
  applyStatus(
    cfg.running
      ? { kind: "searching", detail: "正在查找遥控器…", streaming: false }
      : { kind: "stopped", detail: "已停止", streaming: false }
  );
}

init();
