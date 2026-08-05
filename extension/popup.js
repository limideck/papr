/**
 * Papr popup.
 *
 * Asks the active tab's content script for its detected feeds, renders them,
 * and wires each "Subscribe in Papr" button to open a `papr://subscribe?url=…`
 * deep link. Opening the deep link hands the feed to the desktop app, which
 * focuses its window and opens the Add-feed dialog prefilled.
 */
"use strict";

/** Render the list of detected feeds. */
function render(feeds) {
  const list = document.getElementById("feeds");
  const empty = document.getElementById("empty");
  const subtitle = document.getElementById("subtitle");
  list.textContent = "";

  if (!feeds || feeds.length === 0) {
    empty.hidden = false;
    subtitle.textContent = "No feeds found";
    return;
  }
  empty.hidden = true;
  subtitle.textContent =
    feeds.length + " feed" + (feeds.length === 1 ? "" : "s") + " on this page";

  for (const feed of feeds) {
    const li = document.createElement("li");

    const meta = document.createElement("div");
    meta.className = "meta";

    const title = document.createElement("div");
    title.className = "title";
    title.textContent = feed.title || feed.feedUrl;
    meta.appendChild(title);

    const url = document.createElement("div");
    url.className = "url";
    url.textContent = feed.feedUrl;
    meta.appendChild(url);

    if (feed.kind && feed.kind !== "declared") {
      const kind = document.createElement("span");
      kind.className = "kind";
      kind.textContent = feed.kind;
      title.appendChild(document.createTextNode("  "));
      title.appendChild(kind);
    }

    const btn = document.createElement("button");
    btn.className = "sub";
    btn.textContent = "Subscribe in Papr";
    btn.addEventListener("click", function () {
      const link = PaprDetect.buildSubscribeLink(feed.feedUrl);
      // Navigating a tab to a custom-scheme URL triggers the OS handler
      // (the Papr desktop app) without leaving a stray http page behind.
      chrome.tabs.create({ url: link, active: false });
      btn.textContent = "Sent ✓";
      btn.disabled = true;
    });

    li.appendChild(meta);
    li.appendChild(btn);
    list.appendChild(li);
  }
}

/** Query the active tab's content script and render the result. */
function init() {
  chrome.tabs.query({ active: true, currentWindow: true }, function (tabs) {
    const tab = tabs && tabs[0];
    if (!tab || !tab.id) {
      render([]);
      return;
    }
    chrome.tabs.sendMessage(tab.id, { type: "get-feeds" }, function (resp) {
      // No content script on this tab (chrome:// pages, etc.).
      if (chrome.runtime.lastError || !resp) {
        render([]);
        return;
      }
      render(resp.feeds);
    });
  });
}

document.addEventListener("DOMContentLoaded", init);
