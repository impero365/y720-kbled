const COLORS = [
  ["#e51012", "Red"],
  ["#fd0154", "Torch red"],
  ["#ff00aa", "Hollywood cerise"],
  ["#ff00fe", "Magenta"],
  ["#aa00ff", "Electric violet"],
  ["#5500fe", "Electric indigo"],
  ["#0000ff", "Blue"],
  ["#0055fe", "Blue ribbon"],
  ["#00aaff", "Azure"],
  ["#00ffff", "Cyan"],
  ["#00ffab", "Spring green"],
  ["#00ff55", "Spring green 2"],
  ["#01ff00", "Green"],
  ["#55ff00", "Bright green"],
  ["#aaff01", "Lime"],
  ["#ffff00", "Yellow"],
  ["#ffaa01", "Orange"],
  ["#ff5306", "International orange"],
  ["#ffffff", "White"],
  ["#000000", "Black"],
];

const BRIGHTNESS = ["Off", "Low", "Medium", "High", "Ultra", "Enough"];
const MODES = [
  ["heartbeat", "Heartbeat"],
  ["breath", "Breath"],
  ["smooth", "Smooth"],
  ["always_on", "Always on"],
  ["wave", "Wave"],
];
const ZONE_NAMES = ["Left", "Center left", "Center right", "Numpad"];

const invoke = (command, args) => window.__TAURI__.core.invoke(command, args);
const listen = (name, handler) => window.__TAURI__.event.listen(name, handler);

let config = null;
let selectedZone = 0;

const profileSelect = document.querySelector("#profile");
const notice = document.querySelector("#notice");
const zones = document.querySelector("#zones");
const colors = document.querySelector("#colors");
const brightness = document.querySelector("#brightness");
const modes = document.querySelector("#modes");
const dialog = document.querySelector("#name-dialog");
const nameInput = document.querySelector("#name-input");
const nameLabel = document.querySelector("#name-label");

function current() {
  return config.profiles[config.selected];
}

function showNotice(text) {
  notice.hidden = !text;
  notice.textContent = text || "";
}

function render() {
  if (!config) return;
  const profile = current();
  profileSelect.replaceChildren();
  config.profiles.forEach((item, index) => {
    const option = document.createElement("option");
    option.value = String(index);
    option.textContent = item.name;
    option.selected = index === config.selected;
    profileSelect.append(option);
  });
  document.querySelector("#delete").disabled = config.profiles.length < 2;

  zones.replaceChildren();
  profile.zones.forEach((zone, index) => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "zone" + (index === 3 ? " numpad" : "") + (index === selectedZone ? " selected" : "");
    const color = COLORS[zone.color][0];
    button.style.borderColor = color;
    button.style.boxShadow = zone.color === 19 ? "none" : `inset 0 0 24px ${color}`;
    button.textContent = `${ZONE_NAMES[index]}\n${COLORS[zone.color][1]}, ${BRIGHTNESS[zone.brightness]}`;
    button.style.whiteSpace = "pre-line";
    button.addEventListener("click", () => {
      selectedZone = index;
      render();
    });
    zones.append(button);
  });

  colors.replaceChildren();
  COLORS.forEach(([hex, name], index) => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "swatch" + (profile.zones[selectedZone].color === index ? " selected" : "");
    button.style.background = hex;
    button.title = name;
    button.setAttribute("aria-label", name);
    button.addEventListener("click", () => {
      profile.zones[selectedZone].color = index;
      send();
    });
    colors.append(button);
  });

  brightness.replaceChildren();
  BRIGHTNESS.forEach((name, index) => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "choice" + (profile.zones[selectedZone].brightness === index ? " selected" : "");
    button.textContent = name;
    button.addEventListener("click", () => {
      profile.zones[selectedZone].brightness = index;
      send();
    });
    brightness.append(button);
  });

  modes.replaceChildren();
  MODES.forEach(([id, name]) => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "choice" + (profile.mode === id ? " selected" : "");
    button.textContent = name;
    button.addEventListener("click", () => {
      profile.mode = id;
      send();
    });
    modes.append(button);
  });
}

async function send() {
  try {
    await invoke("set_config", { config });
  } catch (error) {
    showNotice(String(error));
  }
}

function askName(title, value) {
  nameLabel.textContent = title;
  nameInput.value = value;
  dialog.showModal();
  nameInput.focus();
  return new Promise((resolve) => {
    dialog.addEventListener("close", function onClose() {
      dialog.removeEventListener("close", onClose);
      resolve(dialog.returnValue === "ok" ? nameInput.value.trim() : "");
    });
  });
}

profileSelect.addEventListener("change", () => {
  config.selected = Number(profileSelect.value);
  selectedZone = 0;
  send();
});

document.querySelector("#add").addEventListener("click", async () => {
  const name = await askName("New profile name", `Profile ${config.profiles.length + 1}`);
  if (!name) return;
  const copy = structuredClone(current());
  copy.name = name;
  config.profiles.push(copy);
  config.selected = config.profiles.length - 1;
  send();
});

document.querySelector("#rename").addEventListener("click", async () => {
  const name = await askName("Rename profile", current().name);
  if (!name) return;
  current().name = name;
  send();
});

document.querySelector("#delete").addEventListener("click", () => {
  if (config.profiles.length < 2) return;
  config.profiles.splice(config.selected, 1);
  config.selected = Math.max(0, config.selected - 1);
  send();
});

document.querySelector("#all-brightness").addEventListener("click", () => {
  const level = current().zones[selectedZone].brightness;
  current().zones.forEach((zone) => {
    zone.brightness = level;
  });
  send();
});

function applyPayload(payload) {
  if (!payload) return;
  config = payload;
  render();
}

async function start() {
  await listen("config", (event) => applyPayload(event.payload));
  await listen("notice", (event) => showNotice(event.payload));
  await listen("status", (event) => {
    if (event.payload && event.payload.notice) showNotice(event.payload.notice);
    else if (event.payload && event.payload.connected) showNotice("");
  });
  try {
    applyPayload(await invoke("get_config"));
    const status = await invoke("service_status");
    if (status.notice) showNotice(status.notice);
  } catch (error) {
    showNotice(String(error));
  }
}

start();
