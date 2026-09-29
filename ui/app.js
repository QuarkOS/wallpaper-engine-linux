const statusEl = document.querySelector("#status");
const libraryEl = document.querySelector("#library");
const errorEl = document.querySelector("#error");

const EMPTY_LIBRARY = "No workshop items found.";

function hideError() {
  errorEl.hidden = true;
  errorEl.textContent = "";
}

function showPlayError(id) {
  errorEl.hidden = false;
  errorEl.textContent = "Could not play " + id + ".";
}

function emptyLibrary() {
  const paragraph = document.createElement("p");
  paragraph.className = "empty";
  paragraph.textContent = EMPTY_LIBRARY;
  libraryEl.replaceChildren(paragraph);
}

function playableType(type) {
  return type === "video" || type === "web";
}

function renderCard(item) {
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
  if (!playableType(item.type)) {
    button.disabled = true;
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

  if (!payload.found || !payload.path) {
    statusEl.textContent = "No workshop library found.";
    emptyLibrary();
    return;
  }

  statusEl.textContent = "Library found at " + payload.path;
  const items = Array.isArray(payload.items) ? payload.items : [];
  if (items.length === 0) {
    emptyLibrary();
    return;
  }
  libraryEl.replaceChildren(...items.map(renderCard));
}

loadLibrary();
