import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import * as api from "../api";
import { useAuth } from "../auth";
import { useDismiss } from "../hooks/useDismiss";
import { reportError } from "../toast";
import { tagColor } from "../lib/tagColors";
import { clampToViewport } from "../lib/viewport";
import { NO_AUTOCORRECT } from "../lib/inputProps";
import type { Tag } from "../types";
import Icon from "./Icon";
import PromptDialog from "./PromptDialog";

interface Props {
  articleId: number;
  /** Tags already attached to the article (interest + AI). */
  attachedTags: Tag[];
  /** Anchor point (viewport coords) the popover opens from. */
  x: number;
  y: number;
  onClose: () => void;
}

/**
 * Floating tag editor for a single article.
 *
 * - No tags → primary “AI auto-tag” action (any signed-in reader).
 * - Has tags → current chips (detach for anyone; rename global for admin),
 *   optional “AI tag again”, and admin interest vocabulary toggles + create.
 */
export default function TagPicker({
  articleId,
  attachedTags,
  x,
  y,
  onClose,
}: Props) {
  const { t } = useTranslation();
  const { user } = useAuth();
  const isAdmin = !!user?.isAdmin;
  const loggedIn = !!user;
  const qc = useQueryClient();
  const ref = useRef<HTMLDivElement>(null);
  const [draft, setDraft] = useState("");
  const [aiBusy, setAiBusy] = useState(false);
  const [rename, setRename] = useState<Tag | null>(null);

  // Admin attach uses the closed interest vocabulary; AI tags show on the
  // article chips and are created by auto-tag.
  const tags = useQuery({
    queryKey: ["tags", "interest"],
    queryFn: () => api.listTags("interest"),
    enabled: isAdmin,
  });
  const attachedSet = new Set(attachedTags.map((tg) => tg.id));
  const hasTags = attachedTags.length > 0;

  useDismiss(ref, onClose, { onFocusOut: true });

  useEffect(() => {
    const trigger = document.activeElement as HTMLElement | null;
    ref.current
      ?.querySelector<HTMLElement>("button, [role='button'], input")
      ?.focus();
    return () => trigger?.focus?.();
  }, []);

  const sync = () => {
    qc.invalidateQueries({ queryKey: ["article", articleId] });
    qc.invalidateQueries({ queryKey: ["tags"] });
  };

  const detach = (tagId: number) => {
    if (!loggedIn) return;
    api
      .setArticleTag(articleId, tagId, false)
      .then(sync)
      .catch((e) => reportError(e));
  };

  const toggle = (tagId: number, on: boolean) => {
    if (!isAdmin) return;
    api
      .setArticleTag(articleId, tagId, on)
      .then(sync)
      .catch((e) => reportError(e));
  };

  const createAndAttach = async () => {
    if (!isAdmin) return;
    const name = draft.trim();
    if (!name) return;
    try {
      const id = await api.createTag(name, "interest");
      await api.setArticleTag(articleId, id, true);
      setDraft("");
      sync();
    } catch (e) {
      reportError(e);
    }
  };

  const runAiTag = async () => {
    if (!loggedIn || aiBusy) return;
    setAiBusy(true);
    try {
      await api.autoTagArticle(articleId);
      sync();
    } catch (e) {
      reportError(e);
    } finally {
      setAiBusy(false);
    }
  };

  const { left, top } = clampToViewport({
    x,
    y,
    width: 268,
    height: 380,
    margin: 0,
  });

  const showAi = loggedIn;
  const showVocab = isAdmin;
  const showCurrent = hasTags;

  return (
    <>
      <div className="tag-picker" ref={ref} style={{ left, top }}>
        <div className="tag-picker-head">{t("tagPicker.title")}</div>

        {showCurrent && (
          <div className="tag-picker-current">
            <div className="tag-picker-section-label">
              {t("tagPicker.current")}
            </div>
            <div className="tag-picker-chips">
              {attachedTags.map((tag) => (
                <div
                  key={tag.id}
                  className={`tag-picker-chip${tag.kind === "ai" ? " ai" : ""}`}
                  style={
                    { "--tag-c": tagColor(tag.color) } as React.CSSProperties
                  }
                  title={
                    tag.kind === "ai"
                      ? t("reader.aiTagHint")
                      : t("reader.interestTagHint")
                  }
                >
                  <span className="tag-dot" />
                  {isAdmin ? (
                    <button
                      type="button"
                      className="tag-picker-chip-name"
                      onClick={() => setRename(tag)}
                      title={t("tagPicker.rename")}
                    >
                      {tag.name}
                    </button>
                  ) : (
                    <span className="tag-picker-chip-name">{tag.name}</span>
                  )}
                  {loggedIn && (
                    <button
                      type="button"
                      className="tag-picker-chip-remove"
                      onClick={() => detach(tag.id)}
                      title={t("tagPicker.detach")}
                      aria-label={t("tagPicker.detach")}
                    >
                      <Icon name="x" size={11} />
                    </button>
                  )}
                </div>
              ))}
            </div>
          </div>
        )}

        {showAi && (
          <>
            {showCurrent && <div className="tag-picker-sep" />}
            <div className="tag-picker-ai">
              <button
                type="button"
                className={`tag-picker-ai-btn${hasTags ? "" : " primary"}${aiBusy ? " busy" : ""}`}
                onClick={runAiTag}
                disabled={aiBusy}
                aria-busy={aiBusy}
                title={t("tagPicker.aiHint")}
              >
                <Icon
                  name="sparkle"
                  size={14}
                  className={aiBusy ? "spinning" : undefined}
                />
                <span>
                  {aiBusy
                    ? t("tagPicker.aiTagging")
                    : hasTags
                      ? t("tagPicker.aiAgain")
                      : t("tagPicker.aiAuto")}
                </span>
              </button>
              <p className="tag-picker-ai-hint">{t("tagPicker.aiHint")}</p>
            </div>
          </>
        )}

        {showVocab && (
          <>
            {(showCurrent || showAi) && <div className="tag-picker-sep" />}
            <div className="tag-picker-section-label">
              {t("tagPicker.interestVocab")}
            </div>
            <div className="tag-picker-list">
              {(tags.data ?? []).map((tag) => {
                const on = attachedSet.has(tag.id);
                return (
                  <div
                    key={tag.id}
                    className={`tag-picker-row ${on ? "on" : ""}`}
                    role="button"
                    tabIndex={0}
                    aria-pressed={on}
                    onClick={() => toggle(tag.id, !on)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" || e.key === " ") {
                        e.preventDefault();
                        toggle(tag.id, !on);
                      }
                    }}
                  >
                    <span
                      className="tag-dot"
                      style={{ background: tagColor(tag.color) }}
                    />
                    <span className="tag-picker-name">{tag.name}</span>
                    {on && <Icon name="check" size={13} />}
                  </div>
                );
              })}
              {(tags.data ?? []).length === 0 && (
                <div className="tag-picker-empty">{t("tagPicker.empty")}</div>
              )}
            </div>
            <div className="tag-picker-create">
              <input
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.nativeEvent.isComposing) {
                    createAndAttach();
                  }
                }}
                placeholder={t("tagPicker.createPlaceholder")}
                aria-label={t("tagPicker.createPlaceholder")}
                {...NO_AUTOCORRECT}
              />
              <button
                type="button"
                onClick={createAndAttach}
                disabled={!draft.trim()}
              >
                <Icon name="plus" size={13} />
              </button>
            </div>
          </>
        )}

        {!isAdmin && !hasTags && !loggedIn && (
          <div className="tag-picker-empty">{t("tagPicker.empty")}</div>
        )}
      </div>

      {rename && (
        <PromptDialog
          title={
            rename.kind === "ai"
              ? t("tagPicker.renameAi")
              : t("tagPicker.rename")
          }
          initialValue={rename.name}
          onSubmit={(name) => {
            api
              .renameTag(rename.id, name)
              .then(sync)
              .catch((e) => reportError(e));
          }}
          onClose={() => setRename(null)}
        />
      )}
    </>
  );
}
