// <Button> — the text button (UX audit §3.6). Forwards its ref and every
// native <button> attribute; `type` defaults to "button".
//
//   variant?:   'primary'   accent fill (--accent-fill) with a white label
//               'secondary' outlined (--border-strong), primary text — the default
//               'ghost'     no box until hovered
//               'danger'    red fill with a white label, for the final step of a
//                           destructive action
//   size?:      'sm' 28px | 'md' 36px (default) | 'lg' 44px. Every size is at
//               least 44px tall on narrow screens (touch target).
//   icon?:      a lucide icon shown before the label
//   loading?:   shows a spinner in place of the icon, sets aria-busy and
//               disables the button
//   fullWidth?: stretches to the container's width
import React, { forwardRef } from 'react';
import type { LucideIcon } from 'lucide-react';
import Spinner from './Spinner';

export type ButtonVariant = 'primary' | 'secondary' | 'ghost' | 'danger';
export type ButtonSize = 'sm' | 'md' | 'lg';

export interface ButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  icon?: LucideIcon;
  loading?: boolean;
  fullWidth?: boolean;
}

const BASE =
  'u-press inline-flex items-center justify-center gap-1.5 rounded-md font-medium whitespace-nowrap cursor-pointer narrow:min-h-11 disabled:cursor-not-allowed disabled:opacity-50';

const VARIANTS: Record<ButtonVariant, string> = {
  primary: 'bg-accent-fill text-white hover:bg-accent-hover',
  secondary: 'border border-strong bg-transparent text-primary hover:bg-hover',
  ghost: 'bg-transparent text-secondary hover:bg-hover hover:text-primary',
  danger: 'bg-red-700 text-white hover:bg-red-600',
};

const SIZES: Record<ButtonSize, string> = {
  sm: 'h-7 px-2.5 text-xs',
  md: 'h-9 px-3.5 text-sm',
  lg: 'h-11 px-4 text-sm',
};

const ICON_SIZE: Record<ButtonSize, number> = { sm: 13, md: 14, lg: 16 };

const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  {
    variant = 'secondary',
    size = 'md',
    icon: Icon,
    loading = false,
    fullWidth = false,
    type = 'button',
    disabled,
    className = '',
    children,
    ...rest
  },
  ref,
) {
  const iconSize = ICON_SIZE[size];
  return (
    <button
      ref={ref}
      type={type}
      disabled={disabled || loading}
      aria-busy={loading || undefined}
      className={[BASE, VARIANTS[variant], SIZES[size], fullWidth ? 'w-full' : '', className]
        .filter(Boolean)
        .join(' ')}
      {...rest}
    >
      {loading ? (
        <Spinner size={iconSize} />
      ) : Icon ? (
        <Icon size={iconSize} aria-hidden="true" className="shrink-0" />
      ) : null}
      {children}
    </button>
  );
});

export default Button;
