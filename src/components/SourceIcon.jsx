import React from 'react';
import { Globe, Bookmark } from 'lucide-react';
import PinterestIcon from './PinterestIcon';

// Single source of truth for the per-platform brand glyph, so the gallery, the
// post modal and the downloads list all show the *same* source icon instead of
// each rolling its own (or a text "IG"/"PIN"/"TW" badge). Lucide has no
// Instagram/X/Pinterest glyph, so the SVGs live here; everything renders in
// currentColor so callers tint it (brand colour or muted) via the parent.

// Brand accent colours, matching PostModal / PostCard.
export const PLATFORM_COLORS = {
  instagram: '#e1306c',
  pinterest: '#e60023',
  twitter: '#1da1f2',
  web: '#7B5CFF',
  manual: '#7B5CFF',
};

export const PLATFORM_LABELS = {
  instagram: 'Instagram',
  pinterest: 'Pinterest',
  twitter: 'Twitter',
  web: 'Web',
  manual: 'Bookmark',
};

function InstagramGlyph({ size, className, ...props }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 12 12"
      fill="none"
      className={className}
      {...props}
    >
      <rect
        x="1.2"
        y="1.2"
        width="9.6"
        height="9.6"
        rx="2.8"
        stroke="currentColor"
        strokeWidth="1.25"
      />
      <circle cx="6" cy="6" r="2.4" stroke="currentColor" strokeWidth="1.25" />
      <circle cx="9.2" cy="2.8" r="0.7" fill="currentColor" />
    </svg>
  );
}

function XGlyph({ size, className, ...props }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="currentColor"
      className={className}
      {...props}
    >
      <path d="M12.6.75h2.454l-5.36 6.142L16 15.25h-4.937l-3.867-5.07-4.425 5.07H.316l5.733-6.57L0 .75h5.063l3.495 4.633L12.601.75Zm-.86 13.028h1.36L4.323 2.145H2.865z" />
    </svg>
  );
}

export default function SourceIcon({ platform, size = 14, className = '', ...props }) {
  if (platform === 'instagram')
    return <InstagramGlyph size={size} className={className} {...props} />;
  if (platform === 'twitter') return <XGlyph size={size} className={className} {...props} />;
  if (platform === 'pinterest')
    return <PinterestIcon size={size} className={className} {...props} />;
  if (platform === 'web') return <Globe size={size} className={className} {...props} />;
  if (platform === 'manual') return <Bookmark size={size} className={className} {...props} />;
  return null;
}
