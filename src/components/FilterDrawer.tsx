import React from 'react';
import {
  X,
  RotateCcw,
  Film,
  HardDrive,
  Sparkles,
  Bookmark,
  Grid3X3,
  Instagram,
  Twitter,
  Globe,
} from 'lucide-react';
import PinterestIcon from './PinterestIcon';
import { useT } from '../i18n';

// Translator returned by useT — namespaced key + optional interpolation vars.
type Translate = (key: string, vars?: Record<string, string | number>) => string;

// Lucide-compatible icon component: the subset of props this drawer passes
// (size + className only). Covers both lucide-react icons and PinterestIcon.
type IconComponent = (props: { size?: number; className?: string }) => React.ReactNode;

// The slice of the Gallery filter state this drawer reads. The component is
// generic over the full filter object (F) so it hands the same shape back
// through onChange — extra Gallery-owned fields ride along untouched.
interface FilterDrawerFilters {
  mediaType?: string;
  downloadStatus?: string;
  aiTagged?: string;
}

// A library source selection routed back up to App (same path as the sidebar).
interface SourceSelection {
  type: 'platform' | 'collection';
  value: string | number;
  label?: string;
  color?: string;
}

// The currently-active source driving the highlight.
interface ActiveSource {
  type: 'platform' | 'collection';
  value: string | number;
}

interface SegmentedOption {
  value: string;
  label: string;
}

interface SegmentedProps {
  options: SegmentedOption[];
  value: string;
  onChange: (value: string) => void;
  cols: number;
}

interface SectionLabelProps {
  icon: IconComponent;
  children: React.ReactNode;
}

interface FilterDrawerProps<F extends FilterDrawerFilters> {
  open: boolean;
  onClose: () => void;
  filters: F;
  onChange: (filters: F) => void;
  collections?: Shelfy.Collection[];
  stats?: Shelfy.Stats | Record<string, never>;
  activeSource?: ActiveSource;
  onSelectSource?: (source: SourceSelection) => void;
}

function formatCount(n: number | null | undefined): string {
  if (n == null) return '0';
  return n.toLocaleString();
}

// Compact segmented control (same look as the old Filtri popover): one bordered
// track with equal-width cells laid out on a grid so long labels get their cell.
function Segmented({ options, value, onChange, cols }: SegmentedProps): React.JSX.Element {
  return (
    <div
      className="grid gap-1 p-1 rounded-lg bg-[#141414] border border-[#272727]"
      style={{ gridTemplateColumns: `repeat(${cols}, minmax(0, 1fr))` }}
    >
      {options.map((opt) => {
        const active = value === opt.value;
        return (
          <button
            key={opt.value}
            onClick={() => onChange(opt.value)}
            aria-pressed={active}
            className={[
              'u-press px-1.5 py-1.5 rounded-md text-[12.5px] font-medium text-center leading-tight transition-colors',
              active
                ? 'bg-[#7B5CFF] text-white shadow-sm u-pop-in'
                : 'text-gray-400 hover:text-gray-100 hover:bg-[#202020]',
            ].join(' ')}
          >
            {opt.label}
          </button>
        );
      })}
    </div>
  );
}

function SectionLabel({ icon: Icon, children }: SectionLabelProps): React.JSX.Element {
  return (
    <div className="flex items-center gap-1.5 mb-2">
      <Icon size={12} className="text-gray-500 shrink-0" />
      <span className="text-[10px] font-semibold uppercase tracking-wider text-gray-500">
        {children}
      </span>
    </div>
  );
}

// One source/subfolder row's config (consumed by sourceRow below).
interface SourceRowOptions {
  active: boolean;
  onClick: () => void;
  icon?: React.ReactNode;
  dot?: string;
  label: string;
  count: number;
  nested?: boolean;
}

// Right-hand filters drawer that lives inside the Gallery page. Top section is a
// mirror of the sidebar Bookmarks list (sources/subfolders); selecting a row
// routes through onSelectSource (same path as the sidebar) so the app's active
// source, the sidebar highlight and this drawer all stay in sync — the drawer
// stays open and the chosen source stays highlighted. Below it are the media /
// download / AI-tag filters that used to live in the Filtri popover.
export default function FilterDrawer<F extends FilterDrawerFilters>({
  open,
  onClose,
  filters,
  onChange: onFiltersChange,
  collections = [],
  stats = {},
  activeSource,
  onSelectSource,
}: FilterDrawerProps<F>): React.JSX.Element | null {
  const t: Translate = useT('filterDrawer');
  if (!open) return null;

  const MEDIA_TYPE_OPTIONS: SegmentedOption[] = [
    { value: 'all', label: t('mediaAll') },
    { value: 'video', label: t('mediaVideo') },
    { value: 'image', label: t('mediaImage') },
    { value: 'carousel', label: t('mediaCarousel') },
  ];

  const DOWNLOAD_OPTIONS: SegmentedOption[] = [
    { value: 'all', label: t('downloadAll') },
    { value: 'downloaded', label: t('downloadDownloaded') },
    { value: 'linkonly', label: t('downloadLinkOnly') },
  ];

  const AI_TAGS_OPTIONS: SegmentedOption[] = [
    { value: 'all', label: t('aiAll') },
    { value: 'tagged', label: t('aiTagged') },
    { value: 'untagged', label: t('aiUntagged') },
  ];

  // Platform rows mirrored from the sidebar Bookmarks section (same order/icons).
  const PLATFORM_ROWS: { id: string; label: string; Icon: IconComponent }[] = [
    { id: 'instagram', label: 'Instagram', Icon: Instagram },
    { id: 'twitter', label: 'X / Twitter', Icon: Twitter },
    { id: 'pinterest', label: 'Pinterest', Icon: PinterestIcon },
    { id: 'web', label: t('web'), Icon: Globe },
  ];

  // `stats` is the full Shelfy.Stats once loaded, or {} before the first fetch.
  const statsValue: { total?: number; byPlatform?: Partial<Record<string, number>> } = stats;
  const total = statsValue.total ?? 0;
  const byPlatform: Partial<Record<string, number>> = statsValue.byPlatform ?? {};

  const mediaType = filters.mediaType ?? 'all';
  const downloadStatus = filters.downloadStatus ?? 'all';
  const aiTagged = filters.aiTagged ?? 'all';
  const activeCount =
    (mediaType !== 'all' ? 1 : 0) +
    (downloadStatus !== 'all' ? 1 : 0) +
    (aiTagged !== 'all' ? 1 : 0);

  const isActive = (type: ActiveSource['type'], value: string | number): boolean =>
    activeSource?.type === type && activeSource?.value === value;

  const customCollections = collections.filter((c) => !c.platform);

  // One source/subfolder row. `nested` indents it (collections under a platform).
  const sourceRow = (
    key: string,
    { active, onClick, icon, dot, label, count, nested }: SourceRowOptions,
  ): React.JSX.Element => (
    <button
      key={key}
      data-testid={`drawer-source-${key}`}
      aria-current={active ? 'page' : undefined}
      onClick={onClick}
      className={[
        'u-press relative w-full flex items-center gap-2.5 pr-2 py-1.5 text-sm rounded-md cursor-pointer transition-colors',
        nested ? 'pl-9' : 'pl-3',
        active ? 'bg-[#1e1e1e] text-white' : 'text-gray-400 hover:bg-[#1a1a1a] hover:text-gray-200',
      ].join(' ')}
    >
      {active && (
        <span
          aria-hidden
          className="u-bar-in absolute left-0 inset-y-0 my-auto h-4 w-[3px] rounded-r-full"
          style={{ backgroundColor: dot || '#7B5CFF' }}
        />
      )}
      {dot ? (
        <span
          className="w-2 h-2 rounded-full shrink-0"
          style={{ backgroundColor: dot, boxShadow: active ? `0 0 0 3px ${dot}33` : 'none' }}
        />
      ) : (
        icon
      )}
      <span className="flex-1 truncate text-left">{label}</span>
      <span className="text-[11px] text-gray-500 tabular-nums shrink-0">{formatCount(count)}</span>
    </button>
  );

  return (
    <aside
      data-testid="filter-drawer"
      className="u-fade-in-right shrink-0 w-[280px] h-full bg-[#111111] border-l border-[#2e2e2e] flex flex-col overflow-hidden"
    >
      {/* Header */}
      <div className="flex items-center justify-between px-4 h-[52px] shrink-0 border-b border-[#2e2e2e]">
        <span className="text-[13px] font-semibold text-gray-200">{t('title')}</span>
        <div className="flex items-center gap-1">
          {activeCount > 0 && (
            <button
              data-testid="drawer-reset"
              onClick={() =>
                onFiltersChange({
                  ...filters,
                  mediaType: 'all',
                  downloadStatus: 'all',
                  aiTagged: 'all',
                })
              }
              className="u-press flex items-center gap-1 text-[11px] text-gray-500 hover:text-gray-200 transition-colors"
            >
              <RotateCcw size={12} />
              {t('reset')}
            </button>
          )}
          <button
            data-testid="drawer-close"
            onClick={onClose}
            title={t('closeTitle')}
            className="u-press flex items-center justify-center w-6 h-6 rounded text-gray-500 hover:text-white hover:bg-[#1e1e1e] transition-colors"
          >
            <X size={15} />
          </button>
        </div>
      </div>

      <div className="flex-1 min-h-0 overflow-y-auto scrollbar-thin scrollbar-thumb-[#2e2e2e] scrollbar-track-transparent p-3 space-y-4">
        {/* Sources — mirror of the sidebar Bookmarks list */}
        <div>
          <SectionLabel icon={Bookmark}>{t('bookmarks')}</SectionLabel>
          <div className="flex flex-col gap-0.5">
            {sourceRow('all', {
              active: isActive('platform', 'all'),
              onClick: () => onSelectSource?.({ type: 'platform', value: 'all' }),
              icon: <Grid3X3 size={15} className="shrink-0 text-gray-400" />,
              label: t('allPosts'),
              count: total,
            })}

            {PLATFORM_ROWS.map(({ id, label, Icon }) => {
              const children = collections.filter((c) => c.platform === id);
              return (
                <React.Fragment key={id}>
                  {sourceRow(id, {
                    active: isActive('platform', id),
                    onClick: () => onSelectSource?.({ type: 'platform', value: id }),
                    icon: <Icon size={15} className="shrink-0" />,
                    label,
                    count: byPlatform[id] ?? 0,
                  })}
                  {children.map((c) =>
                    sourceRow(`c${c.id}`, {
                      active: isActive('collection', c.id),
                      onClick: () =>
                        onSelectSource?.({
                          type: 'collection',
                          value: c.id,
                          label: c.name,
                          color: c.color,
                        }),
                      dot: c.color,
                      label: c.name,
                      count: c.count ?? 0,
                      nested: true,
                    }),
                  )}
                </React.Fragment>
              );
            })}

            {customCollections.length > 0 && (
              <div className="mt-1.5 flex flex-col gap-0.5">
                {customCollections.map((c) =>
                  sourceRow(`c${c.id}`, {
                    active: isActive('collection', c.id),
                    onClick: () =>
                      onSelectSource?.({
                        type: 'collection',
                        value: c.id,
                        label: c.name,
                        color: c.color,
                      }),
                    dot: c.color,
                    label: c.name,
                    count: c.count ?? 0,
                  }),
                )}
              </div>
            )}
          </div>
        </div>

        {/* Media / download / AI-tag filters (moved from the Filtri popover) */}
        <div data-testid="drawer-mediatype">
          <SectionLabel icon={Film}>{t('mediaType')}</SectionLabel>
          <Segmented
            cols={2}
            options={MEDIA_TYPE_OPTIONS}
            value={mediaType}
            onChange={(val) => onFiltersChange({ ...filters, mediaType: val })}
          />
        </div>

        <div data-testid="drawer-download">
          <SectionLabel icon={HardDrive}>{t('downloadStatus')}</SectionLabel>
          <Segmented
            cols={3}
            options={DOWNLOAD_OPTIONS}
            value={downloadStatus}
            onChange={(val) => onFiltersChange({ ...filters, downloadStatus: val })}
          />
        </div>

        <div data-testid="drawer-aitags">
          <SectionLabel icon={Sparkles}>{t('aiTags')}</SectionLabel>
          <Segmented
            cols={3}
            options={AI_TAGS_OPTIONS}
            value={aiTagged}
            onChange={(val) => onFiltersChange({ ...filters, aiTagged: val })}
          />
        </div>
      </div>
    </aside>
  );
}
