// <IconButton> — a button that shows only an icon (UX audit §3.6). Forwards
// its ref and every native <button> attribute; `type` defaults to "button".
//
//   label:      REQUIRED. Becomes the accessible name (aria-label) and the
//               hover tooltip (title).
//   icon:       a lucide icon
//   size?:      'sm' 28px | 'md' 32px (default) | 'lg' 36px
//   narrow?:    on narrow screens, 'grow' (default) makes the button 44×44;
//               'hit' keeps its visual size and extends the hit area to 44px
//               with `.u-hit` (dense rows, the card checkbox)
//   tone?:      'neutral' (default) | 'danger' (turns red on hover)
//   iconSize?:  overrides the size's icon (14 for sm, 16 for md and lg)
//   children?:  extra content after the icon, such as a count badge
//               (position it absolutely; the button is `relative`)
import React, { forwardRef } from 'react';
import type { LucideIcon } from 'lucide-react';

export type IconButtonSize = 'sm' | 'md' | 'lg';

export interface IconButtonProps extends Omit<
  React.ButtonHTMLAttributes<HTMLButtonElement>,
  'aria-label'
> {
  label: string;
  icon: LucideIcon;
  size?: IconButtonSize;
  narrow?: 'grow' | 'hit';
  tone?: 'neutral' | 'danger';
  iconSize?: number;
}

const SIZES: Record<IconButtonSize, string> = {
  sm: 'h-7 w-7',
  md: 'h-8 w-8',
  lg: 'h-9 w-9',
};

const TONES = {
  neutral: 'text-secondary hover:bg-hover hover:text-primary',
  danger: 'text-secondary hover:bg-error/10 hover:text-error',
} as const;

const IconButton = forwardRef<HTMLButtonElement, IconButtonProps>(function IconButton(
  {
    label,
    icon: Icon,
    size = 'md',
    narrow = 'grow',
    tone = 'neutral',
    iconSize,
    type = 'button',
    title,
    className = '',
    children,
    ...rest
  },
  ref,
) {
  return (
    <button
      ref={ref}
      type={type}
      aria-label={label}
      title={title ?? label}
      className={[
        'u-press relative inline-flex shrink-0 items-center justify-center rounded-md cursor-pointer disabled:cursor-not-allowed disabled:opacity-50',
        SIZES[size],
        narrow === 'grow' ? 'narrow:h-11 narrow:w-11' : 'u-hit',
        TONES[tone],
        className,
      ]
        .filter(Boolean)
        .join(' ')}
      {...rest}
    >
      <Icon size={iconSize ?? (size === 'sm' ? 14 : 16)} aria-hidden="true" />
      {children}
    </button>
  );
});

export default IconButton;
