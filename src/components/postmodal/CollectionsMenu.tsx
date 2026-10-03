import { useRef } from 'react';
import { Check, FolderPlus, Plus } from 'lucide-react';
import { useT } from '../../i18n';
import Popover from '../Popover';
import IconButton from '../ui/IconButton';

interface CollectionsMenuProps {
  collections: Shelfy.Collection[];
  assignedIds: Set<number>;
  open: boolean;
  onToggle: () => void;
  onRequestClose: () => void;
  onAssign: (id: number) => void;
  onCreateNew: () => void;
}

// "Add to folder" header button + picker — single-post mirror of the gallery
// bulk action, using the shared portal Popover so it floats above the modal
// (anchored on desktop, a bottom sheet on narrow, O8) with focus, arrow-key
// menu navigation and Escape handled for us (MOD-6, MOD-8). Presentational: the
// shell owns the open flag (its keyboard handler must know a layer is open),
// the collection list and the membership set.
export default function CollectionsMenu({
  collections,
  assignedIds,
  open,
  onToggle,
  onRequestClose,
  onAssign,
  onCreateNew,
}: CollectionsMenuProps) {
  const t = useT('postModal');
  const assignRef = useRef<HTMLButtonElement | null>(null);

  const row =
    'u-press flex w-full items-center gap-2 px-3 py-1.5 text-sm text-gray-300 hover:bg-[#222] text-left';

  return (
    <>
      <IconButton
        ref={assignRef}
        data-testid="post-modal-assign-toggle"
        icon={FolderPlus}
        label={t('addToSource')}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={onToggle}
      />

      <Popover
        anchorRef={assignRef}
        open={open}
        align="right"
        presentation="auto"
        role="menu"
        aria-label={t('addToSource')}
        onRequestClose={onRequestClose}
        data-testid="post-modal-assign-popover"
        className="w-60 bg-[#1a1a1a] border border-[#2e2e2e] rounded-lg shadow-2xl py-1 u-fade-in-down origin-top-right"
        sheetClassName=""
      >
        <p className="px-3 py-1.5 text-2xs font-semibold uppercase tracking-widest text-muted">
          {t('addTo')}
        </p>
        <div className="max-h-64 overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e]">
          {collections.length === 0 && (
            <p className="px-3 py-2 text-xs text-muted">{t('noSources')}</p>
          )}
          {collections.map((c) => {
            const isIn = assignedIds.has(c.id);
            return (
              <button
                key={c.id}
                role="menuitem"
                data-testid={`post-modal-assign-to-${c.id}`}
                onClick={() => onAssign(c.id)}
                title={isIn ? t('removeFromSource') : undefined}
                className={row}
              >
                <span
                  className="w-2 h-2 rounded-full shrink-0"
                  style={{ backgroundColor: c.color }}
                />
                <span className="flex-1 truncate text-left">{c.name}</span>
                {isIn && <Check size={14} className="u-pop-in text-green-400 shrink-0" />}
              </button>
            );
          })}
        </div>
        <div className="border-t border-[#2e2e2e] mt-1 pt-1">
          <button
            role="menuitem"
            data-testid="post-modal-assign-create-new"
            onClick={onCreateNew}
            className={row}
          >
            <Plus size={14} className="shrink-0" />
            {t('createNewSource')}
          </button>
        </div>
      </Popover>
    </>
  );
}
