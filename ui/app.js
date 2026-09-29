const statusEl = document.querySelector("#status");
const libraryEl = document.querySelector("#library");
const errorEl = document.querySelector("#error");
const rescanEl = document.querySelector("#rescan");
const addFolderEl = document.querySelector("#add-folder");
const extraPathEl = document.querySelector("#extra-path");
const autostartEl = document.querySelector("#autostart");
const autostartPromptEl = document.querySelector("#autostart-prompt");
const autostartYesEl = document.querySelector("#autostart-yes");
const autostartNoEl = document.querySelector("#autostart-no");

const EMPTY_LIBRARY = "No workshop items found.";

function hideError() {
  errorEl.hidden = true;
  errorEl.textContent = "";
}

function showError(message) {
  errorEl.hidden = false;
  errorEl.textContent = message;
}

function showPlayError(id) {
  showError("Could not play " + id + ".");
}

function emptyLibrary() {
  const paragraph = document.createElement("p");
  paragraph.className = "empty";
  paragraph.textContent = EMPTY_LIBRARY;
  libraryEl.replaceChildren(paragraph);
}

function playableType(type, liveScene) {
  if (type === "video" || type === "web") {
    return true;
  }
  return type === "scene" && liveScene === true;
}

function renderPreview(item) {
  const preview = item.preview;
  if (!preview || typeof preview.url !== "string" || preview.url === "") {
    return null;
  }
  let media = null;
  if (preview.kind === "video") {
    media = document.createElement("video");
    media.muted = true;
    media.loop = true;
    media.autoplay = true;
    media.playsInline = true;
  } else if (preview.kind === "image") {
    media = document.createElement("img");
    media.alt = "";
  }
  if (!media) {
    return null;
  }
  media.className = "preview";
  media.addEventListener("error", function () {
    media.remove();
  });
  media.src = preview.url;
  return media;
}

function renderCard(item, liveScene) {
  const card = document.createElement("article");
  card.className = "wallpaper-card";

  const title = document.createElement("h2");
  title.className = "title";
  title.textContent = item.title || item.id;

  const meta = document.createElement("p");
  meta.className = "meta";
  meta.textContent = item.id + " " + item.type;

  const button = document.createElement("button");
  button.className = "play";
  button.type = "button";
  button.dataset.id = item.id;
  button.textContent = "Play";
  if (!playableType(item.type, liveScene)) {
    button.disabled = true;
  }

  const preview = renderPreview(item);
  if (preview) {
    card.append(preview);
  }
  card.append(title, meta, button);
  return card;
}

async function play(id, button) {
  hideError();
  button.disabled = true;
  try {
    const response = await fetch("/api/play", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ id: id }),
    });
    if (!response.ok) {
      showPlayError(id);
      return;
    }
    const payload = await response.json();
    if (!payload.ok) {
      showPlayError(id);
    }
  } catch (error) {
    showPlayError(id);
  } finally {
    button.disabled = false;
  }
}

libraryEl.addEventListener("click", (event) => {
  const button = event.target.closest("button.play");
  if (!button || button.disabled || !button.dataset.id) {
    return;
  }
  play(button.dataset.id, button);
});

function libraryStatus(payload) {
  const steam = payload.found && payload.path ? payload.path : "";
  const extra = typeof payload.extraLibrary === "string" ? payload.extraLibrary : "";
  if (steam && extra) {
    return "Library found at " + steam + " and " + extra;
  }
  if (steam) {
    return "Library found at " + steam;
  }
  if (extra) {
    return "Library found at " + extra;
  }
  return "";
}

async function loadLibrary() {
  hideError();
  let response;
  try {
    response = await fetch("/api/library");
  } catch (error) {
    statusEl.textContent = "No workshop library found.";
    emptyLibrary();
    return;
  }
  if (!response.ok) {
    statusEl.textContent = "No workshop library found.";
    emptyLibrary();
    return;
  }

  let payload;
  try {
    payload = await response.json();
  } catch (error) {
    statusEl.textContent = "No workshop library found.";
    emptyLibrary();
    return;
  }

  const status = libraryStatus(payload);
  if (!status) {
    statusEl.textContent = "No workshop library found.";
    emptyLibrary();
    return;
  }

  statusEl.textContent = status;
  const items = Array.isArray(payload.items) ? payload.items : [];
  if (items.length === 0) {
    emptyLibrary();
    return;
  }
  const liveScene = payload.liveScene === true;
  libraryEl.replaceChildren(...items.map((item) => renderCard(item, liveScene)));
}

async function addFolder(path) {
  hideError();
  let response;
  try {
    response = await fetch("/api/library/extra", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ path: path }),
    });
  } catch (error) {
    showError("Could not add that folder.");
    return;
  }
  let payload = {};
  try {
    payload = await response.json();
  } catch (error) {
    payload = {};
  }
  if (!response.ok || !payload.ok) {
    showError(payload.error || "Could not add that folder.");
    return;
  }
  extraPathEl.value = "";
  await loadLibrary();
}

async function saveAutostart(enabled) {
  hideError();
  let response;
  try {
    response = await fetch("/api/autostart", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ enabled: enabled }),
    });
  } catch (error) {
    showError("Could not save the login setting.");
    return;
  }
  if (!response.ok) {
    showError("Could not save the login setting.");
    return;
  }
  autostartEl.checked = enabled;
  autostartPromptEl.hidden = true;
}

async function loadSettings() {
  let response;
  try {
    response = await fetch("/api/settings");
  } catch (error) {
    return;
  }
  if (!response.ok) {
    return;
  }
  let settings;
  try {
    settings = await response.json();
  } catch (error) {
    return;
  }
  autostartEl.checked = !!settings.autostart;
  if (!settings.autostartAsked) {
    autostartPromptEl.hidden = false;
  }
}

rescanEl.addEventListener("click", () => {
  loadLibrary();
});

addFolderEl.addEventListener("submit", (event) => {
  event.preventDefault();
  const path = extraPathEl.value.trim();
  if (!path) {
    return;
  }
  addFolder(path);
});

autostartEl.addEventListener("change", () => {
  saveAutostart(autostartEl.checked);
});

autostartYesEl.addEventListener("click", () => {
  saveAutostart(true);
});

autostartNoEl.addEventListener("click", () => {
  saveAutostart(false);
});

loadSettings();
loadLibrary();
