import { useEffect, useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { useQueryClient } from "@tanstack/react-query";
import * as api from "../api";
import { errorText } from "../lib/errors";
import { NO_AUTOCORRECT } from "../lib/inputProps";
import type { FeedSource, FeedSourceScanResult } from "../types";
import Icon from "./Icon";

/** Match server normalize: trim + trailing slash; require http(s). */
function normalizeIndexUrl(raw: string): string {
  const u = raw.trim();
  if (!u) return "";
  try {
    const parsed = new URL(u);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return "";
    if (!parsed.host) return "";
  } catch {
    return "";
  }
  return u.replace(/\/+$/, "") + "/";
}

function parseIndexUrlLines(text: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const line of text.split(/\r?\n/)) {
    const u = normalizeIndexUrl(line);
    if (!u || seen.has(u)) continue;
    seen.add(u);
    out.push(u);
  }
  return out;
}

const EXAMPLES = [
  "https://bryan.yzcw.dpdns.org/foreignpolicy/",
  "https://bryan.yzcw.dpdns.org/ft/",
  "https://bryan.yzcw.dpdns.org/wsj/",
];

export default function FeedSourcesAdmin() {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [sources, setSources] = useState<FeedSource[]>([]);
  const [text, setText] = useState("");
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [scan, setScan] = useState<FeedSourceScanResult | null>(null);
  const [scanningId, setScanningId] = useState<number | "all" | null>(null);

  const reload = async () => {
    try {
      setSources(await api.listFeedSources());
    } catch (e) {
      setError(errorText(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void reload();
  }, []);

  const onAdd = async (e: FormEvent) => {
    e.preventDefault();
    const urls = parseIndexUrlLines(text);
    if (urls.length === 0) {
      setError(t("feedSources.invalidUrl"));
      return;
    }
    setBusy(true);
    setError("");
    setScan(null);
    try {
      for (const url of urls) {
        await api.addFeedSource(url);
      }
      setText("");
      await reload();
      qc.invalidateQueries({ queryKey: ["folders"] });
      qc.invalidateQueries({ queryKey: ["feeds"] });
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  };

  const onScan = async (id?: number) => {
    setScanningId(id ?? "all");
    setError("");
    setScan(null);
    try {
      const result = await api.scanFeedSources(id);
      setScan(result);
      await reload();
      qc.invalidateQueries({ queryKey: ["folders"] });
      qc.invalidateQueries({ queryKey: ["feeds"] });
      if ((result.addedCount ?? 0) > 0) {
        qc.invalidateQueries({ queryKey: ["counts"] });
      }
    } catch (err) {
      setError(errorText(err));
    } finally {
      setScanningId(null);
    }
  };

  const onRemove = async (id: number) => {
    if (!window.confirm(t("feedSources.confirmRemove"))) return;
    setBusy(true);
    setError("");
    try {
      await api.removeFeedSource(id);
      await reload();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  };

  const urls = parseIndexUrlLines(text);

  return (
    <div className="feed-sources">
      <p className="feed-sources-hint">{t("feedSources.hint")}</p>

      <form className="feed-sources-form" onSubmit={onAdd}>
        <textarea
          value={text}
          onChange={(e) => setText(e.target.value)}
          rows={3}
          placeholder={EXAMPLES.join("\n")}
          {...NO_AUTOCORRECT}
          disabled={busy}
        />
        <div className="feed-sources-form-actions">
          <span className="feed-sources-count">
            {urls.length > 0
              ? t("feedSources.urlCount", { count: urls.length })
              : t("feedSources.urlHint")}
          </span>
          <button
            type="submit"
            className="s-btn primary"
            disabled={busy || urls.length === 0}
          >
            <Icon name="plus" size={13} />
            {t("feedSources.add")}
          </button>
        </div>
      </form>

      {error && (
        <div className="feed-sources-error" role="alert">
          {error}
        </div>
      )}

      <div className="feed-sources-toolbar">
        <h3 className="settings-group-title">{t("feedSources.listTitle")}</h3>
        <button
          type="button"
          className="s-btn"
          disabled={busy || scanningId != null || sources.length === 0}
          onClick={() => onScan()}
        >
          <Icon name="refresh" size={13} />
          {scanningId === "all"
            ? t("feedSources.scanning")
            : t("feedSources.scanAll")}
        </button>
      </div>

      {loading ? (
        <div className="feed-sources-empty">{t("common.loading")}</div>
      ) : sources.length === 0 ? (
        <div className="feed-sources-empty">{t("feedSources.empty")}</div>
      ) : (
        <ul className="feed-sources-list">
          {sources.map((s) => (
            <li key={s.id}>
              <div className="feed-sources-item-main">
                <a href={s.baseUrl} target="_blank" rel="noopener noreferrer">
                  {s.baseUrl}
                </a>
                <span className="feed-sources-meta">
                  {s.folderName
                    ? `${t("feedSources.folder", { name: s.folderName })} · `
                    : ""}
                  {t("feedSources.feedCount", { count: s.feedCount })}
                  {s.lastCheckedAt
                    ? ` · ${new Date(s.lastCheckedAt).toLocaleString()}`
                    : ""}
                </span>
              </div>
              <div className="feed-sources-item-actions">
                <button
                  type="button"
                  className="s-btn"
                  disabled={busy || scanningId != null}
                  onClick={() => onScan(s.id)}
                  title={t("feedSources.scan")}
                >
                  <Icon name="refresh" size={12} />
                </button>
                <button
                  type="button"
                  className="s-btn"
                  disabled={busy}
                  onClick={() => onRemove(s.id)}
                  title={t("common.remove")}
                >
                  <Icon name="trash" size={12} />
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}

      {scan && (
        <div className="feed-sources-scan">
          <p>
            {t("feedSources.scanResult", {
              added: scan.addedCount ?? 0,
              skipped: scan.skipped ?? 0,
            })}
          </p>
          {(scan.stale?.length ?? 0) > 0 && (
            <p className="feed-sources-stale">
              {t("feedSources.staleCount", { count: scan.stale!.length })}
            </p>
          )}
        </div>
      )}
    </div>
  );
}
