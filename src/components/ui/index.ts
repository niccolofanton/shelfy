// Shared UI primitives (UX audit §3.6, UX-2). Each file documents its props in
// its header; in short:
//
//   <Button variant size icon loading fullWidth …button attrs>  text button
//   <IconButton label icon size narrow tone iconSize …>          icon-only button, `label` required
//   <EmptyState icon title body action secondaryAction>          empty lists and views
//   <Notice tone="info|ok|warn|error">                          one-line message (error = role="alert")
//   <Spinner size label>                                        loading indicator
//   <PageHeader title leading count subtitle actions>           56px view header (Trash, Jobs)
//   <ToastHost toasts={useToasts()}>                            toasts (src/hooks/useToast.ts)
//   useMediaQuery(query), NARROW_QUERY                          the `narrow:` screen in JS
//   COLORS, GRAY, Z                                             design tokens (./tokens.ts)
//
// Elsewhere: useDialog() (src/hooks/useDialog.ts) for modals, sheets and the
// drawer; useReducedMotion() (src/hooks/useReducedMotion.ts); Popover's
// `presentation="auto"` bottom sheet and `role="menu"` keyboard handling
// (src/components/Popover.tsx); `.u-hit` (44px touch target) and the focus
// ring rules in src/index.css.
export { default as Button } from './Button';
export type { ButtonProps, ButtonSize, ButtonVariant } from './Button';
export { default as IconButton } from './IconButton';
export type { IconButtonProps, IconButtonSize } from './IconButton';
export { default as EmptyState } from './EmptyState';
export type { EmptyStateAction, EmptyStateProps } from './EmptyState';
export { default as Notice } from './Notice';
export type { NoticeProps, NoticeTone } from './Notice';
export { default as Spinner } from './Spinner';
export type { SpinnerProps } from './Spinner';
export { default as PageHeader } from './PageHeader';
export type { PageHeaderProps } from './PageHeader';
export { ToastHost } from './Toast';
export type { ToastHostProps } from './Toast';
export { NARROW_QUERY, useMediaQuery } from './useMediaQuery';
export { COLORS, GRAY, Z } from './tokens';
