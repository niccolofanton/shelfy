// <Spinner> — the one loading indicator (UX audit §3.6), in place of ad hoc
// `<Loader2 className="animate-spin" />` copies.
//
//   size?:      icon size in px (default 14)
//   label?:     standalone use: announced as `role="status"` with this name.
//               Without it the spinner is decorative (`aria-hidden`), for use
//               inside a control or next to text that already says "Loading…".
//   className?: color and spacing (it inherits the text color)
import React from 'react';
import { Loader2 } from 'lucide-react';

export interface SpinnerProps {
  size?: number;
  label?: string;
  className?: string;
}

export default function Spinner({
  size = 14,
  label,
  className = '',
}: SpinnerProps): React.JSX.Element {
  const icon = (
    <Loader2 size={size} aria-hidden="true" className={`animate-spin shrink-0 ${className}`} />
  );
  if (!label) return icon;
  return (
    <span role="status" aria-label={label} className="inline-flex">
      {icon}
    </span>
  );
}
