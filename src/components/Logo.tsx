import React from 'react';

interface LogoProps {
  size?: number;
  className?: string;
}

// SHELFY bookmark mark — canonical brand asset.
// Two-tone purple (#7B5CFF → #5A3DDE) with an origami fold highlight.
export default function Logo({ size = 22, className = '' }: LogoProps): React.ReactElement {
  const height = Math.round((size * 128) / 96);
  return (
    <svg
      width={size}
      height={height}
      viewBox="0 0 96 128"
      fill="none"
      className={`u-fade-in logo-idle ${className}`}
      aria-hidden="true"
    >
      <defs>
        <linearGradient
          id="shelfyMark"
          x1="14"
          y1="6"
          x2="82"
          y2="122"
          gradientUnits="userSpaceOnUse"
        >
          <stop stopColor="#7B5CFF" />
          <stop offset="1" stopColor="#5A3DDE" />
        </linearGradient>
      </defs>
      <path d="M14 6H82V122L48 95L14 122V6Z" fill="url(#shelfyMark)" />
      <path d="M14 6H82L14 64V6Z" fill="#FFFFFF" fillOpacity="0.14" />
    </svg>
  );
}
