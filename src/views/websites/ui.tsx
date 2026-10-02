import React, { useEffect, useState } from 'react';
import type { LucideIcon } from 'lucide-react';
import { assetThumbUrl } from '../../lib/asset';

// Small presentational primitives shared by the Websites grid, filters and
// detail panel. Kept dumb on purpose: every label arrives already localised.

export const ACCENT = '#7B5CFF';

// ── Favicon ─────────────────────────────────────────────────────────────────
// The site's own favicon (captured locally). No third-party favicon service: a
// missing/broken icon degrades to a monogram tile tinted with the site's colour.
interface SiteFaviconProps {
  path: string;
  name: string;
  tint?: string;
  size?: number;
  className?: string;
}
export function SiteFavicon({
  path,
  name,
  tint,
  size = 20,
  className = '',
}: SiteFaviconProps): React.ReactElement {
  const [failed, setFailed] = useState(false);
  useEffect(() => setFailed(false), [path]);
  const src = path && !failed ? assetThumbUrl(path, 64) : null;
  const radius = Math.round(size * 0.28);
  if (src) {
    return (
      <img
        src={src}
        alt=""
        width={size}
        height={size}
        draggable={false}
        onError={() => setFailed(true)}
        className={`shrink-0 object-contain ${className}`}
        style={{ width: size, height: size, borderRadius: radius }}
      />
    );
  }
  const letter = (name || '?').trim().charAt(0).toUpperCase() || '?';
  return (
    <span
      aria-hidden
      className={`shrink-0 inline-flex items-center justify-center font-semibold ${className}`}
      style={{
        width: size,
        height: size,
        borderRadius: radius,
        fontSize: Math.round(size * 0.55),
        background: tint || '#2a2a2a',
        color: '#fff',
        boxShadow: 'inset 0 0 0 1px rgba(255,255,255,0.08)',
      }}
    >
      {letter}
    </span>
  );
}

// ── Palette strip ───────────────────────────────────────────────────────────
interface PaletteStripProps {
  colors: string[];
  size?: number;
  title?: (hex: string) => string;
}
export function PaletteStrip({
  colors,
  size = 12,
  title,
}: PaletteStripProps): React.ReactElement | null {
  if (!colors.length) return null;
  return (
    <span className="inline-flex items-center" data-testid="aiweb-palette-strip">
      {colors.map((hex, i) => (
        <span
          key={`${hex}-${i}`}
          title={title ? title(hex) : hex}
          className="rounded-full"
          style={{
            width: size,
            height: size,
            background: hex,
            marginLeft: i ? -Math.round(size * 0.25) : 0,
            boxShadow: '0 0 0 1.5px #161616, inset 0 0 0 1px rgba(255,255,255,0.12)',
          }}
        />
      ))}
    </span>
  );
}

// ── Chips ───────────────────────────────────────────────────────────────────
interface TagChipProps {
  children: React.ReactNode;
  active?: boolean;
  onClick?: () => void;
  title?: string;
  size?: 'xs' | 'sm';
  testId?: string;
  icon?: LucideIcon;
}
export function TagChip({
  children,
  active = false,
  onClick,
  title,
  size = 'sm',
  testId,
  icon: Icon,
}: TagChipProps): React.ReactElement {
  const cls = `inline-flex items-center gap-1 rounded-full whitespace-nowrap u-transition ${
    size === 'xs' ? 'px-2 py-[2px] text-[10.5px]' : 'px-2.5 py-1 text-[11.5px]'
  } ${active ? 'bg-[#7B5CFF] text-white' : 'bg-[#1f1f1f] text-[#bdbdbd] border border-[#2a2a2a]'}`;
  if (onClick) {
    return (
      <button
        type="button"
        data-testid={testId}
        onClick={onClick}
        title={title}
        className={`${cls} u-press ${active ? 'hover:bg-[#6a4cf0]' : 'hover:bg-[#272727] hover:text-white'}`}
      >
        {Icon && <Icon size={11} />}
        {children}
      </button>
    );
  }
  return (
    <span className={cls} title={title} data-testid={testId}>
      {Icon && <Icon size={11} />}
      {children}
    </span>
  );
}

// ── Section title (detail tabs) ─────────────────────────────────────────────
interface BlockTitleProps {
  icon?: LucideIcon;
  children: React.ReactNode;
  count?: number;
  right?: React.ReactNode;
}
export function BlockTitle({
  icon: Icon,
  children,
  count,
  right,
}: BlockTitleProps): React.ReactElement {
  return (
    <div className="flex items-center gap-2 mb-3">
      {Icon && <Icon size={13} className="text-[#7a7a7a]" />}
      <h3 className="text-[11px] font-semibold uppercase tracking-[0.12em] text-[#8a8a8a]">
        {children}
      </h3>
      {typeof count === 'number' && (
        <span className="text-[11px] tabular-nums text-[#5f5f5f]">{count}</span>
      )}
      {right && <div className="ml-auto flex items-center gap-2">{right}</div>}
    </div>
  );
}

// ── Lazy, aspect-reserved image ─────────────────────────────────────────────
// Reserves the image's box from its intrinsic size (no layout shift while
// lazy-loading) and fades it in once decoded.
interface LazyShotProps {
  src: string | null;
  width?: number;
  height?: number;
  alt?: string;
  className?: string;
  style?: React.CSSProperties;
  onClick?: () => void;
  testId?: string;
}
export function LazyShot({
  src,
  width,
  height,
  alt = '',
  className = '',
  style,
  onClick,
  testId,
}: LazyShotProps): React.ReactElement {
  const [loaded, setLoaded] = useState(false);
  const ratio = width && height ? `${width} / ${height}` : '16 / 10';
  return (
    <div
      className={`relative w-full overflow-hidden bg-[#161616] ${onClick ? 'cursor-zoom-in' : ''} ${className}`}
      style={{ aspectRatio: ratio, ...style }}
      onClick={onClick}
      data-testid={testId}
    >
      {src && (
        <img
          src={src}
          alt={alt}
          loading="lazy"
          decoding="async"
          draggable={false}
          onLoad={() => setLoaded(true)}
          className="absolute inset-0 w-full h-full object-cover object-top"
          style={{ opacity: loaded ? 1 : 0, transition: 'opacity var(--dur-2) var(--ease-out)' }}
        />
      )}
    </div>
  );
}

// ── Empty-state line ────────────────────────────────────────────────────────
export function Muted({ children }: { children: React.ReactNode }): React.ReactElement {
  return <p className="text-[12.5px] text-[#6b6b6b] leading-relaxed">{children}</p>;
}
