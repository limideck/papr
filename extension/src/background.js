/**
 * Papr background service worker (Manifest V3).
 *
 * Its only job is the toolbar badge: when a content script reports that the
 * current page has feeds, it shows the feed count on the action icon for that
 * tab. The badge is per-tab, so switching tabs shows the right state.
 */
"use strict";

/** Paint (or clear) the badge for one tab. */
function setBadge(tabId, count) {
  const text = count > 0 ? String(count) : "";
  chrome.action.setBadgeText({ tabId: tabId, text: text });
  if (count > 0) {
    chrome.action.setBadgeBackgroundColor({ tabId: tabId, color: "#B5651D" });
    chrome.action.setTitle({
      tabId: tabId,
      title: count + " feed" + (count === 1 ? "" : "s") + " found — open Papr",
    });
  } else {
    chrome.action.setTitle({ tabId: tabId, title: "Papr — no feeds on this page" });
  }
}

// Content scripts report their feed count here.
chrome.runtime.onMessage.addListener(function (msg, sender) {
  if (msg && msg.type === "feeds-detected" && sender.tab) {
    setBadge(sender.tab.id, msg.count || 0);
  }
});

// Clear the badge when a tab navigates to a new URL — the new page re-reports.
// Gated on `changeInfo.url` so same-document status flips don't flicker the
// badge between the old count and 0.
chrome.tabs.onUpdated.addListener(function (tabId, changeInfo) {
  if (changeInfo.status === "loading" && changeInfo.url) {
    setBadge(tabId, 0);
  }
});
