import { useRef } from 'react';
import { Check, FolderPlus, Plus } from 'lucide-react';
import { useT } from '../../i18n';
import Popover from '../Popover';

interface CollectionsMenuProps {
  collections: Shelfy.Collection[];
  assignedIds: Set<number>;
  open: boolean;
  onToggle: () => void;
  onRequestClose: () => void;
  onAssign: (id: number) => void;
  onCreateNew: () => void;
}

// "Add to a source" header button + picker popover — single-post mirror of the
// gallery bulk action, using the shared portal Popover so it floats above the
// modal. Presentational: the shell owns the open flag (its keyboard handler
// must know a layer is open), the collection list and the membership set.
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
  const assignRef = useRef<HTMLDivElement | null>(null);

  return (
    <div ref={assignRef} className="relative">
      <button
        data-testid="post-modal-assign-toggle"
        onClick={onToggle}
        aria-haspopup="menu"
        aria-expanded={open}
        title={t('addToSource')}
        className="u-press flex items-center justify-center w-8 h-8 rounded-md text-[#a0a0a0] hover:text-white hover:bg-[#2a2a2a]"
      >
        <FolderPlus size={16} />
      </button>

      <Popover
        anchorRef={assignRef}
        open={open}
        align="right"
        onRequestClose={onRequestClose}
        data-testid="post-modal-assign-popover"
        className="w-60 bg-[#1a1a1a] border border-[#2e2e2e] rounded-lg shadow-2xl py-1 u-fade-in-down origin-top-right"
      >
        <p className="px-3 py-1.5 text-[10px] font-semibold uppercase tracking-widest text-gray-600">
          {t('addTo')}
        </p>
        <div className="max-h-64 overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e]">
          {collections.length === 0 && (
            <p className="px-3 py-2 text-xs text-gray-500">{t('noSources')}</p>
          )}
          {collections.map((c) => {
            const isIn = assignedIds.has(c.id);
            return (
              <button
                key={c.id}
                data-testid={`post-modal-assign-to-${c.id}`}
                onClick={() => onAssign(c.id)}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-sm text-gray-300 hover:bg-[#222] u-press"
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
            data-testid="post-modal-assign-create-new"
            onClick={onCreateNew}
            className="w-full flex items-center gap-2 px-3 py-1.5 text-sm text-gray-300 hover:bg-[#222] u-press"
          >
            <Plus size={14} className="shrink-0" />
            {t('createNewSource')}
          </button>
        </div>
      </Popover>
    </div>
  );
}
