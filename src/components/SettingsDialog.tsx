import { useQuery, useQueryClient } from "@tanstack/react-query";
import { cloneElement, isValidElement, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import * as api from "../api";
import { useAuth } from "../auth";
import { useUi, READER_BOUNDS, type OpenMode } from "../store";
import { useArticleActions } from "../hooks/articleActions";
import { useFocusTrap } from "../hooks/useFocusTrap";
import { LANGUAGES, setLanguage, type Language } from "../i18n";
import { feedHost } from "../lib/feedMeta";
import { modKey, modCombo } from "../lib/platform";
import { reportError } from "../toast";
import { downloadFile } from "../lib/download";
import { NO_AUTOCORRECT } from "../lib/inputProps";
import { renderMarkdown } from "../lib/markdown";
import type { Feed, Rule, RuleAction, RuleField, RulePreview, Tag, TagAlias } from "../types";
import { tagColor, TAG_PALETTE } from "../lib/tagColors";
import Icon, { type IconName } from "./Icon";
import ConfirmDialog from "./ConfirmDialog";
import PromptDialog from "./PromptDialog";
import FeedAvatar from "./FeedAvatar";
import FeedSourcesAdmin from "./FeedSourcesAdmin";
import WordCloudConfigAdmin from "./WordCloudConfigAdmin";
// Bundled at build time so web + desktop work without fetching GitHub / docs URL.
import userGuideMd from "../../docs/user-search-and-tags.md?raw";

interface Props {
  onClose: () => void;
  onToast: (msg: string) => void;
  initialSection?: string;
  onAddFeed: () => void;
}

// `labelKey` holds an i18n key — resolved with t() at render time. Nav icons
// stay monochrome (quiet ink, accent only on the active row) — one accent,
// used rarely, never a decorative rainbow of per-section colours.
const BASE_SECTIONS: { id: string; labelKey: string; icon: IconName }[] = [
  { id: "general", labelKey: "settings.nav.general", icon: "settings" },
  { id: "appearance", labelKey: "settings.nav.appearance", icon: "globe" },
  { id: "reading", labelKey: "settings.nav.reading", icon: "eye" },
  { id: "subscriptions", labelKey: "settings.nav.subscriptions", icon: "rss" },
  { id: "filters", labelKey: "settings.nav.filters", icon: "mute" },
  { id: "sync", labelKey: "settings.nav.sync", icon: "refresh" },
  { id: "shortcuts", labelKey: "settings.nav.shortcuts", icon: "command" },
  { id: "notifications", labelKey: "settings.nav.notifications", icon: "inbox" },
  { id: "advanced", labelKey: "settings.nav.advanced", icon: "sort" },
  { id: "about", labelKey: "settings.nav.about", icon: "sparkle" },
];

const ADMIN_SECTIONS: { id: string; labelKey: string; icon: IconName }[] = [
  { id: "feedSources", labelKey: "settings.nav.feedSources", icon: "globe" },
  { id: "wordcloud", labelKey: "settings.nav.wordcloud", icon: "sparkle" },
  { id: "users", labelKey: "settings.nav.users", icon: "star" },
  { id: "autoTag", labelKey: "settings.nav.autoTag", icon: "tag" },
  { id: "stats", labelKey: "settings.nav.stats", icon: "list" },
];

function buildSettingsSections(isAdmin: boolean) {
  if (!isAdmin) {
    return BASE_SECTIONS.filter((s) => s.id !== "filters");
  }
  // Insert admin-only sections after Subscriptions.
  const out = [...BASE_SECTIONS];
  const insertAt = out.findIndex((s) => s.id === "filters");
  out.splice(insertAt, 0, ...ADMIN_SECTIONS);
  return out;
}

function useAppVersion(): string {
  return typeof __APP_VERSION__ === "string" ? __APP_VERSION__ : "";
}

export default function SettingsDialog({
  onClose,
  onToast,
  initialSection,
  onAddFeed,
}: Props) {
  const { t } = useTranslation();
  const { user, logout } = useAuth();
  const isAdmin = !!user?.isAdmin;
  const [section, setSection] = useState(initialSection ?? "general");
  const feeds = useQuery({ queryKey: ["feeds"], queryFn: api.listFeeds });
  const windowRef = useRef<HTMLDivElement>(null);
  useFocusTrap(windowRef);

  const sections = buildSettingsSections(isAdmin);

  useEffect(() => {
    // If a non-admin lands on an admin-only section (deep link / stale state),
    // fall back to General so the content pane isn't blank.
    if (!sections.some((s) => s.id === section)) {
      setSection("general");
    }
  }, [sections, section]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);

  const cur = sections.find((s) => s.id === section) ?? sections[0]!;
  const feedCount = feeds.data?.length ?? 0;

  const subs: Record<string, string> = {
    general: t("settings.sub.general"),
    appearance: t("settings.sub.appearance"),
    reading: t("settings.sub.reading"),
    subscriptions: t("settings.sub.subscriptions", { count: feedCount }),
    feedSources: t("settings.sub.feedSources"),
    wordcloud: t("settings.sub.wordcloud"),
    users: t("settings.sub.users"),
    autoTag: t("settings.sub.autoTag"),
    stats: t("settings.sub.stats"),
    filters: t("settings.sub.filters"),
    sync: t("settings.sub.sync"),
    shortcuts: t("settings.sub.shortcuts"),
    notifications: t("settings.sub.notifications"),
    advanced: t("settings.sub.advanced"),
    about: t("settings.sub.about"),
  };

  return (
    <div className="settings-backdrop" onClick={onClose}>
      <div
        className="settings-window"
        ref={windowRef}
        role="dialog"
        aria-modal="true"
        aria-label={t("settings.title")}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="settings-sidebar">
          <div className="settings-sidebar-title">
            {t("settings.title")}
            <span className="badge">{modCombo(",")}</span>
          </div>
          {sections.map((s) => (
            <div
              key={s.id}
              className={`settings-nav-item ${section === s.id ? "active" : ""}`}
              onClick={() => setSection(s.id)}
            >
              <span className="nav-ico">
                <Icon name={s.icon} size={15} />
              </span>
              {t(s.labelKey)}
            </div>
          ))}
          <div className="settings-nav-spacer" />
          {user && (
            <div className="settings-account">
              <span className="settings-account-name" title={user.username}>
                {user.username}
                {user.isAdmin ? ` · ${t("login.admin")}` : ""}
              </span>
              <button
                type="button"
                className="s-btn"
                onClick={() => void logout()}
              >
                {t("login.signOut")}
              </button>
            </div>
          )}
          {/* <div className="settings-version">
            Papr{version && ` ${version}`}
          </div> */}
        </div>

        <div className="settings-content">
          <div className="settings-header">
            <h2>{t(cur.labelKey)}</h2>
            <span className="sub">{subs[section]}</span>
          </div>
          <button
            className="settings-close"
            onClick={onClose}
            title={t("settings.closeTitle")}
          >
            <Icon name="x" size={15} />
          </button>

          <div className="settings-scroll">
            {section === "general" && (
              <GeneralSection isAdmin={isAdmin} onToast={onToast} />
            )}
            {section === "appearance" && <AppearanceSection />}
            {section === "reading" && <ReadingSection />}
            {section === "subscriptions" && (
              <SubscriptionsSection
                feeds={feeds.data ?? []}
                onToast={onToast}
                onAddFeed={onAddFeed}
                isAdmin={isAdmin}
              />
            )}
            {section === "feedSources" && isAdmin && <FeedSourcesAdmin />}
            {section === "wordcloud" && isAdmin && <WordCloudConfigAdmin />}
            {section === "users" && isAdmin && (
              <UsersSection onToast={onToast} />
            )}
            {section === "autoTag" && isAdmin && (
              <AutoTagSection onToast={onToast} />
            )}
            {section === "stats" && isAdmin && <StatsSection />}
            {section === "filters" && isAdmin && (
              <FiltersSection feeds={feeds.data ?? []} onToast={onToast} />
            )}
            {section === "sync" && <SyncSection onToast={onToast} />}
            {section === "shortcuts" && <ShortcutsSection />}
            {section === "notifications" && <NotificationsSection />}
            {section === "advanced" && (
              <AdvancedSection onToast={onToast} isAdmin={isAdmin} />
            )}
            {section === "about" && <AboutSection />}
          </div>
        </div>
      </div>
    </div>
  );
}

/* ── row helpers ─────────────────────────────────────────── */
function Row({
  label,
  desc,
  children,
}: {
  label: string;
  desc?: string;
  children: React.ReactNode;
}) {
  // Name the row's control with the row label so screen readers don't just
  // announce a bare "checkbox" / "slider" / "combobox". The control
  // components forward the injected aria-label to their element.
  const control = isValidElement(children)
    ? cloneElement(children as React.ReactElement<{ "aria-label"?: string }>, {
        "aria-label": label,
      })
    : children;
  return (
    <div className="settings-row">
      <div className="settings-row-text">
        <div className="settings-row-label">{label}</div>
        {desc && <div className="settings-row-desc">{desc}</div>}
      </div>
      <div className="settings-row-control">{control}</div>
    </div>
  );
}

function Toggle({
  checked,
  onChange,
  "aria-label": ariaLabel,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  "aria-label"?: string;
}) {
  return (
    <input
      type="checkbox"
      className="s-toggle"
      checked={checked}
      aria-label={ariaLabel}
      onChange={(e) => onChange(e.target.checked)}
    />
  );
}

function Select<T extends string>({
  value,
  options,
  onChange,
  "aria-label": ariaLabel,
}: {
  value: T;
  options: { value: T; label: string }[];
  onChange: (v: T) => void;
  "aria-label"?: string;
}) {
  return (
    <select
      className="s-select"
      value={value}
      aria-label={ariaLabel}
      onChange={(e) => onChange(e.target.value as T)}
    >
      {options.map((o) => (
        <option key={o.value} value={o.value}>
          {o.label}
        </option>
      ))}
    </select>
  );
}

function Segmented<T extends string>({
  value,
  options,
  onChange,
  "aria-label": ariaLabel,
}: {
  value: T;
  options: { value: T; label: string }[];
  onChange: (v: T) => void;
  "aria-label"?: string;
}) {
  return (
    <div className="s-seg" role="group" aria-label={ariaLabel}>
      {options.map((o) => (
        <button
          key={o.value}
          className={value === o.value ? "on" : ""}
          aria-pressed={value === o.value}
          onClick={() => onChange(o.value)}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

/** The keys that actually move an `<input type="range">`. `onKeyUp` fires for
 *  every key release while the slider is focused — Tab (which merely lands or
 *  leaves focus), Shift, the modifier keys — so committing on a bare keyup
 *  would run `onCommit` for a key that never changed the value. For the
 *  network-timeout slider that side effect is a full HTTP-client rebuild, so a
 *  user simply Tab-navigating through Settings would trigger one. Restrict the
 *  commit to releases of a value-changing key. */
const SLIDER_KEYS = new Set([
  "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown",
  "Home", "End", "PageUp", "PageDown",
]);

function Slider({
  value,
  min,
  max,
  step = 1,
  unit = "",
  onChange,
  onCommit,
  "aria-label": ariaLabel,
}: {
  value: number;
  min: number;
  max: number;
  step?: number;
  unit?: string;
  /** Fires on every drag tick — for cheap, live updates (e.g. reader preview). */
  onChange?: (v: number) => void;
  /** Fires once the drag/keypress settles — for costly side effects (a backend
   *  write, an HTTP-client rebuild) that must not run ~20× across one drag. */
  onCommit?: (v: number) => void;
  "aria-label"?: string;
}) {
  const [draft, setDraft] = useState(value);
  // Follow external changes (async settings load, reset) when not mid-drag.
  useEffect(() => setDraft(value), [value]);
  return (
    <>
      <input
        type="range"
        className="s-slider"
        min={min}
        max={max}
        step={step}
        value={draft}
        aria-label={ariaLabel}
        aria-valuetext={`${draft}${unit}`}
        onChange={(e) => {
          const v = Number(e.target.value);
          setDraft(v);
          onChange?.(v);
        }}
        onPointerUp={(e) =>
          onCommit?.(Number((e.target as HTMLInputElement).value))
        }
        onKeyUp={(e) => {
          // Only a key that can move the slider commits — a bare keyup from
          // Tab / Shift / a modifier never changed the value.
          if (SLIDER_KEYS.has(e.key)) {
            onCommit?.(Number((e.target as HTMLInputElement).value));
          }
        }}
      />
      <span className="s-value">
        {draft}
        {unit}
      </span>
    </>
  );
}

/** A toggle row backed by a persisted backend setting ("1" / "0"). */
function SettingFlag({
  settingKey,
  label,
  desc,
  fallback = false,
  onChanged,
}: {
  settingKey: string;
  label: string;
  desc?: string;
  fallback?: boolean;
  onChanged?: (v: boolean) => void;
}) {
  const [val, setVal] = useState(fallback);
  useEffect(() => {
    api
      .getSetting(settingKey)
      .then((v) => {
        if (v != null && v !== "") setVal(v === "1");
      })
      .catch(() => {});
  }, [settingKey]);
  const change = (v: boolean) => {
    setVal(v);
    api.setSetting(settingKey, v ? "1" : "0").catch(() => {});
    onChanged?.(v);
  };
  return (
    <Row label={label} desc={desc}>
      <Toggle checked={val} onChange={change} />
    </Row>
  );
}

/** Bytes → human-readable size. */
function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

/* ── general ─────────────────────────────────────────────── */
// Auto-refresh "off" is stored as a year-long interval — the only lever the
// backend scheduler exposes (it reads `refresh_interval_min`, minimum 5).
const OFF_INTERVAL = 525600;

// A persisted numeric setting, coerced into the range its `<Slider>` accepts.
// Settings live in the backend DB and are normally written numeric, but a
// stale value from an older build with different slider limits — or a corrupt
// non-numeric value — would otherwise flow straight into a `<Slider>`: an
// out-of-range value pins the thumb at the limit while the readout shows a
// contradicting number, and a NaN renders the value as a literal "NaN". This
// mirrors `store.ts`'s `ls.num`, which validates the localStorage-backed
// reader sliders for exactly the same reason.
function clampSetting(raw: string | null, fallback: number, min: number, max: number): number {
  if (raw == null || raw === "") return fallback;
  const n = Number(raw);
  if (!Number.isFinite(n)) return fallback;
  return Math.min(max, Math.max(min, n));
}

/* ── change own password (any logged-in user) ─────────────── */
function ChangePasswordGroup({ onToast }: { onToast: (m: string) => void }) {
  const { t } = useTranslation();
  const [oldPassword, setOldPassword] = useState("");
  const [newPassword, setNewPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [busy, setBusy] = useState(false);

  const canSubmit =
    oldPassword.length > 0 &&
    newPassword.length >= 6 &&
    newPassword === confirmPassword &&
    !busy;

  const submit = async () => {
    if (!canSubmit) return;
    if (newPassword !== confirmPassword) {
      onToast(t("settings.general.passwordMismatch"));
      return;
    }
    if (newPassword.length < 6) {
      onToast(t("error.passwordTooShort"));
      return;
    }
    setBusy(true);
    try {
      await api.changePassword(oldPassword, newPassword);
      setOldPassword("");
      setNewPassword("");
      setConfirmPassword("");
      onToast(t("settings.general.passwordChanged"));
    } catch (e) {
      reportError(e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="settings-group">
      <h3 className="settings-group-title">{t("settings.general.account")}</h3>
      <p className="settings-group-desc">{t("settings.general.changePasswordDesc")}</p>
      <Row label={t("settings.general.currentPassword")}>
        <input
          className="s-text-input"
          type="password"
          value={oldPassword}
          onChange={(e) => setOldPassword(e.target.value)}
          autoComplete="current-password"
        />
      </Row>
      <Row
        label={t("settings.general.newPassword")}
        desc={t("settings.general.newPasswordDesc")}
      >
        <input
          className="s-text-input"
          type="password"
          value={newPassword}
          onChange={(e) => setNewPassword(e.target.value)}
          autoComplete="new-password"
        />
      </Row>
      <Row label={t("settings.general.confirmPassword")}>
        <input
          className="s-text-input"
          type="password"
          value={confirmPassword}
          onChange={(e) => setConfirmPassword(e.target.value)}
          autoComplete="new-password"
          onKeyDown={(e) => {
            if (e.key === "Enter" && canSubmit) void submit();
          }}
        />
      </Row>
      <div style={{ display: "flex", justifyContent: "flex-end", padding: "8px 0" }}>
        <button
          className="s-btn primary"
          type="button"
          disabled={!canSubmit}
          onClick={() => void submit()}
        >
          {t("settings.general.savePassword")}
        </button>
      </div>
    </div>
  );
}

function GeneralSection({
  isAdmin,
  onToast,
}: {
  isAdmin: boolean;
  onToast: (m: string) => void;
}) {
  const { t } = useTranslation();
  const { user } = useAuth();
  const prefs = useUi((s) => s.prefs);
  const setPref = useUi((s) => s.setPref);
  const [autoRefresh, setAutoRefresh] = useState(true);
  const [refreshMins, setRefreshMins] = useState(30);

  useEffect(() => {
    if (!isAdmin) return;
    api
      .getSetting("refresh_interval_min")
      .then((v) => {
        const n = v ? Number(v) : 30;
        // A finite interval at/above the "off" sentinel means auto-refresh is
        // disabled; anything else is a live interval clamped to the slider's
        // 5–120 range (a stale larger value would otherwise show e.g. "150
        // minutes" with the thumb stuck at 120, and a NaN would read "NaN").
        if (Number.isFinite(n) && n >= 100000) setAutoRefresh(false);
        else {
          setAutoRefresh(true);
          setRefreshMins(clampSetting(v ?? null, 30, 5, 120));
        }
      })
      .catch(() => {});
  }, [isAdmin]);

  const writeInterval = (auto: boolean, mins: number) => {
    api
      .setSetting("refresh_interval_min", auto ? String(mins) : String(OFF_INTERVAL))
      .catch(() => {});
  };

  return (
    <>
      {user && <ChangePasswordGroup onToast={onToast} />}
      {isAdmin && (
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.general.refresh")}</h3>
        <Row
          label={t("settings.general.autoRefresh")}
          desc={t("settings.general.autoRefreshDesc")}
        >
          <Toggle
            checked={autoRefresh}
            onChange={(v) => {
              setAutoRefresh(v);
              writeInterval(v, refreshMins);
            }}
          />
        </Row>
        {autoRefresh && (
          <Row
            label={t("settings.general.refreshInterval")}
            desc={t("settings.general.refreshIntervalDesc")}
          >
            <Slider
              value={refreshMins}
              min={5}
              max={120}
              step={5}
              unit={t("settings.general.minutesUnit")}
              onChange={setRefreshMins}
              onCommit={(m) => writeInterval(true, m)}
            />
          </Row>
        )}
      </div>
      )}
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.general.readBehavior")}</h3>
        <Row label={t("settings.general.markReadOnOpen")}>
          <Toggle
            checked={prefs.markReadOnOpen}
            onChange={(v) => setPref({ markReadOnOpen: v })}
          />
        </Row>
        <Row
          label={t("settings.general.markReadOnScroll")}
          desc={t("settings.general.markReadOnScrollDesc")}
        >
          <Toggle
            checked={prefs.markReadOnScroll}
            onChange={(v) => setPref({ markReadOnScroll: v })}
          />
        </Row>
      </div>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.general.startup")}</h3>
        <Row
          label={t("settings.general.startupView")}
          desc={t("settings.general.startupViewDesc")}
        >
          <Select
            value={prefs.startupView}
            options={[
              { value: "all", label: t("settings.general.startupAll") },
              { value: "unread", label: t("smart.unread") },
              { value: "starred", label: t("smart.starred") },
              { value: "last", label: t("settings.general.startupLast") },
            ]}
            onChange={(v) => setPref({ startupView: v })}
          />
        </Row>
        <Row label={t("settings.general.hideReadOnStartup")}>
          <Toggle
            checked={prefs.hideReadOnStartup}
            onChange={(v) => setPref({ hideReadOnStartup: v })}
          />
        </Row>
      </div>
    </>
  );
}

/* ── appearance ──────────────────────────────────────────── */
function AppearanceSection() {
  const { t, i18n } = useTranslation();
  const palette = useUi((s) => s.palette);
  const setPalette = useUi((s) => s.setPalette);
  const mode = useUi((s) => s.mode);
  const setMode = useUi((s) => s.setMode);
  const density = useUi((s) => s.density);
  const setDensity = useUi((s) => s.setDensity);
  const viewMode = useUi((s) => s.viewMode);
  const setViewMode = useUi((s) => s.setViewMode);
  const prefs = useUi((s) => s.prefs);
  const setPref = useUi((s) => s.setPref);

  return (
    <>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.appearance.language")}</h3>
        <Row
          label={t("settings.appearance.uiLanguage")}
          desc={t("settings.appearance.languageDesc")}
        >
          <Select
            value={i18n.language}
            options={LANGUAGES.map((l) => ({ value: l.code, label: l.label }))}
            onChange={(v) => setLanguage(v as Language)}
          />
        </Row>
      </div>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.appearance.theme")}</h3>
        <Row
          label={t("settings.appearance.palette")}
          desc={t("settings.appearance.paletteDesc")}
        >
          <Segmented
            value={palette}
            options={[
              { value: "paper", label: t("settings.appearance.palettePaper") },
              { value: "frost", label: t("settings.appearance.paletteFrost") },
              { value: "contrast", label: t("settings.appearance.paletteContrast") },
            ]}
            onChange={setPalette}
          />
        </Row>
        <Row
          label={t("settings.appearance.appearance")}
          desc={t("settings.appearance.appearanceDesc")}
        >
          <Segmented
            value={mode}
            options={[
              { value: "light", label: t("settings.appearance.light") },
              { value: "dark", label: t("settings.appearance.dark") },
              { value: "system", label: t("settings.appearance.system") },
            ]}
            onChange={setMode}
          />
        </Row>
      </div>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.appearance.layout")}</h3>
        <Row
          label={t("settings.appearance.density")}
          desc={t("settings.appearance.densityDesc")}
        >
          <Segmented
            value={density}
            options={[
              { value: "compact", label: t("settings.appearance.densityCompact") },
              { value: "cozy", label: t("settings.appearance.densityCozy") },
              { value: "spacious", label: t("settings.appearance.densitySpacious") },
            ]}
            onChange={setDensity}
          />
        </Row>
        <Row label={t("settings.appearance.listStyle")}>
          <Segmented
            value={viewMode}
            options={[
              { value: "list", label: t("settings.appearance.listStyleList") },
              { value: "card", label: t("settings.appearance.listStyleCard") },
            ]}
            onChange={setViewMode}
          />
        </Row>
      </div>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.appearance.details")}</h3>
        <Row label={t("settings.appearance.sidebarCounts")}>
          <Toggle
            checked={prefs.showSidebarCounts}
            onChange={(v) => setPref({ showSidebarCounts: v })}
          />
        </Row>
        <Row
          label={t("settings.appearance.cardThumbs")}
          desc={t("settings.appearance.cardThumbsDesc")}
        >
          <Toggle
            checked={prefs.showCardThumbs}
            onChange={(v) => setPref({ showCardThumbs: v })}
          />
        </Row>
        <Row
          label={t("settings.appearance.reduceMotion")}
          desc={t("settings.appearance.reduceMotionDesc")}
        >
          <Toggle
            checked={prefs.reduceMotion}
            onChange={(v) => setPref({ reduceMotion: v })}
          />
        </Row>
      </div>
    </>
  );
}

/* ── reading ─────────────────────────────────────────────── */
function ReadingSection() {
  const { t } = useTranslation();
  const readerFont = useUi((s) => s.readerFont);
  const setReaderFont = useUi((s) => s.setReaderFont);
  const readerSize = useUi((s) => s.readerSize);
  const readerLeading = useUi((s) => s.readerLeading);
  const readerWidth = useUi((s) => s.readerWidth);
  const setReader = useUi((s) => s.setReader);
  const prefs = useUi((s) => s.prefs);
  const setPref = useUi((s) => s.setPref);
  return (
    <>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.reading.font")}</h3>
        <Row
          label={t("settings.reading.bodyFont")}
          desc={t("settings.reading.bodyFontDesc")}
        >
          <Segmented
            value={readerFont}
            options={[
              { value: "serif", label: t("settings.reading.serif") },
              { value: "sans", label: t("settings.reading.sans") },
              { value: "hyperlegible", label: t("settings.reading.hyperlegible") },
            ]}
            onChange={setReaderFont}
          />
        </Row>
        <Row label={t("settings.reading.fontSize")}>
          <Slider
            value={readerSize}
            min={READER_BOUNDS.size.min}
            max={READER_BOUNDS.size.max}
            unit="px"
            onChange={(v) => setReader({ readerSize: v })}
          />
        </Row>
        <Row label={t("settings.reading.lineHeight")}>
          <Slider
            value={readerLeading}
            min={READER_BOUNDS.leading.min}
            max={READER_BOUNDS.leading.max}
            step={5}
            unit="%"
            onChange={(v) => setReader({ readerLeading: v })}
          />
        </Row>
      </div>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.reading.layout")}</h3>
        <Row label={t("settings.reading.maxWidth")}>
          <Slider
            value={readerWidth}
            min={READER_BOUNDS.width.min}
            max={READER_BOUNDS.width.max}
            step={20}
            unit="px"
            onChange={(v) => setReader({ readerWidth: v })}
          />
        </Row>
        <Row
          label={t("settings.reading.readingTime")}
          desc={t("settings.reading.readingTimeDesc")}
        >
          <Toggle
            checked={prefs.showReadingTime}
            onChange={(v) => setPref({ showReadingTime: v })}
          />
        </Row>
      </div>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.reading.openModeTitle")}</h3>
        <Row
          label={t("settings.reading.defaultOpenMode")}
          desc={t("settings.reading.defaultOpenModeDesc")}
        >
          <Select
            value={prefs.defaultOpenMode}
            options={[
              { value: "reader", label: t("settings.subscriptions.openReader") },
              {
                value: "extracted",
                label: t("settings.subscriptions.openExtracted"),
              },
              { value: "web", label: t("settings.subscriptions.openWeb") },
            ]}
            onChange={(v) => setPref({ defaultOpenMode: v as OpenMode })}
          />
        </Row>
      </div>
    </>
  );
}

/* ── subscriptions ───────────────────────────────────────── */
function SubscriptionsSection({
  feeds,
  onToast,
  onAddFeed,
  isAdmin,
}: {
  feeds: Feed[];
  onToast: (m: string) => void;
  onAddFeed: () => void;
  isAdmin: boolean;
}) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const actions = useArticleActions();
  const [search, setSearch] = useState("");
  const fileRef = useRef<HTMLInputElement>(null);
  const filtered = feeds.filter(
    (f) => !search || f.title.toLowerCase().includes(search.toLowerCase()),
  );

  // Per-feed refresh interval. "default" ⇒ null (follow the global setting),
  // "off" ⇒ the 525600-minute sentinel, otherwise the literal minute count.
  const REFRESH_OFF = 525600;
  const intervalOptions = [
    { value: "default", label: t("settings.subscriptions.refreshDefault") },
    { value: "15", label: t("settings.subscriptions.refresh15m") },
    { value: "30", label: t("settings.subscriptions.refresh30m") },
    { value: "60", label: t("settings.subscriptions.refresh1h") },
    { value: "360", label: t("settings.subscriptions.refresh6h") },
    { value: "720", label: t("settings.subscriptions.refresh12h") },
    { value: "1440", label: t("settings.subscriptions.refresh1d") },
    { value: "off", label: t("settings.subscriptions.refreshOff") },
  ];
  const intervalValue = (m: number | null) =>
    m == null ? "default" : m >= REFRESH_OFF ? "off" : String(m);
  const updateInterval = (f: Feed, v: string) => {
    const minutes = v === "default" ? null : v === "off" ? REFRESH_OFF : Number(v);
    api
      .setFeedRefreshInterval(f.id, minutes)
      .then(() => qc.invalidateQueries({ queryKey: ["feeds"] }))
      .catch((e) => reportError(e));
  };
  // Per-feed open mode (issue #110): how the feed's articles open in the
  // reader pane. "default" ⇒ null (reader view, honouring the global
  // auto-extract preference).
  const openModeOptions = [
    { value: "default", label: t("settings.subscriptions.openDefault") },
    { value: "reader", label: t("settings.subscriptions.openReader") },
    { value: "extracted", label: t("settings.subscriptions.openExtracted") },
    { value: "web", label: t("settings.subscriptions.openWeb") },
  ];
  const updateOpenMode = (f: Feed, v: string) => {
    const mode = v === "default" ? null : (v as "reader" | "extracted" | "web");
    api
      .setFeedOpenMode(f.id, mode)
      .then(() => qc.invalidateQueries({ queryKey: ["feeds"] }))
      .catch((e) => reportError(e));
  };

  const exportOpml = async () => {
    try {
      const xml = await api.exportOpml();
      downloadFile(xml, "subscriptions.opml", "text/xml");
      onToast(t("settings.subscriptions.opmlExported"));
    } catch (e) {
      reportError(e);
    }
  };

  const importOpml = async (file: File) => {
    try {
      const n = await api.importOpml(await file.text());
      await qc.invalidateQueries();
      onToast(t("settings.subscriptions.opmlImported", { count: n }));
    } catch (e) {
      reportError(e);
    }
  };

  const unsubscribe = (f: Feed) =>
    api
      .deleteFeed(f.id)
      .then(() => {
        // Unsubscribing touches only article-bearing caches — unlike OPML
        // import, it needs no full invalidation.
        actions.refreshAfterBulk();
        onToast(t("settings.subscriptions.unsubscribed", { title: f.title }));
      })
      .catch((e) => reportError(e));

  return (
    <>
      <input
        ref={fileRef}
        type="file"
        accept=".opml,.xml"
        style={{ display: "none" }}
        onChange={(e) => {
          const f = e.target.files?.[0];
          if (f) importOpml(f);
          e.target.value = "";
        }}
      />
      <div className="settings-group" style={{ marginBottom: 18 }}>
        <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
          <div
            style={{
              flex: 1,
              display: "flex",
              alignItems: "center",
              gap: 8,
              padding: "6px 10px",
              borderRadius: 7,
              border: "1px solid var(--hair-strong)",
              background: "var(--panel)",
            }}
          >
            <Icon name="search" size={13} color="var(--muted)" />
            <input
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder={t("settings.subscriptions.searchPlaceholder")}
              {...NO_AUTOCORRECT}
              style={{
                flex: 1,
                border: 0,
                outline: 0,
                background: "transparent",
                fontFamily: "inherit",
                fontSize: 12.5,
                color: "var(--ink)",
              }}
            />
          </div>
          {isAdmin && (
            <button className="s-btn" onClick={() => fileRef.current?.click()}>
              <Icon name="arrow-down" size={12} /> {t("settings.subscriptions.importOpml")}
            </button>
          )}
          <button className="s-btn" onClick={exportOpml}>
            <Icon name="arrow-up" size={12} /> {t("settings.subscriptions.export")}
          </button>
          {isAdmin && (
            <button className="s-btn primary" onClick={onAddFeed}>
              <Icon name="plus" size={12} /> {t("common.add")}
            </button>
          )}
        </div>
      </div>
      {isAdmin && <RsshubInstanceGroup />}
      <div className="settings-group">
        <h3 className="settings-group-title">
          {t("settings.subscriptions.feedsCount", { count: filtered.length })}
        </h3>
        <div>
          {filtered.map((f) => (
            <div key={f.id} className="s-feed-row">
              <FeedAvatar
                title={f.title}
                faviconUrl={f.faviconUrl}
                seed={f.id}
                style={{ width: 22, height: 22, borderRadius: 5 }}
              />
              <span className="name">{f.title}</span>
              <span className="url">{feedHost(f)}</span>
              {isAdmin && (
              <div className="actions">
                <Select
                  value={f.openMode ?? "default"}
                  options={openModeOptions}
                  onChange={(v) => updateOpenMode(f, v)}
                  aria-label={t("settings.subscriptions.openMode")}
                />
                <Select
                  value={intervalValue(f.refreshIntervalMin)}
                  options={intervalOptions}
                  onChange={(v) => updateInterval(f, v)}
                  aria-label={t("settings.subscriptions.refreshInterval")}
                />
                <button
                  className="icon-btn"
                  title={t("settings.subscriptions.unsubscribe")}
                  onClick={() => unsubscribe(f)}
                >
                  <Icon name="trash" size={13} />
                </button>
              </div>
              )}
            </div>
          ))}
          {filtered.length === 0 && (
            <div
              style={{ padding: "16px 4px", fontSize: 13, color: "var(--muted)" }}
            >
              {t("settings.subscriptions.noMatch")}
            </div>
          )}
        </div>
      </div>
    </>
  );
}

/**
 * RSSHub instance for expanding `rsshub://route` short links. Blank uses the
 * public rsshub.app; self-hosters point it at their own instance. Persisted to
 * the `rsshub_instance` setting that `add_feed` reads server-side.
 */
function RsshubInstanceGroup() {
  const { t } = useTranslation();
  const [value, setValue] = useState("");

  useEffect(() => {
    api
      .getSetting("rsshub_instance")
      .then((v) => setValue(v ?? ""))
      .catch(() => {});
  }, []);

  const commit = () => {
    api.setSetting("rsshub_instance", value.trim()).catch(() => {});
  };

  return (
    <div className="settings-group" style={{ marginBottom: 18 }}>
      <h3 className="settings-group-title">{t("settings.subscriptions.rsshubTitle")}</h3>
      <Row
        label={t("settings.subscriptions.rsshubInstance")}
        desc={t("settings.subscriptions.rsshubInstanceDesc")}
      >
        <input
          value={value}
          onChange={(e) => setValue(e.target.value)}
          onBlur={commit}
          onKeyDown={(e) => {
            if (e.key === "Enter") e.currentTarget.blur();
          }}
          placeholder="https://rsshub.app"
          {...NO_AUTOCORRECT}
          style={{
            width: 220,
            padding: "5px 9px",
            borderRadius: 7,
            border: "1px solid var(--hair-strong)",
            background: "var(--panel)",
            fontFamily: "inherit",
            fontSize: 12.5,
            color: "var(--ink)",
            outline: 0,
          }}
        />
      </Row>
    </div>
  );
}

/* ── sync ────────────────────────────────────────────────── */
function SyncSection({ onToast }: { onToast: (m: string) => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const actions = useArticleActions();
  const status = useQuery({
    queryKey: ["freshrss-status"],
    queryFn: api.freshrssStatus,
  });
  const [provider, setProvider] = useState<api.GReaderProvider>("freshrss");
  const [url, setUrl] = useState("");
  const [user, setUser] = useState("");
  const [pass, setPass] = useState("");
  const [busy, setBusy] = useState(false);
  const connected = status.data?.connected ?? false;
  const connectedProvider: api.GReaderProvider =
    status.data?.provider ?? "freshrss";
  const providerLabel = (p: api.GReaderProvider) =>
    p === "miniflux" ? "Miniflux" : "FreshRSS";

  const connect = async () => {
    if (!url.trim() || !user.trim()) return;
    setBusy(true);
    try {
      await api.freshrssConnect(url.trim(), user.trim(), pass, provider);
      await qc.invalidateQueries({ queryKey: ["freshrss-status"] });
      onToast(t("settings.sync.connected"));
      setPass("");
    } catch (e) {
      reportError(e);
    } finally {
      setBusy(false);
    }
  };

  const disconnect = async () => {
    setBusy(true);
    try {
      await api.freshrssDisconnect();
      await qc.invalidateQueries({ queryKey: ["freshrss-status"] });
      onToast(t("settings.sync.disconnected"));
    } catch (e) {
      reportError(e);
    } finally {
      setBusy(false);
    }
  };

  const syncNow = async () => {
    setBusy(true);
    try {
      const n = await api.freshrssSync();
      // Sync reconciles read/starred state and may add feeds — refresh the
      // article-bearing caches, not unrelated ones (AI summaries, settings).
      actions.refreshAfterBulk();
      onToast(t("settings.sync.syncDone", { count: n }));
    } catch (e) {
      reportError(e);
    } finally {
      setBusy(false);
    }
  };

  const unavailable = [
    { name: "Feedly", initial: "F", color: "#2BB24C", reason: t("settings.sync.reasonOauth") },
    { name: "Inoreader", initial: "I", color: "#1976D2", reason: t("settings.sync.reasonOauth") },
    { name: "iCloud", initial: "☁", color: "#0089E0", reason: t("settings.sync.reasonEntitlements") },
  ];

  return (
    <>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.sync.greader")}</h3>
        {connected ? (
          <>
            <div className="s-service">
              <div
                className="logo"
                style={{
                  background:
                    connectedProvider === "miniflux" ? "#1F7AEC" : "#4A4A4A",
                }}
              >
                {connectedProvider === "miniflux" ? "M" : "⚡"}
              </div>
              <div className="info">
                <div className="title">{providerLabel(connectedProvider)}</div>
                <div className="desc">{status.data?.url}</div>
              </div>
              <span className="status on">{t("settings.sync.statusConnected")}</span>
            </div>
            <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
              <button
                className="s-btn primary"
                onClick={syncNow}
                disabled={busy}
              >
                <Icon name="refresh" size={12} />{" "}
                {busy ? t("settings.sync.syncing") : t("settings.sync.syncNow")}
              </button>
              <button className="s-btn" onClick={disconnect} disabled={busy}>
                {t("settings.sync.disconnect")}
              </button>
            </div>
            <p
              style={{
                fontSize: 12,
                color: "var(--muted)",
                marginTop: 12,
                lineHeight: 1.5,
              }}
            >
              {t("settings.sync.syncHint")}
            </p>
          </>
        ) : (
          <>
            <p className="modal-hint" style={{ marginBottom: 14 }}>
              {t("settings.sync.connectHint")}
            </p>
            <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
              <select
                className="modal-input"
                style={{ margin: 0 }}
                value={provider}
                onChange={(e) =>
                  setProvider(e.target.value as api.GReaderProvider)
                }
                aria-label={t("settings.sync.provider")}
              >
                <option value="freshrss">
                  {t("settings.sync.providerFreshrss")}
                </option>
                <option value="miniflux">
                  {t("settings.sync.providerMiniflux")}
                </option>
              </select>
              <input
                className="modal-input"
                style={{ margin: 0 }}
                placeholder={t("settings.sync.serverPlaceholder")}
                {...NO_AUTOCORRECT}
                value={url}
                onChange={(e) => setUrl(e.target.value)}
              />
              <input
                className="modal-input"
                style={{ margin: 0 }}
                placeholder={t("settings.sync.userPlaceholder")}
                {...NO_AUTOCORRECT}
                value={user}
                onChange={(e) => setUser(e.target.value)}
              />
              <input
                className="modal-input"
                style={{ margin: 0 }}
                type="password"
                placeholder={
                  provider === "miniflux"
                    ? t("settings.sync.appPassPlaceholder")
                    : t("settings.sync.passPlaceholder")
                }
                {...NO_AUTOCORRECT}
                value={pass}
                onChange={(e) => setPass(e.target.value)}
              />
              {provider === "miniflux" && (
                <p
                  style={{
                    fontSize: 12,
                    color: "var(--muted)",
                    margin: "2px 0 0",
                    lineHeight: 1.5,
                  }}
                >
                  {t("settings.sync.minifluxPassHint")}
                </p>
              )}
              <div>
                <button
                  className="s-btn primary"
                  onClick={connect}
                  disabled={busy || !url.trim() || !user.trim()}
                >
                  {busy ? t("settings.sync.connecting") : t("settings.sync.connect")}
                </button>
              </div>
            </div>
          </>
        )}
      </div>

      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.sync.otherServices")}</h3>
        {unavailable.map((s) => (
          <div key={s.name} className="s-service" style={{ opacity: 0.6 }}>
            <div className="logo" style={{ background: s.color }}>
              {s.initial}
            </div>
            <div className="info">
              <div className="title">{s.name}</div>
              <div className="desc">{s.reason}</div>
            </div>
            <span className="status">{t("settings.sync.statusUnavailable")}</span>
          </div>
        ))}
      </div>
    </>
  );
}

/* ── shortcuts ───────────────────────────────────────────── */
function ShortcutsSection() {
  const { t } = useTranslation();
  const groups = [
    {
      title: t("settings.shortcuts.navigation"),
      items: [
        { desc: t("settings.shortcuts.nextArticle"), keys: ["J"] },
        { desc: t("settings.shortcuts.prevArticle"), keys: ["K"] },
        { desc: t("settings.shortcuts.openInBrowser"), keys: ["O"] },
        { desc: t("settings.shortcuts.toggleRead"), keys: ["U"] },
        { desc: t("settings.shortcuts.exitFocus"), keys: ["Esc"] },
      ],
    },
    {
      title: t("settings.shortcuts.actions"),
      items: [
        { desc: t("settings.shortcuts.star"), keys: ["S"] },
        { desc: t("settings.shortcuts.readLater"), keys: ["B"] },
        { desc: t("settings.shortcuts.aiSummary"), keys: ["I"] },
        { desc: t("settings.shortcuts.markAllRead"), keys: ["⇧", "A"] },
      ],
    },
    {
      title: t("settings.shortcuts.view"),
      items: [
        { desc: t("settings.shortcuts.focusReading"), keys: ["F"] },
        { desc: t("settings.shortcuts.hideRead"), keys: ["V"] },
        { desc: t("settings.shortcuts.toggleTheme"), keys: ["⇧", "D"] },
      ],
    },
    {
      title: t("settings.shortcuts.global"),
      items: [
        { desc: t("settings.shortcuts.commandPalette"), keys: [modKey, "K"] },
        { desc: t("settings.shortcuts.refreshAll"), keys: [modKey, "R"] },
        { desc: t("settings.shortcuts.addFeed"), keys: ["A"] },
        { desc: t("settings.shortcuts.openSettings"), keys: [modKey, ","] },
      ],
    },
  ];
  return (
    <>
      {groups.map((g) => (
        <div className="settings-group" key={g.title}>
          <h3 className="settings-group-title">{g.title}</h3>
          <div className="s-shortcuts">
            {g.items.map((it, i) => (
              <div className="s-shortcut" key={i}>
                <span className="desc">{it.desc}</span>
                <span className="keys">
                  {it.keys.map((k, j) => (
                    <span className="s-key" key={j}>
                      {k}
                    </span>
                  ))}
                </span>
              </div>
            ))}
          </div>
        </div>
      ))}
    </>
  );
}

/* ── notifications ───────────────────────────────────────── */
function NotificationsSection() {
  const { t } = useTranslation();
  return (
    <>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.notifications.system")}</h3>
        <SettingFlag
          settingKey="notify_enabled"
          fallback
          label={t("settings.notifications.allow")}
          desc={t("settings.notifications.allowDesc")}
        />
        <SettingFlag
          settingKey="notify_badge"
          fallback
          label={t("settings.notifications.badge")}
          desc={t("settings.notifications.badgeDesc")}
        />
        <SettingFlag
          settingKey="notify_sound"
          label={t("settings.notifications.sound")}
          desc={t("settings.notifications.soundDesc")}
        />
      </div>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.notifications.dnd")}</h3>
        <SettingFlag
          settingKey="notify_dnd_night"
          label={t("settings.notifications.dndNight")}
          desc={t("settings.notifications.dndNightDesc")}
        />
      </div>
    </>
  );
}

/* ── advanced ────────────────────────────────────────────── */
function AdvancedSection({
  onToast,
  isAdmin,
}: {
  onToast: (m: string) => void;
  isAdmin: boolean;
}) {
  const { t } = useTranslation();
  if (!isAdmin) {
    return (
      <div className="settings-group">
        <p className="settings-group-desc">{t("settings.sub.advanced")}</p>
      </div>
    );
  }
  return (
    <>
      <AiSettingsGroup onToast={onToast} />
      <OfficialBalanceGroup />
      <AiUsageGroup />
      <StorageGroup onToast={onToast} />
      <NetworkGroup onToast={onToast} />
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.advanced.experimental")}</h3>
        <SettingFlag
          settingKey="dedup_enabled"
          label={t("settings.advanced.dedup")}
          desc={t("settings.advanced.dedupDesc")}
          fallback={true}
        />
      </div>
      <DangerZone onToast={onToast} />
    </>
  );
}

/** Storage panel — real database size, retention cleanup, vacuum. */
function StorageGroup({ onToast }: { onToast: (m: string) => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const stats = useQuery({
    queryKey: ["storage-stats"],
    queryFn: api.storageStats,
  });
  const [retention, setRetention] = useState("forever");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api
      .getSetting("retention_days")
      .then((v) => {
        if (v) setRetention(v);
      })
      .catch(() => {});
  }, []);

  const cleanup = async () => {
    if (retention === "forever") {
      onToast(t("settings.advanced.cleanupForever"));
      return;
    }
    setBusy(true);
    try {
      const n = await api.cleanupArticles(Number(retention));
      await qc.invalidateQueries();
      onToast(
        n > 0
          ? t("settings.advanced.cleanupDone", { count: n })
          : t("settings.advanced.cleanupNone"),
      );
    } catch (e) {
      reportError(e);
    } finally {
      setBusy(false);
    }
  };
  const vacuum = async () => {
    setBusy(true);
    try {
      await api.vacuumDb();
      await qc.invalidateQueries({ queryKey: ["storage-stats"] });
      onToast(t("settings.advanced.vacuumDone"));
    } catch (e) {
      reportError(e);
    } finally {
      setBusy(false);
    }
  };

  const s = stats.data;
  return (
    <div className="settings-group">
      <h3 className="settings-group-title">{t("settings.advanced.storage")}</h3>
      <Row
        label={t("settings.advanced.dbUsage")}
        desc={
          s
            ? t("settings.advanced.dbUsageDesc", {
                articles: s.articleCount,
                feeds: s.feedCount,
              })
            : t("settings.advanced.calculating")
        }
      >
        <span className="s-value">{s ? formatBytes(s.dbBytes) : "—"}</span>
      </Row>
      <Row
        label={t("settings.advanced.retention")}
        desc={t("settings.advanced.retentionDesc")}
      >
        <Select
          value={retention}
          options={[
            { value: "30", label: t("settings.advanced.retention30") },
            { value: "90", label: t("settings.advanced.retention90") },
            { value: "180", label: t("settings.advanced.retention180") },
            { value: "forever", label: t("settings.advanced.retentionForever") },
          ]}
          onChange={(v) => {
            setRetention(v);
            api.setSetting("retention_days", v).catch(() => {});
          }}
        />
      </Row>
      <Row
        label={t("settings.advanced.cleanupNow")}
        desc={t("settings.advanced.cleanupNowDesc")}
      >
        <button className="s-btn" onClick={cleanup} disabled={busy}>
          {t("settings.advanced.cleanup")}
        </button>
      </Row>
      <Row
        label={t("settings.advanced.vacuum")}
        desc={t("settings.advanced.vacuumDesc")}
      >
        <button className="s-btn" onClick={vacuum} disabled={busy}>
          {t("settings.advanced.compress")}
        </button>
      </Row>
    </div>
  );
}

/** Network panel — proxy, fetch concurrency, request timeout. */
function NetworkGroup({ onToast }: { onToast: (m: string) => void }) {
  const { t } = useTranslation();
  const [proxy, setProxy] = useState("system");
  const [customProxy, setCustomProxy] = useState("");
  const [concurrency, setConcurrency] = useState(6);
  const [timeoutSec, setTimeoutSec] = useState(30);

  useEffect(() => {
    Promise.all([
      api.getSetting("net_proxy"),
      api.getSetting("net_concurrency"),
      api.getSetting("net_timeout_sec"),
    ])
      .then(([p, c, t]) => {
        if (p === "system" || p === "none") setProxy(p);
        else if (p) {
          setProxy("custom");
          setCustomProxy(p);
        }
        // Clamp to each slider's range (concurrency 1–16, timeout 5–120) so a
        // stale or corrupt stored value can't show a NaN / out-of-range readout.
        if (c) setConcurrency(clampSetting(c, 6, 1, 16));
        if (t) setTimeoutSec(clampSetting(t, 30, 5, 120));
      })
      .catch(() => {});
  }, []);

  const saveProxy = (mode: string, custom: string) => {
    const value = mode === "custom" ? custom : mode;
    api
      .setSetting("net_proxy", value)
      .then(() => api.applyNetworkSettings())
      .then(() => onToast(t("settings.advanced.proxyApplied")))
      .catch((e) => reportError(e));
  };

  return (
    <div className="settings-group">
      <h3 className="settings-group-title">{t("settings.advanced.network")}</h3>
      <Row label={t("settings.advanced.proxy")}>
        <Select
          value={proxy}
          options={[
            { value: "system", label: t("settings.advanced.proxySystem") },
            { value: "none", label: t("settings.advanced.proxyNone") },
            { value: "custom", label: t("settings.advanced.proxyCustom") },
          ]}
          onChange={(v) => {
            setProxy(v);
            if (v !== "custom") saveProxy(v, "");
          }}
        />
      </Row>
      {proxy === "custom" && (
        <Row
          label={t("settings.advanced.proxyAddress")}
          desc={t("settings.advanced.proxyAddressDesc")}
        >
          <input
            className="s-text-input"
            {...NO_AUTOCORRECT}
            value={customProxy}
            placeholder="http://host:port"
            onChange={(e) => setCustomProxy(e.target.value)}
            onBlur={() => saveProxy("custom", customProxy)}
          />
        </Row>
      )}
      <Row
        label={t("settings.advanced.concurrency")}
        desc={t("settings.advanced.concurrencyDesc")}
      >
        <Slider
          value={concurrency}
          min={1}
          max={16}
          onChange={setConcurrency}
          onCommit={(v) =>
            api.setSetting("net_concurrency", String(v)).catch(() => {})
          }
        />
      </Row>
      <Row label={t("settings.advanced.timeout")}>
        <Slider
          value={timeoutSec}
          min={5}
          max={120}
          step={5}
          unit={t("settings.advanced.secondsUnit")}
          onChange={setTimeoutSec}
          onCommit={(v) =>
            api
              .setSetting("net_timeout_sec", String(v))
              .then(() => api.applyNetworkSettings())
              .catch(() => {})
          }
        />
      </Row>
    </div>
  );
}

/** Danger zone — reset settings, wipe all local data. Each action is gated by
 *  a themed ConfirmDialog rather than the native, unstyled window.confirm. */
function DangerZone({ onToast }: { onToast: (m: string) => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [confirming, setConfirming] = useState<null | "reset" | "clear">(null);

  const doReset = async () => {
    try {
      await api.resetSettings();
      for (const k of Object.keys(localStorage)) {
        if (
          k.startsWith("pref.") ||
          [
            // "accent" / "darkShade" are intentionally still cleared: they
            // remove any value persisted by older builds that exposed an
            // accent picker / dark-shade picker.
            // "theme" is the pre-6-theme key; still cleared so a reset wipes it
            // for migrated installs. "palette"/"mode" are the current keys.
            "palette", "mode", "theme", "accent", "darkShade", "density", "viewMode", "readerFont",
            "useSerif", "readerSize", "readerLeading", "readerWidth",
            "collapsedFolders",
            "papr.feedSort",
            "papr.tagSort",
          ].includes(k)
        ) {
          localStorage.removeItem(k);
        }
      }
      onToast(t("settings.advanced.resetDone"));
      setTimeout(() => location.reload(), 900);
    } catch (e) {
      reportError(e);
    }
  };
  const doClear = async () => {
    try {
      await api.clearAllData();
      await qc.invalidateQueries();
      onToast(t("settings.advanced.clearDone"));
    } catch (e) {
      reportError(e);
    }
  };

  return (
    <div className="settings-group">
      <h3 className="settings-group-title">{t("settings.advanced.dangerZone")}</h3>
      <Row
        label={t("settings.advanced.resetSettings")}
        desc={t("settings.advanced.resetSettingsDesc")}
      >
        <button className="s-btn" onClick={() => setConfirming("reset")}>
          {t("settings.advanced.reset")}
        </button>
      </Row>
      <Row
        label={t("settings.advanced.clearData")}
        desc={t("settings.advanced.clearDataDesc")}
      >
        <button className="s-btn danger" onClick={() => setConfirming("clear")}>
          {t("settings.advanced.clear")}
        </button>
      </Row>
      {confirming === "reset" && (
        <ConfirmDialog
          title={t("settings.advanced.resetSettings")}
          message={t("settings.advanced.resetConfirm")}
          confirmLabel={t("settings.advanced.reset")}
          onConfirm={doReset}
          onClose={() => setConfirming(null)}
        />
      )}
      {confirming === "clear" && (
        <ConfirmDialog
          title={t("settings.advanced.clearData")}
          message={t("settings.advanced.clearConfirm")}
          confirmLabel={t("common.delete")}
          onConfirm={doClear}
          onClose={() => setConfirming(null)}
        />
      )}
    </div>
  );
}

/** The default article-translation engine. "llm" reuses the AI provider
 *  configured for summaries; the rest are standalone machine-translation
 *  services. The reader can override this per translation, but only temporarily. */
type TranslateEngine = "llm" | "google" | "deepl" | "bing";

/** Real AI provider configuration — backing the AI summary feature, plus the
 *  default translation engine + language and the engines' credentials. */
function AiSettingsGroup({ onToast }: { onToast: (m: string) => void }) {
  const { t, i18n } = useTranslation();
  const qc = useQueryClient();
  const [provider, setProvider] = useState<"anthropic" | "openai" | "deepseek">(
    "anthropic",
  );
  const [apiKey, setApiKey] = useState("");
  const [model, setModel] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  // Default engine + target language for translation. Empty lang = follow the UI
  // language until the user picks one.
  const [engine, setEngine] = useState<TranslateEngine>("llm");
  const [translateLang, setTranslateLang] = useState("");
  const savedKey = useRef("");
  const savedModel = useRef("");
  const savedBaseUrl = useRef("");

  useEffect(() => {
    Promise.all([
      api.getSetting("ai_provider"),
      api.getSetting("ai_api_key"),
      api.getSetting("ai_model"),
      api.getSetting("ai_base_url"),
      api.getSetting("translate_engine"),
      api.getSetting("translate_target_lang"),
    ])
      .then(([p, k, m, b, eng, tl]) => {
        if (p === "openai" || p === "anthropic" || p === "deepseek")
          setProvider(p);
        if (k) {
          setApiKey(k);
          savedKey.current = k;
        }
        if (m) {
          setModel(m);
          savedModel.current = m;
        }
        if (b) {
          setBaseUrl(b);
          savedBaseUrl.current = b;
        }
        if (eng === "google" || eng === "deepl" || eng === "bing" || eng === "llm")
          setEngine(eng);
        if (tl) setTranslateLang(tl);
      })
      .catch(() => {});
  }, []);

  const save = (key: string, value: string, label: string) => {
    api
      .setSetting(key, value)
      .then(() => onToast(t("settings.advanced.aiSaved", { label })))
      .catch((e) => reportError(e));
  };

  const placeholder =
    provider === "openai"
      ? t("settings.advanced.aiModelPlaceholderOpenai")
      : provider === "deepseek"
        ? t("settings.advanced.aiModelPlaceholderDeepseek")
        : t("settings.advanced.aiModelPlaceholderAnthropic");

  const baseUrlPlaceholder =
    provider === "openai"
      ? "https://api.openai.com/v1"
      : provider === "deepseek"
        ? "https://api.deepseek.com"
        : "https://api.anthropic.com/v1";

  return (
    <div className="settings-group">
      <h3 className="settings-group-title">{t("settings.advanced.aiSummary")}</h3>
      <Row
        label={t("settings.advanced.aiProvider")}
        desc={t("settings.advanced.aiProviderDesc")}
      >
        <Select
          value={provider}
          options={[
            { value: "anthropic", label: "Anthropic" },
            { value: "openai", label: "OpenAI" },
            { value: "deepseek", label: "DeepSeek" },
          ]}
          onChange={(v) => {
            setProvider(v);
            // The model name and base URL are provider-specific — carrying
            // them over would send e.g. an OpenAI model to Anthropic. Clear
            // both so the backend falls back to the new provider's defaults.
            setModel("");
            savedModel.current = "";
            setBaseUrl("");
            savedBaseUrl.current = "";
            Promise.all([
              api.setSetting("ai_provider", v),
              api.setSetting("ai_model", ""),
              api.setSetting("ai_base_url", ""),
            ])
              .then(() =>
                onToast(
                  t("settings.advanced.aiSaved", {
                    label: t("settings.advanced.aiProviderLabel"),
                  }),
                ),
              )
              .catch((e) => reportError(e));
          }}
        />
      </Row>
      <Row
        label={t("settings.advanced.aiApiKey")}
        desc={t("settings.advanced.aiApiKeyDesc")}
      >
        <input
          className="s-text-input"
          type="password"
          {...NO_AUTOCORRECT}
          value={apiKey}
          placeholder="sk-…"
          onChange={(e) => setApiKey(e.target.value)}
          onBlur={() => {
            // Trim before persisting — a pasted key routinely carries a
            // trailing newline / space that would break the auth header.
            const trimmed = apiKey.trim();
            if (trimmed !== apiKey) setApiKey(trimmed);
            if (trimmed !== savedKey.current) {
              savedKey.current = trimmed;
              save("ai_api_key", trimmed, t("settings.advanced.aiApiKeyLabel"));
            }
          }}
        />
      </Row>
      <Row
        label={t("settings.advanced.aiModel")}
        desc={t("settings.advanced.aiModelDesc")}
      >
        <input
          className="s-text-input"
          type="text"
          {...NO_AUTOCORRECT}
          value={model}
          placeholder={placeholder}
          onChange={(e) => setModel(e.target.value)}
          onBlur={() => {
            // Trim before persisting — a pasted model name with a stray
            // space / newline yields a "model not found" from the provider.
            const trimmed = model.trim();
            if (trimmed !== model) setModel(trimmed);
            if (trimmed !== savedModel.current) {
              savedModel.current = trimmed;
              save("ai_model", trimmed, t("settings.advanced.aiModelLabel"));
            }
          }}
        />
      </Row>
      <Row
        label={t("settings.advanced.aiBaseUrl")}
        desc={t("settings.advanced.aiBaseUrlDesc")}
      >
        <input
          className="s-text-input"
          type="text"
          {...NO_AUTOCORRECT}
          value={baseUrl}
          placeholder={baseUrlPlaceholder}
          onChange={(e) => setBaseUrl(e.target.value)}
          onBlur={() => {
            const trimmed = baseUrl.trim();
            if (trimmed !== baseUrl) setBaseUrl(trimmed);
            if (trimmed !== savedBaseUrl.current) {
              savedBaseUrl.current = trimmed;
              save("ai_base_url", trimmed, t("settings.advanced.aiBaseUrlLabel"));
            }
          }}
        />
      </Row>
      <Row
        label={t("settings.advanced.translateEngine")}
        desc={t("settings.advanced.translateEngineDesc")}
      >
        <Select
          value={engine}
          options={[
            { value: "llm", label: t("settings.advanced.translateEngineLlm") },
            { value: "google", label: "Google" },
            { value: "deepl", label: "DeepL" },
            { value: "bing", label: "Bing" },
          ]}
          aria-label={t("settings.advanced.translateEngine")}
          onChange={(v) => {
            setEngine(v);
            save("translate_engine", v, t("settings.advanced.translateEngineLabel"));
            // The reader reads this default when starting a translation —
            // refresh it so the change takes effect on the next translate.
            qc.invalidateQueries({ queryKey: ["setting", "translate_engine"] });
          }}
        />
      </Row>
      <Row
        label={t("settings.advanced.translateLang")}
        desc={t("settings.advanced.translateLangDesc")}
      >
        <Select
          value={translateLang || i18n.language}
          options={LANGUAGES.map((l) => ({ value: l.code, label: l.label }))}
          aria-label={t("settings.advanced.translateLang")}
          onChange={(v) => {
            setTranslateLang(v);
            save("translate_target_lang", v, t("settings.advanced.translateLangLabel"));
            // The reader caches this default to decide whether a stored
            // translation is still current — refresh it so a change applies now.
            qc.invalidateQueries({ queryKey: ["setting", "translate_target_lang"] });
          }}
        />
      </Row>
    </div>
  );
}

/** Official DeepSeek balance — real money spent per day (balance deltas from
 * the official /user/balance endpoint), plus dashboard usage when a platform
 * token is configured. Refreshes daily server-side; a manual refresh button
 * forces a snapshot now. */
function OfficialBalanceGroup() {
  const { t } = useTranslation();
  const [days, setDays] = useState(14);
  const report = useQuery({
    queryKey: ["ai-balance", days],
    queryFn: () => api.aiBalance(days),
  });
  const qc = useQueryClient();
  const [refreshing, setRefreshing] = useState(false);

  const refresh = async () => {
    setRefreshing(true);
    try {
      await api.aiBalanceRefresh();
      qc.invalidateQueries({ queryKey: ["ai-balance"] });
    } finally {
      setRefreshing(false);
    }
  };

  const fmt = (n?: number | null) =>
    n == null ? "—" : `¥${n.toFixed(2)}`;

  return (
    <div className="settings-group">
      <h3 className="settings-group-title">{t("settings.advanced.aiOfficial")}</h3>
      <Row
        label={t("settings.advanced.aiOfficialWindow")}
        desc={t("settings.advanced.aiOfficialWindowDesc")}
      >
        <Select
          value={String(days)}
          options={[
            { value: "7", label: "7" },
            { value: "14", label: "14" },
            { value: "30", label: "30" },
          ]}
          aria-label={t("settings.advanced.aiOfficialWindow")}
          onChange={(v) => setDays(Number(v))}
        />
      </Row>

      {report.isLoading ? (
        <p className="settings-group-desc">{t("common.loading")}</p>
      ) : report.data?.latest ? (
        <>
          <Row
            label={t("settings.advanced.aiOfficialBalance")}
            desc={t("settings.advanced.aiOfficialBalanceDesc")}
          >
            <span className="s-value">{fmt(report.data.latest.totalBalance)}</span>
          </Row>
          <Row label={t("settings.advanced.aiOfficialToppedUp")}>
            <span className="s-value">
              {fmt(report.data.latest.toppedUpBalance)}{" "}
              <span className="settings-group-desc" style={{ display: "inline" }}>
                + {fmt(report.data.latest.grantedBalance)} granted
              </span>
            </span>
          </Row>
          <Row
            label={t("settings.advanced.aiOfficialDailySpend")}
            desc={t("settings.advanced.aiOfficialDailySpendDesc")}
          >
            <button
              type="button"
              className="s-btn"
              onClick={refresh}
              disabled={refreshing}
            >
              {refreshing ? t("common.loading") : t("settings.advanced.aiOfficialRefresh")}
            </button>
          </Row>
          <div className="settings-group-desc" style={{ paddingTop: 6 }}>
            {[...report.data.history].reverse().map((d) => (
              <div
                key={d.day}
                style={{ display: "flex", justifyContent: "space-between", gap: 12 }}
              >
                <span>{d.day}</span>
                <span style={{ fontVariantNumeric: "tabular-nums" }}>
                  {d.topup != null
                    ? `+${fmt(d.topup)}`
                    : d.spend != null
                      ? `-${fmt(d.spend)}`
                      : "—"}
                </span>
              </div>
            ))}
          </div>
          {report.data.officialUsage.length > 0 && (
            <div className="settings-group-desc" style={{ paddingTop: 6 }}>
              <div style={{ display: "flex", justifyContent: "space-between" }}>
                <span>{t("settings.advanced.aiOfficialPlatform")}</span>
                <span />
              </div>
              {[...report.data.officialUsage].reverse().map((u) => (
                <div
                  key={u.day}
                  style={{ display: "flex", justifyContent: "space-between", gap: 12 }}
                >
                  <span>{u.day}</span>
                  <span style={{ fontVariantNumeric: "tabular-nums" }}>
                    {u.tokens.toLocaleString()} tok · {fmt(u.cost)}
                  </span>
                </div>
              ))}
            </div>
          )}
        </>
      ) : (
        <p className="settings-group-desc">
          {t("settings.advanced.aiOfficialEmpty")}
          <button type="button" className="s-btn" onClick={refresh}>
            {t("settings.advanced.aiOfficialRefresh")}
          </button>
        </p>
      )}
    </div>
  );
}

/** AI usage ledger — tokens spent per feature over a window + estimated cost. */
function AiUsageGroup() {
  const { t } = useTranslation();
  const [days, setDays] = useState(30);
  const report = useQuery({
    queryKey: ["ai-usage", days],
    queryFn: () => api.aiUsage(days),
  });

  const fmt = (n: number) => n.toLocaleString();
  const total = report.data?.total;
  const cost = report.data?.estimatedCost ?? 0;

  return (
    <div className="settings-group">
      <h3 className="settings-group-title">{t("settings.advanced.aiUsage")}</h3>
      <Row
        label={t("settings.advanced.aiUsageWindow")}
        desc={t("settings.advanced.aiUsageWindowDesc")}
      >
        <Select
          value={String(days)}
          options={[
            { value: "7", label: "7" },
            { value: "30", label: "30" },
            { value: "90", label: "90" },
          ]}
          aria-label={t("settings.advanced.aiUsageWindow")}
          onChange={(v) => setDays(Number(v))}
        />
      </Row>

      {report.isLoading ? (
        <p className="settings-group-desc">{t("common.loading")}</p>
      ) : total ? (
        <>
          <Row
            label={t("settings.advanced.aiUsageCalls")}
            desc={t("settings.advanced.aiUsageCallsDesc", { days })}
          >
            <span className="s-value">{fmt(total.calls)}</span>
          </Row>
          <Row label={t("settings.advanced.aiUsagePrompt")}>
            <span className="s-value">{fmt(total.promptTokens)}</span>
          </Row>
          <Row label={t("settings.advanced.aiUsageCacheHit")}>
            <span className="s-value">{fmt(total.cacheHitTokens ?? 0)}</span>
          </Row>
          <Row label={t("settings.advanced.aiUsageCompletion")}>
            <span className="s-value">{fmt(total.completionTokens)}</span>
          </Row>
          <Row label={t("settings.advanced.aiUsageReasoning")}>
            <span className="s-value">{fmt(total.reasoningTokens)}</span>
          </Row>
          <Row
            label={t("settings.advanced.aiUsageCost")}
            desc={t("settings.advanced.aiUsageCostDesc")}
          >
            <span className="s-value">¥{cost.toFixed(4)}</span>
          </Row>

          {report.data!.byFeature.length > 0 && (
            <div className="settings-group-desc" style={{ paddingTop: 6 }}>
              {report.data!.byFeature.map((r) => (
                <div
                  key={r.feature}
                  style={{ display: "flex", justifyContent: "space-between", gap: 12 }}
                >
                  <span>{t(`settings.advanced.aiFeature.${r.feature}`, {
                    defaultValue: r.feature,
                  })}</span>
                  <span style={{ fontVariantNumeric: "tabular-nums" }}>
                    {fmt(r.calls)} · {fmt(r.promptTokens + r.completionTokens)}
                  </span>
                </div>
              ))}
            </div>
          )}
          <AiPriceInputs onChanged={() => report.refetch()} />
        </>
      ) : (
        <p className="settings-group-desc">{t("settings.advanced.aiUsageEmpty")}</p>
      )}
    </div>
  );
}

/** Per-million-token price inputs (CNY). Defaults match deepseek-v4-flash. */
function AiPriceInputs({ onChanged }: { onChanged: () => void }) {
  const { t } = useTranslation();
  // Official deepseek-v4-flash CNY / M tokens:
  // cache hit 0.02 · cache miss 1 · output 2
  const defaults = {
    cacheHit: "0.02",
    cacheMiss: "1",
    output: "2",
  };
  const [prices, setPrices] = useState<Record<string, string>>(defaults);

  useEffect(() => {
    Promise.all([
      api.getSetting("ai_price_cache_hit_per_m"),
      api.getSetting("ai_price_input_per_m"),
      api.getSetting("ai_price_output_per_m"),
    ]).then(([hit, miss, out]) => {
      const next = {
        cacheHit: hit || defaults.cacheHit,
        cacheMiss: miss || defaults.cacheMiss,
        output: out || defaults.output,
      };
      setPrices(next);
      // Persist official defaults when unset so cost estimates and the
      // settings form share the same deepseek-v4-flash numbers.
      const writes: Promise<unknown>[] = [];
      if (!hit) writes.push(api.setSetting("ai_price_cache_hit_per_m", defaults.cacheHit));
      if (!miss) writes.push(api.setSetting("ai_price_input_per_m", defaults.cacheMiss));
      if (!out) writes.push(api.setSetting("ai_price_output_per_m", defaults.output));
      if (writes.length) {
        Promise.all(writes)
          .then(onChanged)
          .catch((e) => reportError(e));
      }
    });
  }, []);

  const blur = (key: "cacheHit" | "cacheMiss" | "output") => {
    const v = prices[key];
    const settingKey =
      key === "cacheHit"
        ? "ai_price_cache_hit_per_m"
        : key === "cacheMiss"
          ? "ai_price_input_per_m"
          : "ai_price_output_per_m";
    const fallback = defaults[key];
    api
      .setSetting(settingKey, /^\d+(\.\d+)?$/.test(v) ? v : fallback)
      .then(onChanged)
      .catch((e) => reportError(e));
  };

  const input = (
    labelKey: string,
    key: "cacheHit" | "cacheMiss" | "output",
    ph: string,
  ) => (
    <Row label={t(labelKey)} desc={t("settings.advanced.aiPricePerM")}>
      <input
        className="s-text-input"
        type="text"
        inputMode="decimal"
        placeholder={ph}
        value={prices[key] ?? ""}
        onChange={(e) => setPrices({ ...prices, [key]: e.target.value })}
        onBlur={() => blur(key)}
      />
    </Row>
  );

  return (
    <>
      {input("settings.advanced.aiPriceCacheHit", "cacheHit", defaults.cacheHit)}
      {input(
        "settings.advanced.aiPriceCacheMiss",
        "cacheMiss",
        defaults.cacheMiss,
      )}
      {input("settings.advanced.aiPriceOutput", "output", defaults.output)}
    </>
  );
}

/* ── users (admin) ───────────────────────────────────────── */
function UsersSection({ onToast }: { onToast: (m: string) => void }) {
  const { t } = useTranslation();
  const { user: me } = useAuth();
  const qc = useQueryClient();
  const users = useQuery({ queryKey: ["users"], queryFn: api.listUsers });
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [isAdmin, setIsAdmin] = useState(false);
  const [busy, setBusy] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState<{
    id: number;
    name: string;
  } | null>(null);
  const [resetId, setResetId] = useState<number | null>(null);
  const [resetPw, setResetPw] = useState("");

  const refresh = () => qc.invalidateQueries({ queryKey: ["users"] });

  const create = async () => {
    const name = username.trim();
    if (!name || !password) return;
    setBusy(true);
    try {
      await api.createUser(name, password, isAdmin);
      setUsername("");
      setPassword("");
      setIsAdmin(false);
      await refresh();
      onToast(t("settings.users.created", { name }));
    } catch (e) {
      reportError(e);
    } finally {
      setBusy(false);
    }
  };

  const remove = async (id: number, name: string) => {
    if (me && id === me.id) {
      onToast(t("settings.users.cannotDeleteSelf"));
      return;
    }
    setBusy(true);
    try {
      await api.deleteUser(id);
      await refresh();
      onToast(t("settings.users.deleted", { name }));
    } catch (e) {
      reportError(e);
    } finally {
      setBusy(false);
      setConfirmDelete(null);
    }
  };

  const toggleAdmin = async (id: number, name: string, next: boolean) => {
    try {
      await api.patchUser(id, { isAdmin: next });
      await refresh();
      onToast(t("settings.users.adminUpdated", { name }));
    } catch (e) {
      reportError(e);
    }
  };

  const resetPassword = async (id: number, name: string) => {
    const pw = resetPw.trim();
    if (!pw) return;
    try {
      await api.patchUser(id, { password: pw });
      setResetId(null);
      setResetPw("");
      onToast(t("settings.users.passwordUpdated", { name }));
    } catch (e) {
      reportError(e);
    }
  };

  return (
    <>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.users.createTitle")}</h3>
        <Row label={t("settings.users.username")}>
          <input
            className="s-text-input"
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            {...NO_AUTOCORRECT}
            autoComplete="off"
          />
        </Row>
        <Row label={t("settings.users.password")}>
          <input
            className="s-text-input"
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            {...NO_AUTOCORRECT}
            autoComplete="new-password"
          />
        </Row>
        <Row
          label={t("settings.users.isAdmin")}
          desc={t("settings.users.isAdminDesc")}
        >
          <Toggle checked={isAdmin} onChange={setIsAdmin} />
        </Row>
        <div style={{ display: "flex", justifyContent: "flex-end", padding: "8px 0" }}>
          <button
            className="s-btn primary"
            disabled={busy || !username.trim() || !password}
            onClick={() => void create()}
          >
            {t("settings.users.create")}
          </button>
        </div>
      </div>

      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.users.listTitle")}</h3>
        {(users.data ?? []).length === 0 && (
          <div style={{ padding: "12px 4px", fontSize: 13, color: "var(--muted)" }}>
            {t("settings.users.empty")}
          </div>
        )}
        {(users.data ?? []).map((u) => {
          const isSelf = me?.id === u.id;
          return (
            <div key={u.id} className="s-feed-row">
              <span className="name" title={u.username}>
                {u.username}
              </span>
              <span className="url">
                {u.isAdmin
                  ? t("settings.users.roleAdmin")
                  : t("settings.users.roleUser")}
              </span>
              <div className="actions">
                <Toggle
                  checked={u.isAdmin}
                  onChange={(v) => void toggleAdmin(u.id, u.username, v)}
                  aria-label={t("settings.users.isAdmin")}
                />
                <button
                  className="s-btn"
                  type="button"
                  onClick={() => {
                    setResetId(u.id);
                    setResetPw("");
                  }}
                >
                  {t("settings.users.resetPassword")}
                </button>
                <button
                  className="icon-btn"
                  type="button"
                  title={t("common.delete")}
                  disabled={isSelf}
                  onClick={() =>
                    setConfirmDelete({ id: u.id, name: u.username })
                  }
                >
                  <Icon name="trash" size={13} />
                </button>
              </div>
            </div>
          );
        })}
      </div>

      {resetId != null && (
        <div className="settings-group">
          <h3 className="settings-group-title">{t("settings.users.resetPassword")}</h3>
          <Row label={t("settings.users.newPassword")}>
            <input
              className="s-text-input"
              type="password"
              value={resetPw}
              onChange={(e) => setResetPw(e.target.value)}
              {...NO_AUTOCORRECT}
              autoComplete="new-password"
            />
          </Row>
          <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
            <button className="s-btn" type="button" onClick={() => setResetId(null)}>
              {t("common.cancel")}
            </button>
            <button
              className="s-btn primary"
              type="button"
              disabled={!resetPw.trim()}
              onClick={() => {
                const u = (users.data ?? []).find((x) => x.id === resetId);
                if (u) void resetPassword(u.id, u.username);
              }}
            >
              {t("common.save")}
            </button>
          </div>
        </div>
      )}

      {confirmDelete && (
        <ConfirmDialog
          title={t("common.delete")}
          message={t("settings.users.deleteConfirm", { name: confirmDelete.name })}
          confirmLabel={t("common.delete")}
          danger
          onConfirm={() => void remove(confirmDelete.id, confirmDelete.name)}
          onClose={() => setConfirmDelete(null)}
        />
      )}
    </>
  );
}

/* ── tag management: AI tags | interest tags (admin) ─────── */
const TAG_LIST_PAGE_SIZE = 20;

type TagSortMode = "alpha" | "count" | "unread";
type TagSortDir = "asc" | "desc";

function defaultTagSortDir(mode: TagSortMode): TagSortDir {
  return mode === "alpha" ? "asc" : "desc";
}

function tagUnreadCount(tag: Tag): number {
  return tag.unreadCount ?? 0;
}

function sortTagsList(
  list: Tag[],
  mode: TagSortMode,
  dir: TagSortDir,
): Tag[] {
  const sorted = [...list];
  const mul = dir === "asc" ? 1 : -1;
  if (mode === "alpha") {
    sorted.sort(
      (a, b) =>
        mul *
        a.name.localeCompare(b.name, undefined, { sensitivity: "base" }),
    );
  } else if (mode === "count") {
    sorted.sort((a, b) => {
      const byCount = (a.articleCount - b.articleCount) * mul;
      if (byCount !== 0) return byCount;
      return a.name.localeCompare(b.name, undefined, {
        sensitivity: "base",
      });
    });
  } else {
    sorted.sort((a, b) => {
      const byUnread = (tagUnreadCount(a) - tagUnreadCount(b)) * mul;
      if (byUnread !== 0) return byUnread;
      const byCount = b.articleCount - a.articleCount;
      if (byCount !== 0) return byCount;
      return a.name.localeCompare(b.name, undefined, {
        sensitivity: "base",
      });
    });
  }
  return sorted;
}

const STATS_DAILY_DAYS = 30;

function StatsSection() {
  const { t } = useTranslation();
  const overview = useQuery({
    queryKey: ["stats-overview", STATS_DAILY_DAYS],
    queryFn: () => api.getStatsOverview(STATS_DAILY_DAYS),
    retry: false,
    refetchInterval: 60_000,
  });

  if (overview.isError) {
    return (
      <div className="settings-group">
        <p className="settings-group-desc">{t("settings.stats.unavailable")}</p>
      </div>
    );
  }

  const d = overview.data;
  const daily = d?.daily ?? [];
  const maxDaily = Math.max(1, ...daily.map((x) => x.count));

  return (
    <>
      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.stats.overview")}</h3>
        {!d ? (
          <p className="settings-group-desc">{t("settings.stats.loading")}</p>
        ) : (
          <>
            <Row label={t("settings.stats.totalArticles")}>
              <span className="s-value">{d.totalArticles}</span>
            </Row>
            <Row label={t("settings.stats.feeds")}>
              <span className="s-value">{d.feeds}</span>
            </Row>
            <Row
              label={t("settings.stats.tagged")}
              desc={t("settings.stats.taggedDesc")}
            >
              <span className="s-value">{d.taggedArticles}</span>
            </Row>
            <Row label={t("settings.stats.taggedInterest")}>
              <span className="s-value">{d.taggedInterest}</span>
            </Row>
            <Row label={t("settings.stats.taggedAi")}>
              <span className="s-value">{d.taggedAi}</span>
            </Row>
          </>
        )}
      </div>

      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.stats.queue")}</h3>
        {!d ? (
          <p className="settings-group-desc">{t("settings.stats.loading")}</p>
        ) : (
          <>
            <Row label={t("settings.stats.pending")}>
              <span className="s-value">{d.queue.pending}</span>
            </Row>
            <Row label={t("settings.stats.processing")}>
              <span className="s-value">{d.queue.processing}</span>
            </Row>
            <Row label={t("settings.stats.failed")}>
              <span className="s-value">{d.queue.failed}</span>
            </Row>
            <Row label={t("settings.stats.done")}>
              <span className="s-value">{d.queue.done}</span>
            </Row>
          </>
        )}
      </div>

      <div className="settings-group">
        <h3 className="settings-group-title">
          {t("settings.stats.daily", { days: STATS_DAILY_DAYS })}
        </h3>
        <p className="settings-group-desc">{t("settings.stats.dailyDesc")}</p>
        {!d ? (
          <p className="settings-group-desc">{t("settings.stats.loading")}</p>
        ) : (
          <div className="s-stats-daily" role="img" aria-label={t("settings.stats.daily", { days: STATS_DAILY_DAYS })}>
            {daily.map((row) => {
              const pct = Math.round((row.count / maxDaily) * 100);
              const label = row.date.slice(5); // MM-DD
              return (
                <div key={row.date} className="s-stats-daily-row" title={`${row.date}: ${row.count}`}>
                  <span className="s-stats-daily-date">{label}</span>
                  <div className="s-stats-daily-bar-track">
                    <div
                      className="s-stats-daily-bar"
                      style={{ width: `${pct}%` }}
                    />
                  </div>
                  <span className="s-stats-daily-count">{row.count}</span>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </>
  );
}

function AutoTagSection({ onToast }: { onToast: (m: string) => void }) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const [tab, setTab] = useState<"ai" | "interest" | "aliases">("ai");
  const [interestEnabled, setInterestEnabled] = useState(false);
  const [aiEnabled, setAiEnabled] = useState(false);
  const [interestMax, setInterestMax] = useState(5);
  const [aiMax, setAiMax] = useState(5);
  const [backfillDays, setBackfillDays] = useState(7);
  const [backfillForce, setBackfillForce] = useState(false);
  const [backfillBusy, setBackfillBusy] = useState(false);
  const [clearBusy, setClearBusy] = useState(false);
  const [confirmClearQueue, setConfirmClearQueue] = useState(false);
  const [statusError, setStatusError] = useState(false);
  const [prompt, setPrompt] = useState<{
    title: string;
    initial: string;
    onSubmit: (v: string) => void;
  } | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<Tag | null>(null);
  const [confirmCleanupEmpty, setConfirmCleanupEmpty] = useState(false);
  const [cleanupBusy, setCleanupBusy] = useState(false);
  const [tagSort, setTagSort] = useState<{
    mode: TagSortMode;
    dir: TagSortDir;
  }>({ mode: "unread", dir: "desc" });
  const [page, setPage] = useState(0);
  const [aliasTagId, setAliasTagId] = useState<number | "">("");
  const [aliasDraft, setAliasDraft] = useState("");
  const [aliasFilter, setAliasFilter] = useState("");
  const [confirmDeleteAlias, setConfirmDeleteAlias] = useState<TagAlias | null>(
    null,
  );

  const tags = useQuery({ queryKey: ["tags"], queryFn: () => api.listTags() });
  const aliases = useQuery({
    queryKey: ["tag-aliases", "interest"],
    queryFn: () => api.listTagAliases({ kind: "interest" }),
  });
  const status = useQuery({
    queryKey: ["auto-tag-status", backfillDays],
    queryFn: () => api.getAutoTagStatus(backfillDays),
    retry: false,
    refetchInterval: 15_000,
  });

  useEffect(() => {
    if (status.isError) setStatusError(true);
    else if (status.isSuccess) setStatusError(false);
  }, [status.isError, status.isSuccess]);

  useEffect(() => {
    Promise.all([
      api.getSetting("auto_tag_enabled"),
      api.getSetting("auto_tag_max_tags_per_article"),
      api.getSetting("ai_tag_enabled"),
      api.getSetting("ai_tag_max_tags_per_article"),
    ])
      .then(([interestEn, interestMt, aiEn, aiMt]) => {
        setInterestEnabled(interestEn === "1" || interestEn === "true");
        setInterestMax(clampSetting(interestMt, 5, 1, 30));
        setAiEnabled(aiEn === "1" || aiEn === "true");
        setAiMax(clampSetting(aiMt, 5, 1, 30));
      })
      .catch(() => {});
  }, []);

  const refreshTags = () => {
    void qc.invalidateQueries({ queryKey: ["tags"] });
  };

  const refreshAliases = () => {
    void qc.invalidateQueries({ queryKey: ["tag-aliases"] });
  };

  const saveSetting = (key: string, value: string) => {
    api
      .setSetting(key, value)
      .then(() => onToast(t("settings.autoTag.saved")))
      .catch((e) => reportError(e));
  };

  const createInterestTag = () =>
    setPrompt({
      title: t("settings.autoTag.newTag"),
      initial: "",
      onSubmit: (v) => {
        api
          .createTag(v, "interest")
          .then(() => {
            refreshTags();
            onToast(t("settings.autoTag.tagCreated"));
          })
          .catch((e) => reportError(e));
      },
    });

  const renameTag = (tag: Tag) =>
    setPrompt({
      title:
        tag.kind === "ai"
          ? t("settings.autoTag.renameAiTag")
          : t("settings.autoTag.renameTag"),
      initial: tag.name,
      onSubmit: (v) => {
        api
          .renameTag(tag.id, v)
          .then(() => {
            refreshTags();
            refreshAliases();
            onToast(
              tag.kind === "ai"
                ? t("settings.autoTag.aiTagRenamed")
                : t("settings.autoTag.tagRenamed"),
            );
          })
          .catch((e) => reportError(e));
      },
    });

  const recolorTag = (tag: Tag, color: string) => {
    api
      .setTagColor(tag.id, color)
      .then(() => refreshTags())
      .catch((e) => reportError(e));
  };

  const removeTag = async (tag: Tag) => {
    try {
      await api.deleteTag(tag.id);
      refreshTags();
      refreshAliases();
      onToast(
        tag.kind === "ai"
          ? t("settings.autoTag.aiTagDeleted")
          : t("settings.autoTag.tagDeleted"),
      );
    } catch (e) {
      reportError(e);
    } finally {
      setConfirmDelete(null);
    }
  };

  const addAlias = async () => {
    const alias = aliasDraft.trim();
    if (aliasTagId === "" || !alias) return;
    try {
      await api.createTagAlias(aliasTagId, alias);
      setAliasDraft("");
      refreshAliases();
      onToast(t("settings.autoTag.aliasCreated"));
    } catch (e) {
      reportError(e);
    }
  };

  const removeAlias = async (row: TagAlias) => {
    try {
      await api.deleteTagAlias(row.id);
      refreshAliases();
      onToast(t("settings.autoTag.aliasDeleted"));
    } catch (e) {
      reportError(e);
    } finally {
      setConfirmDeleteAlias(null);
    }
  };

  const cleanupEmptyAiTags = async () => {
    setCleanupBusy(true);
    try {
      const res = await api.cleanupEmptyTags("ai");
      refreshTags();
      onToast(t("settings.autoTag.cleanupEmptyDone", { count: res.deleted }));
    } catch (e) {
      reportError(e);
    } finally {
      setCleanupBusy(false);
      setConfirmCleanupEmpty(false);
    }
  };

  const runBackfill = async () => {
    setBackfillBusy(true);
    try {
      const res = await api.backfillAutoTag(backfillDays, backfillForce);
      const count =
        res && typeof res === "object"
          ? (res.enqueued ?? res.queued ?? res.count)
          : undefined;
      onToast(
        count != null
          ? t("settings.autoTag.backfillQueued", { count })
          : t("settings.autoTag.backfillDone"),
      );
      void status.refetch();
    } catch (e) {
      reportError(e);
    } finally {
      setBackfillBusy(false);
    }
  };

  const runClearQueue = async () => {
    setClearBusy(true);
    try {
      const res = await api.clearAutoTagQueue();
      onToast(t("settings.autoTag.clearQueueDone", { count: res.cleared }));
      void status.refetch();
    } catch (e) {
      reportError(e);
    } finally {
      setClearBusy(false);
      setConfirmClearQueue(false);
    }
  };

  const setTagSortMode = (mode: TagSortMode) => {
    setTagSort((prev) =>
      prev.mode === mode
        ? { mode, dir: prev.dir === "asc" ? "desc" : "asc" }
        : { mode, dir: defaultTagSortDir(mode) },
    );
    setPage(0);
  };

  const switchTab = (next: "ai" | "interest" | "aliases") => {
    setTab(next);
    setPage(0);
  };

  const st = status.data;
  const allTags = tags.data ?? [];
  const interestList = allTags.filter(
    (tg) => (tg.kind ?? "interest") === "interest",
  );
  const aiList = allTags.filter((tg) => tg.kind === "ai");
  const emptyAiCount = aiList.filter((tg) => (tg.articleCount ?? 0) === 0)
    .length;
  const activeList = tab === "ai" ? aiList : interestList;
  const sortedList = sortTagsList(activeList, tagSort.mode, tagSort.dir);
  const totalPages = Math.max(
    1,
    Math.ceil(sortedList.length / TAG_LIST_PAGE_SIZE),
  );
  const safePage = Math.min(page, totalPages - 1);
  const pageList = sortedList.slice(
    safePage * TAG_LIST_PAGE_SIZE,
    (safePage + 1) * TAG_LIST_PAGE_SIZE,
  );

  const aliasRows = aliases.data ?? [];
  const aliasFilterNorm = aliasFilter.trim().toLowerCase();
  const filteredAliases = aliasFilterNorm
    ? aliasRows.filter(
        (a) =>
          a.alias.toLowerCase().includes(aliasFilterNorm) ||
          a.tagName.toLowerCase().includes(aliasFilterNorm),
      )
    : aliasRows;

  // After delete (or other list shrinks), pull back if the current page
  // would be empty past the last page.
  useEffect(() => {
    if (page !== safePage) setPage(safePage);
  }, [page, safePage]);

  // Prefer a valid interest tag once the vocabulary loads.
  useEffect(() => {
    if (aliasTagId !== "") return;
    const first = interestList[0];
    if (first) setAliasTagId(first.id);
  }, [aliasTagId, interestList]);

  const renderTagRows = (emptyKey: string, allowCreate: boolean) => (
    <div className="settings-group">
      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          gap: 12,
          marginBottom: 8,
        }}
      >
        <div>
          <h3 className="settings-group-title" style={{ margin: 0 }}>
            {tab === "ai"
              ? t("settings.autoTag.aiVocabulary")
              : t("settings.autoTag.vocabulary")}
          </h3>
          <p className="settings-group-desc" style={{ margin: "4px 0 0" }}>
            {tab === "ai"
              ? t("settings.autoTag.aiVocabularyDesc")
              : t("settings.autoTag.vocabularyDesc")}
          </p>
        </div>
        <div className="s-tag-list-actions">
          {sortedList.length > 0 && (
            <span
              className="s-tag-sort"
              role="group"
              aria-label={t("settings.autoTag.sortBy")}
            >
              <button
                type="button"
                className={tagSort.mode === "alpha" ? "active" : ""}
                onClick={() => setTagSortMode("alpha")}
                aria-pressed={tagSort.mode === "alpha"}
                title={t("settings.autoTag.sortAlphaHint")}
              >
                {tagSort.mode === "alpha" && tagSort.dir === "desc"
                  ? "Z-A ↓"
                  : "A-Z ↑"}
              </button>
              <span className="s-tag-sort-sep" aria-hidden="true">
                ·
              </span>
              <button
                type="button"
                className={tagSort.mode === "count" ? "active" : ""}
                onClick={() => setTagSortMode("count")}
                aria-pressed={tagSort.mode === "count"}
                title={t("settings.autoTag.sortCountHint")}
              >
                {t("settings.autoTag.sortCount")}{" "}
                {tagSort.mode === "count" && tagSort.dir === "asc" ? "↑" : "↓"}
              </button>
              <span className="s-tag-sort-sep" aria-hidden="true">
                ·
              </span>
              <button
                type="button"
                className={tagSort.mode === "unread" ? "active" : ""}
                onClick={() => setTagSortMode("unread")}
                aria-pressed={tagSort.mode === "unread"}
                title={t("settings.autoTag.sortUpdatesHint")}
              >
                {t("settings.autoTag.sortUpdates")}{" "}
                {tagSort.mode === "unread" && tagSort.dir === "asc" ? "↑" : "↓"}
              </button>
            </span>
          )}
          {/* Always visible on AI tab — never gate on client empty count. */}
          {tab === "ai" && (
            <button
              className="s-btn danger"
              type="button"
              disabled={cleanupBusy}
              onClick={() => setConfirmCleanupEmpty(true)}
              title={t("settings.autoTag.cleanupEmptyHint")}
            >
              {t("settings.autoTag.cleanupEmpty")}
            </button>
          )}
          {allowCreate && (
            <button
              className="s-btn primary"
              type="button"
              onClick={createInterestTag}
            >
              <Icon name="plus" size={12} /> {t("common.add")}
            </button>
          )}
        </div>
      </div>
      {sortedList.length === 0 ? (
        <p className="settings-group-desc">{t(emptyKey)}</p>
      ) : (
        <>
          <div className="s-interest-tags">
            {pageList.map((tag) => (
              <div key={tag.id} className="s-interest-tag-row">
                <span
                  className="s-interest-tag-dot"
                  style={{ background: tagColor(tag.color) }}
                  aria-hidden
                />
                <span className="s-interest-tag-name">{tag.name}</span>
                <span className="s-interest-tag-count">
                  {t("settings.autoTag.articleCountWithUnread", {
                    total: tag.articleCount,
                    unread: tagUnreadCount(tag),
                  })}
                </span>
                <div className="s-interest-tag-swatches" role="group">
                  {Object.entries(TAG_PALETTE).map(([key, color]) => (
                    <button
                      key={key}
                      type="button"
                      className={`s-interest-tag-swatch ${
                        tag.color === key ? "on" : ""
                      }`}
                      style={{ background: color }}
                      title={key}
                      aria-label={key}
                      aria-pressed={tag.color === key}
                      onClick={() => recolorTag(tag, key)}
                    />
                  ))}
                </div>
                <button
                  className="icon-btn"
                  type="button"
                  title={
                    tag.kind === "ai"
                      ? t("settings.autoTag.renameAiTag")
                      : t("settings.autoTag.renameTag")
                  }
                  onClick={() => renameTag(tag)}
                >
                  <Icon name="settings" size={13} />
                </button>
                <button
                  className="icon-btn"
                  type="button"
                  title={t("common.delete")}
                  onClick={() => setConfirmDelete(tag)}
                >
                  <Icon name="trash" size={13} />
                </button>
              </div>
            ))}
          </div>
          {totalPages > 1 && (
            <div className="s-tag-pager">
              <span className="s-tag-pager-label">
                {t("settings.autoTag.pageOf", {
                  current: safePage + 1,
                  total: totalPages,
                })}
              </span>
              <button
                type="button"
                className="s-btn"
                disabled={safePage <= 0}
                onClick={() => setPage((p) => Math.max(0, p - 1))}
                aria-label={t("settings.autoTag.prevPage")}
              >
                {t("settings.autoTag.prevPage")}
              </button>
              <button
                type="button"
                className="s-btn"
                disabled={safePage >= totalPages - 1}
                onClick={() =>
                  setPage((p) => Math.min(totalPages - 1, p + 1))
                }
                aria-label={t("settings.autoTag.nextPage")}
              >
                {t("settings.autoTag.nextPage")}
              </button>
            </div>
          )}
        </>
      )}
    </div>
  );

  const renderAliases = () => (
    <div className="settings-group">
      <h3 className="settings-group-title" style={{ margin: 0 }}>
        {t("settings.autoTag.aliases")}
      </h3>
      <p className="settings-group-desc" style={{ margin: "4px 0 12px" }}>
        {t("settings.autoTag.aliasesDesc")}
      </p>
      {interestList.length === 0 ? (
        <p className="settings-group-desc">
          {t("settings.autoTag.aliasesNeedTags")}
        </p>
      ) : (
        <>
          <div className="s-alias-form">
            <label className="s-alias-field">
              <span>{t("settings.autoTag.aliasCanonical")}</span>
              <select
                className="s-text-input"
                value={aliasTagId === "" ? "" : String(aliasTagId)}
                onChange={(e) => {
                  const v = e.target.value;
                  setAliasTagId(v ? Number(v) : "");
                }}
              >
                {interestList.map((tg) => (
                  <option key={tg.id} value={tg.id}>
                    {tg.name}
                  </option>
                ))}
              </select>
            </label>
            <label className="s-alias-field s-alias-field-grow">
              <span>{t("settings.autoTag.aliasName")}</span>
              <input
                className="s-text-input"
                value={aliasDraft}
                placeholder={t("settings.autoTag.aliasPlaceholder")}
                onChange={(e) => setAliasDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    void addAlias();
                  }
                }}
                {...NO_AUTOCORRECT}
              />
            </label>
            <button
              className="s-btn primary"
              type="button"
              disabled={aliasTagId === "" || !aliasDraft.trim()}
              onClick={() => void addAlias()}
            >
              <Icon name="plus" size={12} /> {t("common.add")}
            </button>
          </div>
          {aliasRows.length > 0 && (
            <input
              className="s-text-input s-alias-filter"
              value={aliasFilter}
              placeholder={t("settings.autoTag.aliasFilter")}
              onChange={(e) => setAliasFilter(e.target.value)}
              {...NO_AUTOCORRECT}
            />
          )}
          {filteredAliases.length === 0 ? (
            <p className="settings-group-desc">
              {aliasRows.length === 0
                ? t("settings.autoTag.aliasesEmpty")
                : t("settings.autoTag.aliasesFilterEmpty")}
            </p>
          ) : (
            <div className="s-interest-tags">
              {filteredAliases.map((row) => (
                <div key={row.id} className="s-interest-tag-row">
                  <span className="s-interest-tag-name">{row.alias}</span>
                  <span className="s-interest-tag-count">
                    → {row.tagName}
                  </span>
                  <button
                    className="icon-btn"
                    type="button"
                    title={t("common.delete")}
                    onClick={() => setConfirmDeleteAlias(row)}
                  >
                    <Icon name="trash" size={13} />
                  </button>
                </div>
              ))}
            </div>
          )}
        </>
      )}
    </div>
  );

  return (
    <>
      <div className="s-tag-mgmt-tabs" role="tablist">
        <button
          type="button"
          role="tab"
          aria-selected={tab === "ai"}
          className={tab === "ai" ? "on" : ""}
          onClick={() => switchTab("ai")}
        >
          {t("settings.autoTag.tabAi")}
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={tab === "interest"}
          className={tab === "interest" ? "on" : ""}
          onClick={() => switchTab("interest")}
        >
          {t("settings.autoTag.tabInterest")}
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={tab === "aliases"}
          className={tab === "aliases" ? "on" : ""}
          onClick={() => switchTab("aliases")}
        >
          {t("settings.autoTag.tabAliases")}
        </button>
      </div>

      {tab === "ai" ? (
        <>
          {renderTagRows("settings.autoTag.aiVocabularyEmpty", false)}
          <div className="settings-group">
            <Row
              label={t("settings.autoTag.aiEnabled")}
              desc={t("settings.autoTag.aiEnabledDesc")}
            >
              <Toggle
                checked={aiEnabled}
                onChange={(on) => {
                  setAiEnabled(on);
                  saveSetting("ai_tag_enabled", on ? "1" : "0");
                }}
              />
            </Row>
            <Row
              label={t("settings.autoTag.aiMaxTotal")}
              desc={t("settings.autoTag.aiMaxTotalDesc")}
            >
              <input
                className="s-text-input"
                type="number"
                min={1}
                max={30}
                value={aiMax}
                onChange={(e) => setAiMax(Number(e.target.value) || 1)}
                onBlur={() => {
                  const v = clampSetting(String(aiMax), 5, 1, 30);
                  setAiMax(v);
                  saveSetting("ai_tag_max_tags_per_article", String(v));
                }}
              />
            </Row>
          </div>
        </>
      ) : tab === "interest" ? (
        <>
          {renderTagRows("settings.autoTag.vocabularyEmpty", true)}
          <div className="settings-group">
            <Row
              label={t("settings.autoTag.enabled")}
              desc={t("settings.autoTag.enabledDesc")}
            >
              <Toggle
                checked={interestEnabled}
                onChange={(on) => {
                  setInterestEnabled(on);
                  saveSetting("auto_tag_enabled", on ? "1" : "0");
                }}
              />
            </Row>
            <Row
              label={t("settings.autoTag.maxTotal")}
              desc={t("settings.autoTag.maxTotalDesc")}
            >
              <input
                className="s-text-input"
                type="number"
                min={1}
                max={30}
                value={interestMax}
                onChange={(e) => setInterestMax(Number(e.target.value) || 1)}
                onBlur={() => {
                  const v = clampSetting(String(interestMax), 5, 1, 30);
                  setInterestMax(v);
                  saveSetting("auto_tag_max_tags_per_article", String(v));
                }}
              />
            </Row>
          </div>
        </>
      ) : (
        renderAliases()
      )}

      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.autoTag.queue")}</h3>
        {statusError || status.isError ? (
          <p className="settings-group-desc">
            {t("settings.autoTag.queueUnavailable")}
          </p>
        ) : st ? (
          <>
            <div
              style={{
                display: "flex",
                flexWrap: "wrap",
                gap: 10,
                fontSize: 13,
                color: "var(--ink-2)",
                padding: "4px 0 10px",
              }}
            >
              {st.pending != null && (
                <span>
                  {t("settings.autoTag.pending", { count: st.pending })}
                </span>
              )}
              {st.processing != null && (
                <span>
                  {t("settings.autoTag.processing", { count: st.processing })}
                </span>
              )}
              {st.failed != null && (
                <span>
                  {t("settings.autoTag.failed", { count: st.failed })}
                </span>
              )}
              {st.done != null && (
                <span>{t("settings.autoTag.done", { count: st.done })}</span>
              )}
            </div>
            {st.lastError && (
              <Row label={t("settings.autoTag.lastError")}>
                <span
                  className="s-value"
                  style={{ maxWidth: 280, textAlign: "right" }}
                >
                  {st.lastError}
                </span>
              </Row>
            )}
          </>
        ) : (
          <p className="settings-group-desc">{t("common.loading")}</p>
        )}
      </div>

      <div className="settings-group">
        <h3 className="settings-group-title">{t("settings.autoTag.backfill")}</h3>
        <p className="settings-group-desc">
          {t("settings.autoTag.backfillDesc")}
        </p>
        <Row
          label={t("settings.autoTag.backfillDays")}
          desc={t("settings.autoTag.backfillDaysHint")}
        >
          <input
            className="s-text-input"
            type="number"
            min={0}
            max={365}
            value={backfillDays}
            onChange={(e) =>
              setBackfillDays(clampSetting(e.target.value, 7, 0, 365))
            }
          />
        </Row>
        {st?.articlesInWindow != null && st.untaggedInWindow != null && (
          <p className="settings-group-desc" style={{ marginTop: -4 }}>
            {t(
              backfillDays === 0
                ? "settings.autoTag.backfillWindowHintAll"
                : "settings.autoTag.backfillWindowHint",
              {
                untagged: st.untaggedInWindow,
                total: st.articlesInWindow,
              },
            )}
          </p>
        )}
        <Row
          label={t("settings.autoTag.backfillForce")}
          desc={t("settings.autoTag.backfillForceDesc")}
        >
          <Toggle checked={backfillForce} onChange={setBackfillForce} />
        </Row>
        <p className="settings-group-desc" style={{ marginTop: 4 }}>
          {t("settings.autoTag.clearQueueDesc")}
        </p>
        <div
          style={{
            display: "flex",
            justifyContent: "flex-end",
            gap: 8,
            paddingTop: 8,
          }}
        >
          <button
            className="s-btn danger"
            type="button"
            disabled={clearBusy || backfillBusy}
            onClick={() => setConfirmClearQueue(true)}
          >
            {t("settings.autoTag.clearQueue")}
          </button>
          <button
            className="s-btn primary"
            type="button"
            disabled={backfillBusy || clearBusy}
            onClick={() => void runBackfill()}
          >
            {t("settings.autoTag.backfillRun")}
          </button>
        </div>
      </div>

      {prompt && (
        <PromptDialog
          title={prompt.title}
          initialValue={prompt.initial}
          placeholder={t("settings.autoTag.tagPlaceholder")}
          onSubmit={(v) => {
            prompt.onSubmit(v);
            setPrompt(null);
          }}
          onClose={() => setPrompt(null)}
        />
      )}
      {confirmDelete && (
        <ConfirmDialog
          title={
            confirmDelete.kind === "ai"
              ? t("settings.autoTag.deleteAiTag")
              : t("settings.autoTag.deleteTag")
          }
          message={t(
            confirmDelete.kind === "ai"
              ? "settings.autoTag.deleteAiConfirm"
              : "settings.autoTag.deleteConfirm",
            { name: confirmDelete.name },
          )}
          confirmLabel={t("common.delete")}
          danger
          onConfirm={() => void removeTag(confirmDelete)}
          onClose={() => setConfirmDelete(null)}
        />
      )}
      {confirmDeleteAlias && (
        <ConfirmDialog
          title={t("settings.autoTag.deleteAlias")}
          message={t("settings.autoTag.deleteAliasConfirm", {
            alias: confirmDeleteAlias.alias,
            tag: confirmDeleteAlias.tagName,
          })}
          confirmLabel={t("common.delete")}
          danger
          onConfirm={() => void removeAlias(confirmDeleteAlias)}
          onClose={() => setConfirmDeleteAlias(null)}
        />
      )}
      {confirmCleanupEmpty && (
        <ConfirmDialog
          title={t("settings.autoTag.cleanupEmpty")}
          message={
            emptyAiCount > 0
              ? t("settings.autoTag.cleanupEmptyConfirm", {
                  count: emptyAiCount,
                })
              : t("settings.autoTag.cleanupEmptyConfirmUnknown")
          }
          confirmLabel={t("settings.autoTag.cleanupEmpty")}
          danger
          onConfirm={() => void cleanupEmptyAiTags()}
          onClose={() => setConfirmCleanupEmpty(false)}
        />
      )}
      {confirmClearQueue && (
        <ConfirmDialog
          title={t("settings.autoTag.clearQueue")}
          message={t("settings.autoTag.clearQueueConfirm")}
          confirmLabel={t("settings.autoTag.clearQueue")}
          danger
          onConfirm={() => void runClearQueue()}
          onClose={() => setConfirmClearQueue(false)}
        />
      )}
    </>
  );
}

/* ── filters ─────────────────────────────────────────────── */
function FiltersSection({
  feeds,
  onToast,
}: {
  feeds: Feed[];
  onToast: (m: string) => void;
}) {
  const { t } = useTranslation();
  const qc = useQueryClient();
  const rules = useQuery({ queryKey: ["rules"], queryFn: api.listRules });
  // `null` = not editing, "new" = the add form, a Rule = editing that rule.
  const [editing, setEditing] = useState<Rule | "new" | null>(null);

  const refresh = () => qc.invalidateQueries({ queryKey: ["rules"] });
  // Saving a rule also backfills existing articles (star / read / skip), so
  // refresh the article list, sidebar counts and per-feed counts the backfill
  // touched — not just the rule list. The editor shows the "saved"/"applied"
  // toast itself, since only it knows how many articles were affected.
  const afterSaved = () => {
    setEditing(null);
    refresh();
    qc.invalidateQueries({ queryKey: ["articles"] });
    qc.invalidateQueries({ queryKey: ["counts"] });
    qc.invalidateQueries({ queryKey: ["feeds"] });
  };
  const feedName = (id: number | null) =>
    id == null
      ? t("settings.filters.allFeeds")
      : feeds.find((f) => f.id === id)?.title ?? t("settings.filters.allFeeds");

  const toggle = (r: Rule) =>
    api
      .updateRule(r.id, r.name, !r.enabled, r.feedId, r.field, r.query, r.action)
      .then(refresh)
      .catch((e) => reportError(e));

  const remove = (r: Rule) =>
    api
      .deleteRule(r.id)
      .then(() => {
        refresh();
        onToast(t("settings.filters.deleted"));
      })
      .catch((e) => reportError(e));

  const summary = (r: Rule) =>
    [
      t(`settings.filters.action.${r.action}`),
      "·",
      t(`settings.filters.field.${r.field}`),
      `“${r.query}”`,
      "·",
      feedName(r.feedId),
    ].join(" ");

  const list = rules.data ?? [];

  return (
    <>
      <div className="settings-group" style={{ marginBottom: 18 }}>
        <h3 className="settings-group-title">{t("settings.filters.title")}</h3>
        <p className="settings-group-desc">{t("settings.filters.intro")}</p>
        {editing !== "new" && (
          <button
            className="s-btn primary"
            style={{ marginTop: 10 }}
            onClick={() => setEditing("new")}
          >
            <Icon name="plus" size={12} /> {t("settings.filters.newRule")}
          </button>
        )}
      </div>

      {editing === "new" && (
        <RuleEditor
          rule={null}
          feeds={feeds}
          onCancel={() => setEditing(null)}
          onSaved={afterSaved}
          onToast={onToast}
        />
      )}

      <div className="settings-group">
        {list.length === 0 && editing !== "new" && (
          <div style={{ padding: "16px 4px", fontSize: 13, color: "var(--muted)" }}>
            {t("settings.filters.empty")}
          </div>
        )}
        {list.map((r) =>
          editing !== "new" && typeof editing === "object" && editing?.id === r.id ? (
            <RuleEditor
              key={r.id}
              rule={r}
              feeds={feeds}
              onCancel={() => setEditing(null)}
              onSaved={afterSaved}
              onToast={onToast}
            />
          ) : (
            <div className="rule-row" key={r.id}>
              <Toggle checked={r.enabled} onChange={() => toggle(r)} />
              <div className="rule-text" style={{ opacity: r.enabled ? 1 : 0.5 }}>
                <div className="rule-name">
                  {r.name || t("settings.filters.untitled")}
                </div>
                <div className="rule-summary">{summary(r)}</div>
              </div>
              <div className="actions">
                <button
                  className="icon-btn"
                  title={t("common.rename")}
                  onClick={() => setEditing(r)}
                >
                  <Icon name="settings" size={13} />
                </button>
                <button
                  className="icon-btn"
                  title={t("common.delete")}
                  onClick={() => remove(r)}
                >
                  <Icon name="trash" size={13} />
                </button>
              </div>
            </div>
          ),
        )}
      </div>
    </>
  );
}

function RuleEditor({
  rule,
  feeds,
  onCancel,
  onSaved,
  onToast,
}: {
  rule: Rule | null;
  feeds: Feed[];
  onCancel: () => void;
  onSaved: () => void;
  onToast: (m: string) => void;
}) {
  const { t } = useTranslation();
  const [name, setName] = useState(rule?.name ?? "");
  const [query, setQuery] = useState(rule?.query ?? "");
  const [field, setField] = useState<RuleField>(rule?.field ?? "title");
  const [action, setAction] = useState<RuleAction>(rule?.action ?? "skip");
  const [scope, setScope] = useState(rule?.feedId == null ? "" : String(rule.feedId));
  const [busy, setBusy] = useState(false);
  const [preview, setPreview] = useState<RulePreview | null>(null);
  const [previewing, setPreviewing] = useState(false);
  // When a `skip` rule would delete existing matches, hold that count here to
  // pop a confirmation before the destructive backfill runs.
  const [pendingDelete, setPendingDelete] = useState<number | null>(null);

  // Debounced dry-run: count matching stored articles as the draft changes.
  useEffect(() => {
    const q = query.trim();
    if (!q) {
      setPreview(null);
      return;
    }
    setPreviewing(true);
    const feedId = scope === "" ? null : Number(scope);
    // `cancelled` guards against a stale response: a request started before
    // the draft changed could otherwise resolve last and overwrite the
    // preview for the current draft.
    let cancelled = false;
    const handle = window.setTimeout(() => {
      api
        .previewRule(feedId, field, q)
        .then((r) => !cancelled && setPreview(r))
        .catch(() => !cancelled && setPreview(null))
        .finally(() => !cancelled && setPreviewing(false));
    }, 400);
    return () => {
      cancelled = true;
      window.clearTimeout(handle);
    };
  }, [query, field, scope]);

  // A rule backfills the existing backlog only when it's active: new rules
  // start enabled, and an existing rule keeps its current enabled state (the
  // editor has no enabled toggle — that lives in the rule list).
  const willApply = rule ? rule.enabled : true;

  // Save the rule, then backfill the existing articles it matches. Split out
  // from `save` so the `skip` confirmation can resume here after the user agrees.
  const persist = async () => {
    setBusy(true);
    const feedId = scope === "" ? null : Number(scope);
    const q = query.trim();
    try {
      if (rule) {
        await api.updateRule(rule.id, name, rule.enabled, feedId, field, q, action);
      } else {
        await api.createRule(name, feedId, field, q, action);
      }
      const applied = willApply
        ? await api.applyRuleToExisting(feedId, field, q, action)
        : 0;
      onToast(
        applied > 0
          ? t("settings.filters.appliedExisting", { count: applied })
          : t("settings.filters.saved"),
      );
      onSaved();
    } catch (e) {
      reportError(e);
      setBusy(false);
    }
  };

  const save = async () => {
    const q = query.trim();
    if (!q) {
      onToast(t("settings.filters.needQuery"));
      return;
    }
    // `skip` deletes the existing articles it matches — confirm first, showing
    // the exact count fetched fresh (not the debounced preview, which may lag).
    if (willApply && action === "skip") {
      setBusy(true);
      const feedId = scope === "" ? null : Number(scope);
      try {
        const p = await api.previewRule(feedId, field, q);
        if (p.count > 0) {
          setPendingDelete(p.count);
          return;
        }
      } catch (e) {
        // Never fall through to the destructive backfill when we couldn't
        // confirm how many articles it would delete.
        reportError(e);
        return;
      } finally {
        setBusy(false);
      }
    }
    await persist();
  };

  return (
    <div className="rule-card">
      <input
        className="rule-input"
        {...NO_AUTOCORRECT}
        value={name}
        onChange={(e) => setName(e.target.value)}
        placeholder={t("settings.filters.namePlaceholder")}
      />
      <input
        className="rule-input"
        {...NO_AUTOCORRECT}
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        placeholder={t("settings.filters.queryPlaceholder")}
      />
      <div className="rule-fields">
        <label>
          {t("settings.filters.matchIn")}
          <Select
            value={field}
            onChange={(v) => setField(v as RuleField)}
            options={[
              { value: "title", label: t("settings.filters.field.title") },
              { value: "author", label: t("settings.filters.field.author") },
              { value: "content", label: t("settings.filters.field.content") },
              { value: "any", label: t("settings.filters.field.any") },
            ]}
          />
        </label>
        <label>
          {t("settings.filters.thenLabel")}
          <Select
            value={action}
            onChange={(v) => setAction(v as RuleAction)}
            options={[
              { value: "skip", label: t("settings.filters.action.skip") },
              { value: "read", label: t("settings.filters.action.read") },
              { value: "star", label: t("settings.filters.action.star") },
            ]}
          />
        </label>
        <label>
          {t("settings.filters.scopeLabel")}
          <Select
            value={scope}
            onChange={setScope}
            options={[
              { value: "", label: t("settings.filters.allFeeds") },
              ...feeds.map((f) => ({ value: String(f.id), label: f.title })),
            ]}
          />
        </label>
      </div>
      {query.trim() && (
        <div className="rule-preview">
          <span className="rule-preview-count">
            {previewing && !preview
              ? t("settings.filters.preview.checking")
              : t("settings.filters.preview.count", {
                  count: preview?.count ?? 0,
                })}
          </span>
          {preview && preview.samples.length > 0 && (
            <ul className="rule-preview-samples">
              {preview.samples.map((s, i) => (
                <li key={i}>{s}</li>
              ))}
            </ul>
          )}
        </div>
      )}
      <div className="rule-card-actions">
        <button className="s-btn" onClick={onCancel} disabled={busy}>
          {t("common.cancel")}
        </button>
        <button className="s-btn primary" onClick={save} disabled={busy}>
          {t("common.save")}
        </button>
      </div>
      {pendingDelete != null && (
        <ConfirmDialog
          title={t("settings.filters.confirmSkipTitle")}
          message={t("settings.filters.confirmSkipMessage", { count: pendingDelete })}
          confirmLabel={t("settings.filters.confirmSkipConfirm")}
          onConfirm={persist}
          onClose={() => setPendingDelete(null)}
        />
      )}
    </div>
  );
}

/* ── about ───────────────────────────────────────────────── */
type AboutTab = "about" | "help";

function AboutSection() {
  const { t, i18n } = useTranslation();
  const version = useAppVersion();
  const [tab, setTab] = useState<AboutTab>("about");
  const helpHtml = useMemo(() => renderMarkdown(userGuideMd), []);
  const showHelpLangNote = !i18n.language.startsWith("zh");

  return (
    <div className="s-about-wrap">
      <div className="s-tag-mgmt-tabs" role="tablist" aria-label={t("settings.nav.about")}>
        <button
          type="button"
          role="tab"
          aria-selected={tab === "about"}
          className={tab === "about" ? "on" : ""}
          onClick={() => setTab("about")}
        >
          {t("settings.about.tabAbout")}
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={tab === "help"}
          className={tab === "help" ? "on" : ""}
          onClick={() => setTab("help")}
        >
          {t("settings.about.tabHelp")}
        </button>
      </div>

      {tab === "about" ? (
        <div className="s-about" role="tabpanel">
          <div className="mark">
            <Icon name="papr" size={34} color="#fff" />
          </div>
          <p className="tagline">{t("settings.about.tagline")}</p>
          <div className="version">
            Version{version && ` ${version}`}
          </div>
          <p className="credits">
            {/* {t("settings.about.creditsFonts")} */}
            {/* <br />
            {t("settings.about.creditsRender")}
            <br />
            {t("settings.about.creditsThanks")} */}
          </p>
        </div>
      ) : (
        <div className="s-about-help" role="tabpanel">
          {showHelpLangNote && (
            <p className="s-about-help-note">{t("settings.about.helpLangNote")}</p>
          )}
          <div
            className="s-about-help-body"
            dangerouslySetInnerHTML={{ __html: helpHtml }}
          />
        </div>
      )}
    </div>
  );
}
