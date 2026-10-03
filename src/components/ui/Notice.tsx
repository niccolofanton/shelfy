// <Notice> — a one-line message under a form or a card (UX audit §3.6; the
// shared version of Settings' InlineNote). Icon plus color, never color alone.
//
//   tone:       'info' | 'ok' | 'warn' | 'error'. 'error' is `role="alert"`
//               (announced at once); the others are `role="status"`.
//   children:   the message
//   testId?, className? (spacing; it has no outer margin of its own)
import React from 'react';
import { AlertTriangle, Check, Info, OctagonAlert } from 'lucide-react';

export type NoticeTone = 'info' | 'ok' | 'warn' | 'error';

export interface NoticeProps {
  tone: NoticeTone;
  children: React.ReactNode;
  testId?: string;
  className?: string;
}

const TONES = {
  info: { Icon: Info, color: 'text-secondary' },
  ok: { Icon: Check, color: 'text-success' },
  warn: { Icon: AlertTriangle, color: 'text-warning' },
  error: { Icon: OctagonAlert, color: 'text-error' },
} as const;

export default function Notice({
  tone,
  children,
  testId,
  className = '',
}: NoticeProps): React.JSX.Element {
  const { Icon, color } = TONES[tone];
  return (
    <p
      data-testid={testId}
      role={tone === 'error' ? 'alert' : 'status'}
      className={`u-fade-in flex items-start gap-1.5 text-xs leading-relaxed ${color} ${className}`}
    >
      <Icon size={13} aria-hidden="true" className="mt-px shrink-0" />
      <span>{children}</span>
    </p>
  );
}
