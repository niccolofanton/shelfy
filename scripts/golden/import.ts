// Synthetic exports through the actual desktop IG/X import normalizers.
import { normalizeExportedPost as ig } from '../../electron/ig-parser';
import { normalizeExportedPost as x } from '../../electron/tw-parser';
import type { GoldenSet } from './lib';
const cases = [
  {
    id: 'extension-caption',
    value: { shortcode: 'CxKwJ0fLmQZ', caption: 'Synthetic caption', authorUsername: 'someone' },
  },
  {
    id: 'desktop-fields-ai',
    value: {
      id: '3191575067010950169_1',
      shortcode: 'CxKwJ0fLmQZ',
      text: 'Desktop text',
      mediaType: 'video',
      thumbnailUrl: 'https://pbs.twimg.com/media/synthetic.jpg',
      aiDescription: 'Synthetic analysis',
      aiTags: ['glass'],
      aiGeneralTags: [],
      aiSpecificTags: ['lamp'],
      aiEntities: ['Object'],
      aiKeywords: ['light'],
      aiCategory: 'design',
      aiContentType: 'photo',
      aiLanguage: 'it',
      aiSaveReason: 'Idea',
      aiStatus: 'done',
      aiModel: 'fixture',
      aiAnalyzedAt: 1700000000000,
    },
  },
  { id: 'empty-fields', value: { id: '1700000000000000001', text: '', authorUsername: '' } },
  {
    id: 'media-list',
    value: {
      id: '1700000000000000002',
      text: 'Synthetic X',
      authorUsername: 'someone',
      mediaType: 'images',
      media: [
        { url: 'https://pbs.twimg.com/media/a.jpg' },
        { type: 'video', thumbnailUrl: 'https://pbs.twimg.com/media/b.jpg' },
      ],
    },
  },
  {
    id: 'invalid-ai-types',
    value: { id: '1700000000000000003', aiDescription: null, aiTags: 'wrong', aiStatus: 7 },
  },
];
export default [
  {
    name: 'import/instagram',
    source: 'electron/ig-parser.ts#normalizeExportedPost',
    generator: 'scripts/golden/import.ts',
    build: () => cases.map(({ id, value }) => ({ id, args: [value], output: ig(value) })),
  },
  {
    name: 'import/twitter',
    source: 'electron/tw-parser.ts#normalizeExportedPost',
    generator: 'scripts/golden/import.ts',
    build: () => cases.map(({ id, value }) => ({ id, args: [value], output: x(value) })),
  },
] satisfies GoldenSet[];
