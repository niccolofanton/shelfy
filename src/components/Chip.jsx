import React from 'react';
import { X } from 'lucide-react';
import { useT } from '../i18n';

/**
 * Reusable pill chip — identical styling to the inline Chip used in AiTags.
 *
 * Props:
 *  - label:    string shown inside the pill
 *  - count?:   optional numeric badge rendered to the right of the label
 *  - active?:  highlights the chip with the accent colour
 *  - tone?:    'violet' (default, tags/brand) | 'sky' (search keywords)
 *  - onClick?: when set, the label becomes a button
 *  - onRemove?: when set, renders a small X button (e.g. for active filters)
 *  - color?:   optional leading colour dot
 *  - className?: extra classes appended to the wrapper
 *  - 'data-testid'?: forwarded to the wrapper for testing hooks
 */

// Per-tone styling. The colour encodes the chip TYPE so tags (violet, the brand
// accent) and free-text keywords (sky) stay distinguishable whether active or not.
const TONE_CLASSES = {
  violet: {
    active: 'bg-[#7B5CFF] text-white',
    idle: 'bg-[#1a1a1a] text-gray-300 hover:bg-[#222]',
  },
  sky: {
    active: 'bg-[#38BDF8] text-[#06283b] font-medium',
    idle: 'bg-[#38BDF8]/10 text-[#7FD3F7] hover:bg-[#38BDF8]/20',
  },
};

export default function Chip({
  label,
  count,
  active,
  tone = 'violet',
  onClick,
  onRemove,
  color,
  className,
  style,
  'data-testid': dataTestId,
}) {
  const t = useT('chip');
  const toneClasses = TONE_CLASSES[tone] || TONE_CLASSES.violet;
  return (
    <span
      data-testid={dataTestId}
      style={style}
      className={[
        'inline-flex items-center gap-1 rounded-full pl-2.5 pr-2 py-1 text-xs transition-colors u-press',
        active ? toneClasses.active : toneClasses.idle,
        className || '',
      ].join(' ')}
    >
      {color && <span className="w-1.5 h-1.5 rounded-full" style={{ backgroundColor: color }} />}
      {onClick ? (
        <button type="button" onClick={onClick} className="leading-none">
          {label}
        </button>
      ) : (
        <span className="leading-none">{label}</span>
      )}
      {typeof count === 'number' && <span className="tabular-nums opacity-70">{count}</span>}
      {onRemove && (
        // The chip is controlled by the parent's list, so on remove it unmounts
        // immediately — there's no owned lifecycle here to play a `u-pop-out`
        // exit without changing the data flow. We keep the tactile `u-press`
        // feedback on the button instead (premium micro-interaction, no logic
        // change), and the X icon nudges to full opacity on hover.
        <button
          type="button"
          onClick={onRemove}
          className="ml-0.5 opacity-70 hover:opacity-100 u-press u-transition"
          title={t('remove')}
        >
          <X size={12} />
        </button>
      )}
    </span>
  );
}
