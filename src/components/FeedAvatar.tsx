import { useEffect, useState } from "react";
import { feedAvatar, feedColor } from "../lib/feedMeta";

interface Props {
  title: string;
  /** The feed's favicon URL, if the backend resolved one. */
  faviconUrl?: string | null;
  /** Hash seed for the fallback colour — normally the feed id. */
  seed: string | number;
  /** Extra inline styles (size, border-radius) merged onto the avatar. */
  style?: React.CSSProperties;
}

/**
 * A feed's logo. Shows the site's favicon when one is available, and falls
 * back to the coloured letter avatar when there is no icon — or when the
 * icon fails to load.
 */
export default function FeedAvatar({ title, faviconUrl, seed, style }: Props) {
  const [broken, setBroken] = useState(false);

  // The same component instance is reused across list rows as feeds change,
  // so clear the error flag whenever the icon URL does.
  useEffect(() => setBroken(false), [faviconUrl]);

  const showIcon = !!faviconUrl && !broken;

  return (
    <span
      className={`sb-feed-avatar ${showIcon ? "has-icon" : ""}`}
      style={{ background: showIcon ? undefined : feedColor(seed), ...style }}
    >
      {showIcon ? (
        <img
          src={faviconUrl!}
          alt=""
          loading="lazy"
          draggable={false}
          onError={() => setBroken(true)}
        />
      ) : (
        feedAvatar(title)
      )}
    </span>
  );
}
