import React, { useEffect, useRef, useState } from 'react';
import { Check, Palette, X } from 'lucide-react';
import { useT } from '../../i18n';
import Popover from '../../components/Popover';
import { inkOn, normHex } from './model';

// Colour search: pick a swatch or type a hex → sites whose background, surface
// or accent colour is perceptually close (queryWebReferences `color`).

const PRESETS = [
  '#000000',
  '#1c1c1c',
  '#6b6b6b',
  '#f5f5f0',
  '#ffffff',
  '#e5484d',
  '#f76b15',
  '#ffc53d',
  '#a8d61f',
  '#30a46c',
  '#12a594',
  '#05a2c2',
  '#0090ff',
  '#3e63dd',
  '#6e56cf',
  '#ab4aba',
  '#d6409f',
  '#a18072',
  '#e8d8c3',
  '#1b2a4a',
];

interface ColorFilterProps {
  value: string | null;
  onChange: (hex: string | null) => void;
}

export default function ColorFilter({ value, onChange }: ColorFilterProps): React.ReactElement {
  const t = useT('aiWebsites');
  const anchor = useRef<HTMLButtonElement | null>(null);
  const [open, setOpen] = useState(false);
  const [draft, setDraft] = useState(value || '');
  useEffect(() => setDraft(value || ''), [value]);
  const parsed = normHex(draft);

  const apply = (hex: string | null): void => {
    onChange(hex);
    setOpen(false);
  };

  return (
    <>
      <button
        ref={anchor}
        type="button"
        data-testid="aiweb-color"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        title={t('colorTitle')}
        className={`flex items-center gap-1.5 h-8 pl-2 pr-2.5 rounded-lg text-[12.5px] u-press border ${
          value
            ? 'border-[#7B5CFF]/60 bg-[#7B5CFF]/10 text-white'
            : 'border-[#2a2a2a] bg-[#171717] text-[#bdbdbd] hover:text-white hover:bg-[#1f1f1f]'
        }`}
      >
        {value ? (
          <span
            className="w-4 h-4 rounded-full"
            style={{ background: value, boxShadow: 'inset 0 0 0 1px rgba(255,255,255,0.2)' }}
          />
        ) : (
          <Palette size={14} />
        )}
        <span className="tabular-nums">{value ? value.toUpperCase() : t('colorButton')}</span>
        {value && (
          <span
            role="button"
            tabIndex={0}
            aria-label={t('colorClear')}
            onClick={(e) => {
              e.stopPropagation();
              onChange(null);
            }}
            onKeyDown={(e) => {
              if (e.key === 'Enter') {
                e.stopPropagation();
                onChange(null);
              }
            }}
            className="ml-0.5 -mr-1 flex items-center justify-center w-4 h-4 rounded hover:bg-white/10"
          >
            <X size={12} />
          </span>
        )}
      </button>
      <Popover
        anchorRef={anchor}
        open={open}
        onRequestClose={() => setOpen(false)}
        gap={6}
        className="z-[120] w-[236px] rounded-xl border border-[#2e2e2e] bg-[#1a1a1a] p-3 shadow-2xl"
      >
        <div className="text-[11px] font-semibold uppercase tracking-[0.12em] text-[#8a8a8a] mb-2">
          {t('colorPopoverTitle')}
        </div>
        <div className="grid grid-cols-5 gap-1.5" data-testid="aiweb-color-presets">
          {PRESETS.map((hex) => (
            <button
              key={hex}
              type="button"
              onClick={() => apply(hex)}
              title={hex.toUpperCase()}
              aria-label={hex}
              className="relative h-8 rounded-md u-press flex items-center justify-center"
              style={{ background: hex, boxShadow: 'inset 0 0 0 1px rgba(255,255,255,0.1)' }}
            >
              {value === hex && <Check size={14} color={inkOn(hex)} strokeWidth={3} />}
            </button>
          ))}
        </div>
        <form
          className="mt-3 flex items-center gap-1.5"
          onSubmit={(e) => {
            e.preventDefault();
            if (parsed) apply(parsed);
          }}
        >
          <label
            className="relative shrink-0 w-8 h-8 rounded-md overflow-hidden cursor-pointer"
            style={{
              background: parsed || '#2a2a2a',
              boxShadow: 'inset 0 0 0 1px rgba(255,255,255,0.12)',
            }}
            title={t('colorPickerTitle')}
          >
            <input
              type="color"
              value={parsed || '#7b5cff'}
              onChange={(e) => setDraft(e.target.value)}
              className="absolute inset-0 opacity-0 cursor-pointer"
            />
          </label>
          <input
            data-testid="aiweb-color-hex"
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            placeholder="#7B5CFF"
            spellCheck={false}
            className="flex-1 min-w-0 h-8 rounded-md bg-[#121212] border border-[#2e2e2e] px-2 text-[12.5px] tabular-nums text-white outline-none focus:border-[#7B5CFF]"
          />
          <button
            type="submit"
            disabled={!parsed}
            className="h-8 px-2.5 rounded-md bg-[#7B5CFF] text-white text-[12px] font-medium u-press disabled:opacity-40 hover:bg-[#6a4cf0]"
          >
            {t('colorApply')}
          </button>
        </form>
      </Popover>
    </>
  );
}
